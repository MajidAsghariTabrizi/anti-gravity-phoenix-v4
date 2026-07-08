use phoenix_engine::config::EngineConfig;
use phoenix_engine::execution::{ExecutionCoordinator, ExecutionMode};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn main() {
    let cfg = EngineConfig::from_env();
    let mode = ExecutionMode::from_env(cfg.mode.as_str(), cfg.live_execution);
    let coordinator = ExecutionCoordinator::new(mode);
    println!(
        "phoenix-engine mode={} live_allowed={}",
        coordinator.mode().as_str(),
        coordinator.live_allowed()
    );
    let production = std::env::var("PHOENIX_ENV")
        .map(|v| v.eq_ignore_ascii_case("production"))
        .unwrap_or(false);
    let ready = !production;
    let detail = if production {
        "NATS subscription and production state bootstrap are not implemented"
    } else {
        "ready"
    };
    let health_addr =
        std::env::var("ENGINE_HEALTH_ADDR").unwrap_or_else(|_| "0.0.0.0:9200".to_string());
    start_health_server(health_addr, ready, detail.to_string());
    if std::env::var("PHOENIX_ONESHOT").map(|v| v == "true").unwrap_or(false) {
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn start_health_server(addr: String, ready: bool, detail: String) {
    thread::spawn(move || {
        let listener = match TcpListener::bind(&addr) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("phoenix-engine health bind failed on {addr}: {err}");
                return;
            }
        };
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let mut buf = [0u8; 1024];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("/");
                    if path == "/healthz" {
                        write_response(&mut stream, 200, "ok\n");
                    } else if path == "/readyz" {
                        if ready {
                            write_response(&mut stream, 200, "ready\n");
                        } else {
                            let body = format!("{detail}\n");
                            write_response(&mut stream, 503, &body);
                        }
                    } else {
                        write_response(&mut stream, 404, "not found\n");
                    }
                }
                Err(err) => eprintln!("phoenix-engine health connection failed: {err}"),
            }
        }
    });
}

fn write_response(stream: &mut impl Write, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
}
