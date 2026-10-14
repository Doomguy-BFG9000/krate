//! A loopback HTTPS server whose certificate is signed by the test-only root
//! in `tests/fixtures`, standing in for a company network that inspects
//! secure traffic with its own root.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const SERVER_CERT: &str = include_str!("../fixtures/server.pem");
const SERVER_KEY: &str = include_str!("../fixtures/server.pkcs8.pem");

/// Serve one `GET` with `ok`, on a fresh loopback port. Returns the URL.
pub fn serve_once() -> String {
    let certs = CertificateDer::pem_slice_iter(SERVER_CERT.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("the fixture certificate parses");
    let key = PrivateKeyDer::from_pem_slice(SERVER_KEY.as_bytes()).expect("the fixture key parses");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .expect("the fixture pair is a valid server identity");
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        let mut tls = rustls::ServerConnection::new(config).unwrap();
        let mut stream = rustls::Stream::new(&mut tls, &mut tcp);
        let mut buf = [0u8; 2048];
        // A refused handshake ends here with an error: that is the point of
        // half of the tests.
        if stream.read(&mut buf).is_ok() {
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            let _ = stream.flush();
        }
    });
    format!("https://localhost:{port}/")
}
