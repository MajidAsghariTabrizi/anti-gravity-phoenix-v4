use std::fs::File;
use std::io::{self, BufRead, Write};

fn main() -> io::Result<()> {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/feed/recorded.ndjson".to_string());
    let mut file = File::create(output)?;
    for line in io::stdin().lock().lines() {
        writeln!(file, "{}", line?)?;
    }
    Ok(())
}

