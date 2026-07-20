use phoenix_live_executor::approval::{ApprovalInput, ApprovalMaterializer, APPROVAL_CONFIRMATION};
use std::collections::BTreeMap;

#[tokio::main]
async fn main() {
    match run().await {
        Ok(output) => match serde_json::to_string(&output) {
            Ok(encoded) => println!("{encoded}"),
            Err(_) => {
                eprintln!("LIVE_CANARY_APPROVAL_ERROR: code=output_encoding");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("LIVE_CANARY_APPROVAL_ERROR: code={}", error.code());
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<phoenix_live_executor::approval::ApprovalOutcome, CliError> {
    let arguments = parse_arguments(std::env::args().skip(1))?;
    if required(&arguments, "--confirm")? != APPROVAL_CONFIRMATION {
        return Err(CliError::Confirmation);
    }
    let input = ApprovalInput {
        simulation_result_hash: required(&arguments, "--result-hash")?.to_string(),
        approved_by: required(&arguments, "--approved-by")?.to_string(),
        approval_ttl_seconds: required(&arguments, "--approval-ttl-seconds")?
            .parse()
            .map_err(|_| CliError::Arguments)?,
        max_priority_fee_per_gas: required(&arguments, "--max-priority-fee-per-gas-wei")?
            .parse()
            .map_err(|_| CliError::Arguments)?,
    };
    let dsn = std::env::var("POSTGRES_DSN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(CliError::Configuration)?;
    let materializer = ApprovalMaterializer::connect(&dsn)
        .await
        .map_err(CliError::Approval)?;
    materializer
        .materialize(input, chrono::Utc::now())
        .await
        .map_err(CliError::Approval)
}

fn parse_arguments(
    arguments: impl Iterator<Item = String>,
) -> Result<BTreeMap<String, String>, CliError> {
    let mut parsed = BTreeMap::new();
    let mut pending = None;
    for argument in arguments {
        if let Some(name) = pending.take() {
            if argument.starts_with("--") || parsed.insert(name, argument).is_some() {
                return Err(CliError::Arguments);
            }
        } else if matches!(
            argument.as_str(),
            "--result-hash"
                | "--approved-by"
                | "--approval-ttl-seconds"
                | "--max-priority-fee-per-gas-wei"
                | "--confirm"
        ) {
            pending = Some(argument);
        } else {
            return Err(CliError::Arguments);
        }
    }
    if pending.is_some() || parsed.len() != 5 {
        return Err(CliError::Arguments);
    }
    Ok(parsed)
}

fn required<'a>(arguments: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, CliError> {
    arguments
        .get(name)
        .map(String::as_str)
        .ok_or(CliError::Arguments)
}

#[derive(Debug)]
enum CliError {
    Arguments,
    Confirmation,
    Configuration,
    Approval(phoenix_live_executor::approval::ApprovalError),
}

impl CliError {
    const fn code(&self) -> &'static str {
        match self {
            Self::Arguments => "invalid_arguments",
            Self::Confirmation => "confirmation_mismatch",
            Self::Configuration => "missing_database_configuration",
            Self::Approval(error) => error.code(),
        }
    }
}
