use phoenix_engine::config::EngineConfig;
use phoenix_engine::execution::ExecutionCoordinator;
use phoenix_engine::readiness::{health_response, initialize_runtime, ReadinessState};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, RwLock};
use std::thread;

fn main() {
    let health_addr =
        std::env::var("ENGINE_HEALTH_ADDR").unwrap_or_else(|_| "0.0.0.0:9200".to_string());
    let readiness = Arc::new(RwLock::new(ReadinessState::initializing()));
    start_health_server(health_addr, Arc::clone(&readiness));

    let cfg = EngineConfig::from_env();
    match initialize_runtime(&cfg) {
        Ok(init) => {
            let coordinator = ExecutionCoordinator::new(init.mode);
            println!(
                "phoenix-engine mode={} live_allowed={}",
                coordinator.mode().as_str(),
                coordinator.live_allowed()
            );
            set_readiness(
                &readiness,
                ReadinessState::ready(init.readiness_detail),
            );
        }
        Err(err) => {
            eprintln!("phoenix-engine runtime initialization failed: {}", err.detail());
            set_readiness(&readiness, ReadinessState::not_ready(err.detail()));
        }
    }

    if std::env::var("PHOENIX_ONESHOT")
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn start_health_server(addr: String, readiness: Arc<RwLock<ReadinessState>>) {
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
                    let state = readiness
                        .read()
                        .map(|state| state.clone())
                        .unwrap_or_else(|_| {
                            ReadinessState::not_ready("readiness_state_unavailable")
                        });
                    match health_response(path, &state) {
                        Some(response) => {
                            let body = format!("{}\n", response.body);
                            write_response(&mut stream, response.status, &body);
                        }
                        None => write_response(&mut stream, 404, "not found\n"),
                    }
                }
                Err(err) => eprintln!("phoenix-engine health connection failed: {err}"),
            }
        }
    });
}

fn set_readiness(readiness: &Arc<RwLock<ReadinessState>>, state: ReadinessState) {
    match readiness.write() {
        Ok(mut current) => *current = state,
        Err(_) => eprintln!("phoenix-engine readiness state update failed"),
    }
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
