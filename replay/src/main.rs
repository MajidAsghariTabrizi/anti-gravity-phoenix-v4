use std::fs;
use std::io;

use phoenix_replay::{replay, ReplayConfig};

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let fixture = match args.next().as_deref() {
        Some("--fixture") => args
            .next()
            .unwrap_or_else(|| "fixtures/replay/shadow_cases.ndjson".to_string()),
        _ => "fixtures/replay/shadow_cases.ndjson".to_string(),
    };
    let config = ReplayConfig {
        fixture: fixture.clone(),
        code_version: std::env::var("PHOENIX_CODE_VERSION")
            .unwrap_or_else(|_| "unversioned-local".to_string()),
        config_version: std::env::var("PHOENIX_CONFIG_VERSION")
            .unwrap_or_else(|_| "unversioned-local".to_string()),
    };
    let input = fs::read_to_string(&config.fixture)?;
    let report = replay(&input).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("deterministic replay failed: {error:?}"),
        )
    })?;
    print!(
        "{}",
        report.render(&config.code_version, &config.config_version)
    );
    Ok(())
}
