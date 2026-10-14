//! A request made through `krate_http` really goes to the proxy the
//! environment names, and `NO_PROXY` really sends it direct (IC-949 N). The
//! choice itself is unit-tested; this proves the client is built with it.
//! Its own test binary, because it sets process-wide variables.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

/// A proxy that records the first request line it is sent and refuses it.
fn recording_proxy() -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut seen = Vec::new();
            let mut byte = [0u8; 1];
            while !seen.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
                seen.push(byte[0]);
            }
            let line = String::from_utf8_lossy(&seen)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            let _ = tx.send(line);
            let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        }
    });
    (port, rx)
}

#[test]
fn a_request_goes_through_the_named_proxy_unless_no_proxy_names_the_host() {
    let (port, seen) = recording_proxy();
    std::env::set_var("HTTPS_PROXY", format!("http://127.0.0.1:{port}"));
    std::env::set_var("NO_PROXY", "direct.invalid");

    let _ = krate_http::get("https://hub.example.invalid/x").call();
    let line = seen
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the request reached the proxy");
    assert_eq!(line, "CONNECT hub.example.invalid:443 HTTP/1.1");

    // Named in NO_PROXY: resolved directly (and fails, .invalid never
    // resolves), and the proxy hears nothing.
    let _ = krate_http::get("https://api.direct.invalid/x").call();
    assert!(
        seen.recv_timeout(std::time::Duration::from_millis(500))
            .is_err(),
        "a NO_PROXY host must not be sent to the proxy"
    );
}
