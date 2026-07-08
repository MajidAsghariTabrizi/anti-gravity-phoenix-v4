use std::io::{Read, Write};
use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let addr = std::env::var("RPC_GATEWAY_ADDR").unwrap_or_else(|_| "0.0.0.0:9300".to_string());
    let production = std::env::var("PHOENIX_ENV")
        .map(|v| v.eq_ignore_ascii_case("production"))
        .unwrap_or(false);
    let provider_urls = std::env::var("RPC_PROVIDER_URLS").unwrap_or_default();
    let ready = !production || provider_config_valid(&provider_urls);
    let listener = TcpListener::bind(&addr)?;
    println!("rpc-gateway listening on {addr}");
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
                "RPC_PROVIDER_URLS must contain at least one http(s) provider in production\n",
            )?,
            _ => write_response(&mut stream, 404, "not found\n")?,
        }
    }
    Ok(())
}

fn provider_config_valid(value: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|url| url.starts_with("https://") || url.starts_with("http://"))
}

fn write_response(stream: &mut impl Write, status: u16, body: &str) -> std::io::Result<()> {
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
