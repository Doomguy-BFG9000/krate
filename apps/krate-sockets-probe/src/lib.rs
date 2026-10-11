//! TCP, UDP, name lookup and QUIC from inside a Krate app (IC-935), through
//! net-krate over `krate:net/sockets`.
//!
//! `run` takes one job from its arguments:
//!
//! ```text
//! tcp HOST PORT        connect, send "hello krate", print what comes back
//! udp HOST PORT        send "ping", print the answer
//! lookup HOST          print the name's addresses
//! tokio HOST PORT      the tcp job on a tokio runtime, async
//! quic HOST PORT NAME  a QUIC connection (certificate for NAME, trusted from
//!                      the DER file cert.der in the app's files): send a
//!                      line on a stream, print the answer
//! wake                 a tokio runtime with its I/O driver (as `enable_all`
//!                      builds it), one worker, a timer 30 s away: how long
//!                      a task woken from another thread waits (K-1069)
//! ```
//!
//! Built as a threaded app (the socket ticker and tokio want threads):
//!
//! ```text
//! TOOLCHAIN=nightly-2026-06-13 THREADS=1 EXTRA_RUSTFLAGS="--cfg tokio_unstable" \
//!   ~/Projects/krate-bevy/build-app.sh $PWD/apps/krate-sockets-probe
//! ```
//!
//! after `python3 crates/net-krate/make-quinn.py patches` from the repo root
//! (tokio's I/O driver on WASI needs `tokio_unstable`, as in Veloren).
//! The result is crates/cli/tests/fixtures/sockets-probe.wasm.

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Instant;

fn tcp(host: &str, port: u16) -> std::io::Result<String> {
    let t = Instant::now();
    let mut s = net_krate::TcpStream::connect(host, port)?;
    s.write_all(b"hello krate")?;
    let mut got = vec![0u8; 11];
    s.read_exact(&mut got)?;
    Ok(format!(
        "tcp: connected to {}, sent 11 bytes, got back {:?} in {} ms",
        s.peer_addr()?,
        String::from_utf8_lossy(&got),
        t.elapsed().as_millis()
    ))
}

fn udp(host: &str, port: u16) -> std::io::Result<String> {
    let s = net_krate::UdpSocket::bind(false)?;
    s.send_to_host(b"ping", host, port)?;
    let mut buf = [0u8; 64];
    let (n, from) = s.recv_from(&mut buf)?;
    Ok(format!(
        "udp: sent \"ping\" from port {}, got {:?} from {from}",
        s.local_addr()?.port(),
        String::from_utf8_lossy(&buf[..n])
    ))
}

async fn tokio_tcp(host: &str, port: u16) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ips = net_krate::tokio::lookup_host(host).await?;
    // Every address in turn, as std's connect does.
    let mut last = None;
    let mut stream = None;
    for ip in &ips {
        match net_krate::tokio::TcpStream::connect(&ip.to_string(), port).await {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let Some(mut s) = stream else {
        return Err(last.unwrap_or_else(|| std::io::Error::other("no address")));
    };
    s.write_all(b"hello krate").await?;
    let mut got = vec![0u8; 11];
    s.read_exact(&mut got).await?;
    Ok(format!(
        "tokio: {host} is {ips:?}, connected to {}, got back {:?}",
        s.peer_addr()?,
        String::from_utf8_lossy(&got)
    ))
}

async fn quic(host: &str, port: u16, name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let der = std::fs::read("cert.der")?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(der))?;
    let crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let client = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    ));
    let socket = Arc::new(net_krate::quinn::KrateUdpSocket::bind(false)?);
    let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        socket,
        Arc::new(net_krate::quinn::KrateRuntime),
    )?;
    endpoint.set_default_client_config(client);
    let ip = *net_krate::tokio::lookup_host(host)
        .await?
        .iter()
        .find(|ip| ip.is_ipv4())
        .ok_or("no IPv4 address")?;
    let t = Instant::now();
    let conn = endpoint
        .connect(std::net::SocketAddr::new(ip, port), name)?
        .await?;
    let handshake = t.elapsed().as_millis();
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(b"hello over quic").await?;
    send.finish()?;
    let answer = recv.read_to_end(1024).await?;
    // A bulk stream both ways, as a game's terrain is: 4 MB out, 4 MB back.
    let t = Instant::now();
    let (mut send, mut recv) = conn.open_bi().await?;
    let bulk = vec![7u8; 4 << 20];
    let writer = async {
        send.write_all(&bulk).await?;
        send.finish()?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (w, back) = tokio::join!(writer, recv.read_to_end(8 << 20));
    w?;
    let back = back?;
    let bulk_line = format!(
        "; bulk: {} bytes back in {} ms",
        back.len(),
        t.elapsed().as_millis()
    );
    conn.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(format!(
        "quic: handshake with {name} at {ip}:{port} in {handshake} ms, got back {:?}{bulk_line}",
        String::from_utf8_lossy(&answer)
    ))
}

/// A task woken from a thread outside the runtime, while the runtime's one
/// worker waits in its I/O driver for a timer 30 s away: the worst wait of
/// five, in milliseconds. Natively microseconds; on WASI tokio cannot wake
/// its driver from another thread, and this was seconds (K-1069).
fn remote_wake() -> f64 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a runtime");
    let mut worst = 0.0f64;
    for _ in 0..5 {
        let (tx, rx) = tokio::sync::oneshot::channel::<Instant>();
        let task = rt.spawn(async move {
            let _far = tokio::spawn(tokio::time::sleep(std::time::Duration::from_secs(30)));
            rx.await.map(|sent| sent.elapsed()).unwrap_or_default()
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = tx.send(Instant::now());
        let waited = rt.block_on(task).unwrap_or_default();
        worst = worst.max(waited.as_secs_f64() * 1000.0);
    }
    rt.shutdown_background();
    worst
}

struct App;

impl krate::Guest for App {
    fn run() -> i32 {
        let args: Vec<String> = std::env::args().collect();
        let a = |i: usize| args.get(i).cloned().unwrap_or_default();
        let port = |i: usize| a(i).parse::<u16>().unwrap_or(0);
        let rt = || {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a runtime")
        };
        let out: Result<String, String> = match a(1).as_str() {
            "tcp" => tcp(&a(2), port(3)).map_err(|e| e.to_string()),
            "udp" => udp(&a(2), port(3)).map_err(|e| e.to_string()),
            "lookup" => net_krate::lookup_host(&a(2))
                .map(|ips| format!("lookup: {} is {ips:?}", a(2)))
                .map_err(|e| e.to_string()),
            "tokio" => rt()
                .block_on(tokio_tcp(&a(2), port(3)))
                .map_err(|e| e.to_string()),
            "quic" => rt()
                .block_on(quic(&a(2), port(3), &a(4)))
                .map_err(|e| e.to_string()),
            // The same on a multi-thread runtime, as a game's client runs it.
            "quic-mt" => tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_time()
                .build()
                .expect("a runtime")
                .block_on(quic(&a(2), port(3), &a(4)))
                .map_err(|e| e.to_string()),
            "wake" => Ok(format!(
                "a task woken from another thread waited at most {:.1} ms",
                remote_wake()
            )),
            other => Err(format!(
                "no job {other:?}: tcp, udp, lookup, tokio, quic, quic-mt or wake"
            )),
        };
        let code = match out {
            Ok(line) => {
                println!("{line}");
                0
            }
            Err(why) => {
                println!("refused or failed: {why}");
                1
            }
        };
        krate_c_runtime::finish();
        code
    }
}

krate::export!(App);
