use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::TryStreamExt;
use phoenix_engine::positive_route_evidence::{
    analyze_stored_transaction, DiscoveryStatistics, StoredTransactionEvidence,
    TransactionProvenance, POSTGRES_FEED_EVENT_SOURCE,
};
use phoenix_engine::shadow_processor::RouteRegistry;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

const DEFAULT_SCAN_LIMIT: usize = 10_000;
const MAX_SCAN_LIMIT: usize = 100_000;
const MAX_JSONL_LINE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    ScanPostgres,
    ReplayJsonl,
}

#[derive(Clone, Debug)]
struct Options {
    command: Command,
    dsn_env: String,
    route_registry_env: String,
    route_registry_file: Option<PathBuf>,
    input_jsonl: Option<PathBuf>,
    export_jsonl: Option<PathBuf>,
    tx_hash: Option<String>,
    source_sequence: Option<String>,
    limit: usize,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("arguments are invalid")]
    Arguments,
    #[error("route registry configuration is invalid")]
    RouteRegistry,
    #[error("PostgreSQL configuration is unavailable")]
    DatabaseConfiguration,
    #[error("read-only PostgreSQL discovery is unavailable")]
    DatabaseUnavailable,
    #[error("stored transaction evidence query failed")]
    DatabaseQuery,
    #[error("JSONL evidence input is invalid")]
    InputEvidence,
    #[error("JSONL evidence output could not be created")]
    OutputEvidence,
    #[error("production decoder replay failed")]
    Analysis,
    #[error("scan limit is smaller than the eligible evidence set")]
    ScanLimit,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("SHADOW_POSITIVE_ROUTE_EVIDENCE_ERROR: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), CliError> {
    let options = parse_options()?;
    let routes = load_route_registry(&options)?;
    match options.command {
        Command::ScanPostgres => scan_postgres(&options, &routes).await,
        Command::ReplayJsonl => replay_jsonl(&options, &routes),
    }
}

fn parse_options() -> Result<Options, CliError> {
    let mut arguments = env::args().skip(1);
    let command = match arguments.next().as_deref() {
        Some("scan-postgres") => Command::ScanPostgres,
        Some("replay-jsonl") => Command::ReplayJsonl,
        _ => return Err(CliError::Arguments),
    };
    let mut options = Options {
        command,
        dsn_env: "POSTGRES_DSN".to_string(),
        route_registry_env: "ENGINE_ROUTE_REGISTRY_JSON".to_string(),
        route_registry_file: None,
        input_jsonl: None,
        export_jsonl: None,
        tx_hash: None,
        source_sequence: None,
        limit: DEFAULT_SCAN_LIMIT,
    };
    while let Some(flag) = arguments.next() {
        let value = arguments.next().ok_or(CliError::Arguments)?;
        match flag.as_str() {
            "--dsn-env" => options.dsn_env = bounded_environment_name(&value)?,
            "--route-registry-env" => {
                options.route_registry_env = bounded_environment_name(&value)?;
            }
            "--route-registry-file" => {
                set_once(&mut options.route_registry_file, PathBuf::from(value))?;
            }
            "--input" => set_once(&mut options.input_jsonl, PathBuf::from(value))?,
            "--export-jsonl" => set_once(&mut options.export_jsonl, PathBuf::from(value))?,
            "--tx-hash" => {
                if options.tx_hash.is_some() || !canonical_transaction_hash(&value) {
                    return Err(CliError::Arguments);
                }
                options.tx_hash = Some(value);
            }
            "--source-sequence" => {
                if options.source_sequence.is_some() || !canonical_source_sequence(&value) {
                    return Err(CliError::Arguments);
                }
                options.source_sequence = Some(value);
            }
            "--limit" => {
                options.limit = value.parse().map_err(|_| CliError::Arguments)?;
                if options.limit == 0 || options.limit > MAX_SCAN_LIMIT {
                    return Err(CliError::Arguments);
                }
            }
            _ => return Err(CliError::Arguments),
        }
    }
    match command {
        Command::ScanPostgres if options.input_jsonl.is_some() => Err(CliError::Arguments),
        Command::ReplayJsonl
            if options.input_jsonl.is_none()
                || options.export_jsonl.is_some()
                || options.tx_hash.is_some()
                || options.source_sequence.is_some() =>
        {
            Err(CliError::Arguments)
        }
        _ => Ok(options),
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError::Arguments);
    }
    *slot = Some(value);
    Ok(())
}

fn bounded_environment_name(value: &str) -> Result<String, CliError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(CliError::Arguments);
    }
    Ok(value.to_string())
}

fn load_route_registry(options: &Options) -> Result<RouteRegistry, CliError> {
    let raw = if let Some(path) = options.route_registry_file.as_deref() {
        std::fs::read_to_string(path).map_err(|_| CliError::RouteRegistry)?
    } else {
        env::var(&options.route_registry_env).map_err(|_| CliError::RouteRegistry)?
    };
    RouteRegistry::from_json(&raw).map_err(|_| CliError::RouteRegistry)
}

