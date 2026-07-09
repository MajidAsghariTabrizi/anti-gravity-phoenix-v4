use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::net::TcpListener;

fn main() -> io::Result<()> {
    if std::env::var("RECORDER_DAEMON")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return run_daemon();
    }
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/feed/recorded.ndjson".to_string());
    let mut file = File::create(output)?;
    for line in io::stdin().lock().lines() {
        writeln!(file, "{}", line?)?;
    }
    Ok(())
}

fn run_daemon() -> io::Result<()> {
    let addr = std::env::var("RECORDER_HEALTH_ADDR").unwrap_or_else(|_| "0.0.0.0:9400".to_string());
    let production = std::env::var("PHOENIX_ENV")
        .map(|v| v.eq_ignore_ascii_case("production"))
        .unwrap_or(false);
    let ready = !production;
    let listener = TcpListener::bind(&addr)?;
    println!("phoenix-recorder health listening on {addr}");
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let path = request.split_whitespace().nth(1).unwrap_or("/");
        match path {
            "/healthz" => write_response(&mut stream, 200, "ok\n")?,
            "/readyz" if ready => write_response(&mut stream, 200, "ready\n")?,
            "/readyz" => write_response(
                &mut stream,
                503,
                "PostgreSQL schema verification and NATS subscription are not implemented\n",
            )?,
            _ => write_response(&mut stream, 404, "not found\n")?,
        }
    }
    Ok(())
}

fn write_response(stream: &mut impl Write, status: u16, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}
