use std::fs::File;
use std::io::{self, BufRead, BufReader};

use phoenix_engine::execution::ExecutionMode;
use phoenix_replay::ReplayConfig;

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let fixture = match args.next().as_deref() {
        Some("--fixture") => args
            .next()
            .unwrap_or_else(|| "fixtures/feed/profitable.ndjson".to_string()),
        _ => "fixtures/feed/profitable.ndjson".to_string(),
    };
    let config = ReplayConfig {
        fixture: fixture.clone(),
        execution_mode: ExecutionMode::Shadow,
    };
    let file = File::open(&config.fixture)?;
    let reader = BufReader::new(file);
    let mut count = 0usize;
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            count += 1;
        }
    }
    println!(
        "replayed_events={} mode={}",
        count,
        config.execution_mode.as_str()
    );
    Ok(())
}
