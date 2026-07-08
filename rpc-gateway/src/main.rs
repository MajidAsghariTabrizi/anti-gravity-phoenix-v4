use std::io::{Read, Write};
use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let addr = std::env::var("RPC_GATEWAY_ADDR").unwrap_or_else(|_| "0.0.0.0:9300".to_string());
    let listener = TcpListener::bind(&addr)?;
    println!("rpc-gateway listening on {addr}");
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let body = b"ok\n";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
    }
    Ok(())
}