async fn scan_postgres(options: &Options, routes: &RouteRegistry) -> Result<(), CliError> {
    let dsn = env::var(&options.dsn_env).map_err(|_| CliError::DatabaseConfiguration)?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&dsn)
        .await
        .map_err(|_| CliError::DatabaseUnavailable)?;
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| CliError::DatabaseUnavailable)?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(|_| CliError::DatabaseQuery)?;

    let eligible_count: i64 = sqlx::query_scalar(
        r#"
SELECT count(*)
FROM (
    SELECT 1
    FROM feed_events AS feed
    WHERE lower(feed.payload->>'to') IN ($1, $2, $3)
      AND ($4::text IS NULL OR feed.tx_hash = $4)
      AND ($5::numeric IS NULL OR feed.sequence_number = $5::numeric)
    ORDER BY feed.id
    LIMIT $6
) AS bounded_eligible
"#,
    )
    .bind("0xe592427a0aece92de3edee1f18e0157c05861564")
    .bind("0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45")
    .bind("0xa51afafe0263b40edaef0df8781ea9aa03e381a3")
    .bind(options.tx_hash.as_deref())
    .bind(options.source_sequence.as_deref())
    .bind(options.limit as i64 + 1)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| CliError::DatabaseQuery)?;
    if eligible_count < 0 || eligible_count as usize > options.limit {
        return Err(CliError::ScanLimit);
    }

    let mut export = options
        .export_jsonl
        .as_deref()
        .map(create_private_output)
        .transpose()?
        .map(BufWriter::new);
    let mut statistics = DiscoveryStatistics::default();
    let mut rows = sqlx::query(
        r#"
SELECT feed.id,
       feed.payload,
       feed.recorded_at,
       origin.metadata->>'block_number' AS source_block_number,
       origin.metadata->>'block_hash' AS source_block_hash
FROM feed_events AS feed
LEFT JOIN origin_transactions AS origin ON origin.tx_hash = feed.tx_hash
WHERE lower(feed.payload->>'to') IN ($1, $2, $3)
  AND ($4::text IS NULL OR feed.tx_hash = $4)
  AND ($5::numeric IS NULL OR feed.sequence_number = $5::numeric)
ORDER BY feed.id
LIMIT $6
"#,
    )
    .bind("0xe592427a0aece92de3edee1f18e0157c05861564")
    .bind("0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45")
    .bind("0xa51afafe0263b40edaef0df8781ea9aa03e381a3")
    .bind(options.tx_hash.as_deref())
    .bind(options.source_sequence.as_deref())
    .bind(options.limit as i64)
    .fetch(&mut *transaction);

    while let Some(row) = rows.try_next().await.map_err(|_| CliError::DatabaseQuery)? {
        let feed_event_id: i64 = row.try_get("id").map_err(|_| CliError::DatabaseQuery)?;
        let payload: Value = row
            .try_get("payload")
            .map_err(|_| CliError::DatabaseQuery)?;
        let recorded_at: DateTime<Utc> = row
            .try_get("recorded_at")
            .map_err(|_| CliError::DatabaseQuery)?;
        let block_number = row
            .try_get::<Option<String>, _>("source_block_number")
            .map_err(|_| CliError::DatabaseQuery)?
            .and_then(|value| value.parse().ok());
        let block_hash = row
            .try_get::<Option<String>, _>("source_block_hash")
            .map_err(|_| CliError::DatabaseQuery)?;
        let stored = StoredTransactionEvidence {
            provenance: TransactionProvenance {
                source: POSTGRES_FEED_EVENT_SOURCE.to_string(),
                feed_event_id,
                recorded_at: recorded_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
                source_block_number: block_number,
                source_block_hash: block_hash,
            },
            payload,
        };
        if let Some(writer) = export.as_mut() {
            serde_json::to_writer(&mut *writer, &stored).map_err(|_| CliError::OutputEvidence)?;
            writer
                .write_all(b"\n")
                .map_err(|_| CliError::OutputEvidence)?;
        }
        analyze_and_report(&stored, routes, &mut statistics)?;
    }
    drop(rows);
    if let Some(writer) = export.as_mut() {
        writer.flush().map_err(|_| CliError::OutputEvidence)?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| CliError::DatabaseQuery)?;
    report_statistics(&statistics)
}

fn replay_jsonl(options: &Options, routes: &RouteRegistry) -> Result<(), CliError> {
    let input = options.input_jsonl.as_deref().ok_or(CliError::Arguments)?;
    let file = File::open(input).map_err(|_| CliError::InputEvidence)?;
    let mut reader = BufReader::new(file);
    let mut statistics = DiscoveryStatistics::default();
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|_| CliError::InputEvidence)?;
        if bytes == 0 {
            break;
        }
        if bytes > MAX_JSONL_LINE_BYTES {
            return Err(CliError::InputEvidence);
        }
        if line.trim().is_empty() {
            continue;
        }
        if statistics.inspected_transactions as usize >= options.limit {
            return Err(CliError::ScanLimit);
        }
        let stored: StoredTransactionEvidence =
            serde_json::from_str(&line).map_err(|_| CliError::InputEvidence)?;
        analyze_and_report(&stored, routes, &mut statistics)?;
    }
    report_statistics(&statistics)
}

fn analyze_and_report(
    stored: &StoredTransactionEvidence,
    routes: &RouteRegistry,
    statistics: &mut DiscoveryStatistics,
) -> Result<(), CliError> {
    let summary = analyze_stored_transaction(stored, routes).map_err(|_| CliError::Analysis)?;
    statistics.observe(&summary);
    println!(
        "{}",
        serde_json::to_string(&summary).map_err(|_| CliError::Analysis)?
    );
    Ok(())
}

fn report_statistics(statistics: &DiscoveryStatistics) -> Result<(), CliError> {
    println!(
        "DISCOVERY_STATISTICS {}",
        serde_json::to_string(statistics).map_err(|_| CliError::Analysis)?
    );
    println!("{}", statistics.terminal_result());
    Ok(())
}

fn create_private_output(path: &Path) -> Result<File, CliError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|_| CliError::OutputEvidence)
}

fn canonical_transaction_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_source_sequence(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}
