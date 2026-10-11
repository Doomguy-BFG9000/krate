//! TCP and UDP for apps that speak their own protocol (IC-935).
//!
//! The shape is `async_ws`'s, for its reasons:
//!
//! - **The grant is judged before this module is reached.** The host
//!   implementation checks every connect and every datagram's destination
//!   against `net.connect` on the calling thread; nothing here dials a host
//!   it was not handed already judged.
//! - **Every guest-facing call returns at once.** A connection lives on two
//!   host threads (one reading, one writing); a lookup on one. The guest
//!   asks how they are doing from its own loop.
//! - **A socket cannot outlive its handle.** `close` shuts the connection
//!   down; dropping the table drops every socket and its threads notice.
//!
//! An address a lookup returned stands for the name looked up: the table
//! remembers which names each address came from, so a grant for a name
//! covers its addresses (and only those).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// The most sockets and lookups one app may hold at once. Each connection is
/// two OS threads for as long as it lives (K-137: the thread ceiling is a
/// process-wide crash, not an error), so the cap is a refusal the app can
/// handle.
pub(crate) const MAX_SOCKETS: usize = 64;

/// How much one connection holds in each direction before it pushes back:
/// writes take fewer bytes, and reading from the network pauses.
pub(crate) const QUEUE_BYTES: usize = 4 * 1024 * 1024;

/// How long a connect may take before it fails.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest UDP payload there is.
const MAX_DATAGRAM: usize = 65_535;

/// How a lookup is doing. Mirrors the WIT `lookup-status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupStatus {
    Pending,
    Ready(Vec<String>),
    Failed(String),
    UnknownHandle,
}

/// How a connection is doing. Mirrors the WIT `connect-status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectStatus {
    Pending,
    Connected(String),
    Failed(String),
    UnknownHandle,
}

/// What a read found. Mirrors the WIT `read-result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadResult {
    Data(Vec<u8>),
    Pending,
    Closed,
    Failed(String),
}

/// One datagram that arrived. Mirrors the WIT `datagram`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    pub host: String,
    pub port: u16,
    pub data: Vec<u8>,
}

#[derive(Default)]
struct TcpInner {
    /// Some once connected: the peer.
    peer: Option<String>,
    /// Some once it failed or broke.
    error: Option<String>,
    inbox: VecDeque<u8>,
    outbox: VecDeque<u8>,
    /// The other end finished sending.
    eof: bool,
    /// The guest closed it.
    closed: bool,
    stream: Option<std::net::TcpStream>,
}

struct TcpShared {
    inner: Mutex<TcpInner>,
    changed: Condvar,
}

struct Udp {
    socket: std::net::UdpSocket,
    /// Addresses this socket has sent to: the only ones it hears from.
    peers: HashSet<IpAddr>,
}

enum Socket {
    Tcp(Arc<TcpShared>),
    Udp(Udp),
}

/// Where a lookup's thread sends its answer.
type LookupAnswer = Receiver<Result<Vec<IpAddr>, String>>;

/// The table of an app's sockets and lookups, owned by its host.
#[derive(Default)]
pub struct AsyncSockets {
    next_handle: u64,
    sockets: BTreeMap<u64, Socket>,
    lookups: BTreeMap<u64, (String, LookupAnswer)>,
    /// Each address a lookup returned, with the names it came from.
    resolved: HashMap<IpAddr, BTreeSet<String>>,
}

impl AsyncSockets {
    pub fn new() -> Self {
        Self::default()
    }

    fn handle(&mut self) -> Result<u64, String> {
        if self.sockets.len() + self.lookups.len() >= MAX_SOCKETS {
            return Err(format!(
                "too many sockets: this app already holds {MAX_SOCKETS}"
            ));
        }
        self.next_handle += 1;
        Ok(self.next_handle)
    }

    /// The names an address stands for, from this app's own lookups.
    pub fn names_for(&self, host: &str) -> Vec<String> {
        host.parse::<IpAddr>()
            .ok()
            .and_then(|ip| self.resolved.get(&ip))
            .map(|names| names.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Start resolving a name the caller has judged.
    pub fn lookup(&mut self, host: String) -> Result<u64, String> {
        let handle = self.handle()?;
        let (tx, rx) = channel();
        let name = host.clone();
        std::thread::spawn(move || {
            let answer = (name.as_str(), 0)
                .to_socket_addrs()
                .map(|addrs| {
                    let mut ips: Vec<IpAddr> = Vec::new();
                    for a in addrs {
                        if !ips.contains(&a.ip()) {
                            ips.push(a.ip());
                        }
                    }
                    ips
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(answer);
        });
        self.lookups.insert(handle, (host, rx));
        Ok(handle)
    }

    pub fn lookup_status(&mut self, handle: u64) -> LookupStatus {
        let Some((_, rx)) = self.lookups.get(&handle) else {
            return LookupStatus::UnknownHandle;
        };
        let answer = match rx.try_recv() {
            Err(TryRecvError::Empty) => return LookupStatus::Pending,
            Err(TryRecvError::Disconnected) => Err("the lookup stopped".to_string()),
            Ok(answer) => answer,
        };
        let (name, _) = self.lookups.remove(&handle).expect("present above");
        match answer {
            Ok(ips) if ips.is_empty() => LookupStatus::Failed(format!("{name} has no address")),
            Ok(ips) => {
                for ip in &ips {
                    self.resolved
                        .entry(*ip)
                        .or_default()
                        .insert(name.to_ascii_lowercase());
                }
                LookupStatus::Ready(ips.iter().map(|ip| ip.to_string()).collect())
            }
            Err(why) => LookupStatus::Failed(why),
        }
    }

    /// Start a connection to an endpoint the caller has judged.
    pub fn tcp_connect(&mut self, host: String, port: u16) -> Result<u64, String> {
        let handle = self.handle()?;
        let shared = Arc::new(TcpShared {
            inner: Mutex::new(TcpInner::default()),
            changed: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        std::thread::spawn(move || run_connection(host, port, worker));
        self.sockets.insert(handle, Socket::Tcp(shared));
        Ok(handle)
    }

    pub fn tcp_status(&self, handle: u64) -> ConnectStatus {
        let Some(Socket::Tcp(shared)) = self.sockets.get(&handle) else {
            return ConnectStatus::UnknownHandle;
        };
        let inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
        match (&inner.error, &inner.peer) {
            (Some(why), _) => ConnectStatus::Failed(why.clone()),
            (None, Some(peer)) => ConnectStatus::Connected(peer.clone()),
            (None, None) => ConnectStatus::Pending,
        }
    }

    pub fn tcp_write(&self, handle: u64, bytes: &[u8]) -> Result<u32, String> {
        let Some(Socket::Tcp(shared)) = self.sockets.get(&handle) else {
            return Err("no such connection".to_string());
        };
        let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(why) = &inner.error {
            return Err(why.clone());
        }
        let room = QUEUE_BYTES.saturating_sub(inner.outbox.len());
        let taken = bytes.len().min(room);
        inner.outbox.extend(&bytes[..taken]);
        drop(inner);
        shared.changed.notify_all();
        Ok(taken as u32)
    }

    pub fn tcp_read(&self, handle: u64, max_bytes: u32) -> ReadResult {
        let Some(Socket::Tcp(shared)) = self.sockets.get(&handle) else {
            return ReadResult::Failed("no such connection".to_string());
        };
        let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !inner.inbox.is_empty() {
            let n = inner.inbox.len().min(max_bytes.max(1) as usize);
            let data: Vec<u8> = inner.inbox.drain(..n).collect();
            drop(inner);
            // The reader may be waiting for room.
            shared.changed.notify_all();
            return ReadResult::Data(data);
        }
        if let Some(why) = &inner.error {
            return ReadResult::Failed(why.clone());
        }
        if inner.eof {
            return ReadResult::Closed;
        }
        ReadResult::Pending
    }

    pub fn udp_bind(&mut self, ipv6: bool) -> Result<u64, String> {
        let handle = self.handle()?;
        let any: SocketAddr = if ipv6 {
            "[::]:0".parse().expect("an address")
        } else {
            "0.0.0.0:0".parse().expect("an address")
        };
        let socket = std::net::UdpSocket::bind(any).map_err(|e| e.to_string())?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        self.sockets.insert(
            handle,
            Socket::Udp(Udp {
                socket,
                peers: HashSet::new(),
            }),
        );
        Ok(handle)
    }

    pub fn udp_local_port(&self, handle: u64) -> Result<u16, String> {
        match self.sockets.get(&handle) {
            Some(Socket::Udp(udp)) => udp
                .socket
                .local_addr()
                .map(|a| a.port())
                .map_err(|e| e.to_string()),
            _ => Err("no such UDP socket".to_string()),
        }
    }

    /// Send to an endpoint the caller has judged. A name is resolved here
    /// (and remembered, as a lookup would be).
    pub fn udp_send(
        &mut self,
        handle: u64,
        host: &str,
        port: u16,
        data: &[u8],
    ) -> Result<(), String> {
        if data.len() > MAX_DATAGRAM {
            return Err(format!("a datagram carries at most {MAX_DATAGRAM} bytes"));
        }
        let ipv6 = match self.sockets.get(&handle) {
            Some(Socket::Udp(udp)) => udp
                .socket
                .local_addr()
                .map(|a| a.is_ipv6())
                .unwrap_or(false),
            _ => return Err("no such UDP socket".to_string()),
        };
        let ip = match host.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => {
                // A name: the first of its addresses this socket's family can
                // reach ("localhost" is ::1 first, which an IPv4 socket cannot).
                let ip = (host, port)
                    .to_socket_addrs()
                    .map_err(|e| e.to_string())?
                    .map(|a| a.ip())
                    .find(|ip| ip.is_ipv6() == ipv6)
                    .ok_or_else(|| {
                        format!(
                            "{host} has no {} address",
                            if ipv6 { "IPv6" } else { "IPv4" }
                        )
                    })?;
                self.resolved
                    .entry(ip)
                    .or_default()
                    .insert(host.to_ascii_lowercase());
                ip
            }
        };
        let Some(Socket::Udp(udp)) = self.sockets.get_mut(&handle) else {
            return Err("no such UDP socket".to_string());
        };
        udp.peers.insert(ip);
        match udp.socket.send_to(data, SocketAddr::new(ip, port)) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                Err("the send queue is full; the datagram was dropped".to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn udp_receive(&mut self, handle: u64) -> Option<Datagram> {
        let Some(Socket::Udp(udp)) = self.sockets.get_mut(&handle) else {
            return None;
        };
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            match udp.socket.recv_from(&mut buf) {
                Ok((n, from)) => {
                    // Only what came from an address this socket sent to.
                    if !udp.peers.contains(&from.ip()) {
                        continue;
                    }
                    return Some(Datagram {
                        host: from.ip().to_string(),
                        port: from.port(),
                        data: buf[..n].to_vec(),
                    });
                }
                // WouldBlock is "nothing yet"; a connection-refused ICMP is
                // also only "nothing", as UDP has no connection.
                Err(_) => return None,
            }
        }
    }

    pub fn close(&mut self, handle: u64) {
        self.lookups.remove(&handle);
        if let Some(Socket::Tcp(shared)) = self.sockets.remove(&handle) {
            close_tcp(&shared);
        }
    }
}

impl Drop for AsyncSockets {
    fn drop(&mut self) {
        for socket in self.sockets.values() {
            if let Socket::Tcp(shared) = socket {
                close_tcp(shared);
            }
        }
    }
}

fn close_tcp(shared: &TcpShared) {
    let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
    inner.closed = true;
    if let Some(stream) = &inner.stream {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    drop(inner);
    shared.changed.notify_all();
}

/// The connection's own thread: dial, then read until the end, while a
/// second thread writes what the app queues.
fn run_connection(host: String, port: u16, shared: Arc<TcpShared>) {
    let fail = |why: String| {
        let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.error.is_none() {
            inner.error = Some(why);
        }
        drop(inner);
        shared.changed.notify_all();
    };
    let addrs: Vec<SocketAddr> = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => return fail(format!("could not find {host}: {e}")),
    };
    let mut last = format!("{host} has no address");
    let mut stream = None;
    for addr in addrs {
        match std::net::TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = format!("could not connect to {addr}: {e}"),
        }
    }
    let Some(stream) = stream else {
        return fail(last);
    };
    let _ = stream.set_nodelay(true);
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let (Ok(writer), Ok(control)) = (stream.try_clone(), stream.try_clone()) else {
        return fail("could not share the connection between its threads".to_string());
    };
    {
        let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.closed {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        inner.peer = Some(peer);
        inner.stream = Some(control);
    }
    shared.changed.notify_all();
    let writer_shared = Arc::clone(&shared);
    std::thread::spawn(move || run_writer(writer, writer_shared));
    run_reader(stream, &shared);
}

fn run_reader(mut stream: std::net::TcpStream, shared: &TcpShared) {
    use std::io::Read;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // Hold off while the app has not taken what arrived.
        {
            let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
            while inner.inbox.len() >= QUEUE_BYTES && !inner.closed {
                inner = shared
                    .changed
                    .wait(inner)
                    .unwrap_or_else(|p| p.into_inner());
            }
            if inner.closed {
                return;
            }
        }
        match stream.read(&mut buf) {
            Ok(0) => {
                let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
                inner.eof = true;
                return;
            }
            Ok(n) => {
                let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
                inner.inbox.extend(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
                if !inner.closed && inner.error.is_none() {
                    inner.error = Some(format!("the connection broke: {e}"));
                }
                return;
            }
        }
    }
}

fn run_writer(mut stream: std::net::TcpStream, shared: Arc<TcpShared>) {
    use std::io::Write;
    loop {
        let chunk: Vec<u8> = {
            let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
            while inner.outbox.is_empty() && !inner.closed && inner.error.is_none() {
                inner = shared
                    .changed
                    .wait(inner)
                    .unwrap_or_else(|p| p.into_inner());
            }
            if inner.closed || inner.error.is_some() {
                return;
            }
            let n = inner.outbox.len().min(256 * 1024);
            inner.outbox.drain(..n).collect()
        };
        shared.changed.notify_all();
        if let Err(e) = stream.write_all(&chunk) {
            let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
            if !inner.closed && inner.error.is_none() {
                inner.error = Some(format!("the connection broke: {e}"));
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait<T>(mut f: impl FnMut() -> Option<T>) -> T {
        for _ in 0..500 {
            if let Some(v) = f() {
                return v;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn a_connection_carries_bytes_both_ways_and_ends() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = listener.accept().unwrap();
            let mut got = [0u8; 5];
            s.read_exact(&mut got).unwrap();
            s.write_all(b"world").unwrap();
            got
        });
        let mut table = AsyncSockets::new();
        let h = table.tcp_connect("127.0.0.1".into(), port).unwrap();
        // Written before the connection is up: sent once it is.
        assert_eq!(table.tcp_write(h, b"hello").unwrap(), 5);
        let peer = wait(|| match table.tcp_status(h) {
            ConnectStatus::Connected(p) => Some(p),
            ConnectStatus::Pending => None,
            other => panic!("{other:?}"),
        });
        assert_eq!(peer, format!("127.0.0.1:{port}"));
        let mut got = Vec::new();
        let end = wait(|| match table.tcp_read(h, 2) {
            ReadResult::Data(d) => {
                got.extend(d);
                None
            }
            ReadResult::Pending => None,
            other => Some(other),
        });
        assert_eq!(got, b"world");
        assert_eq!(end, ReadResult::Closed);
        assert_eq!(&server.join().unwrap(), b"hello");
        table.close(h);
        assert_eq!(table.tcp_status(h), ConnectStatus::UnknownHandle);
    }

    #[test]
    fn a_refused_connection_fails_with_the_reason() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut table = AsyncSockets::new();
        let h = table.tcp_connect("127.0.0.1".into(), port).unwrap();
        let why = wait(|| match table.tcp_status(h) {
            ConnectStatus::Failed(why) => Some(why),
            _ => None,
        });
        assert!(why.contains("could not connect"), "{why}");
    }

    #[test]
    fn a_udp_socket_hears_only_addresses_it_sent_to() {
        let echo = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        let mut table = AsyncSockets::new();
        let h = table.udp_bind(false).unwrap();
        let mine = table.udp_local_port(h).unwrap();
        // A stranger's datagram before we sent anywhere: not heard.
        let stranger = std::net::UdpSocket::bind("127.0.0.2:0")
            .or_else(|_| std::net::UdpSocket::bind("[::1]:0"))
            .unwrap();
        let _ = stranger.send_to(b"psst", ("127.0.0.1", mine));
        // By name: "localhost" is ::1 first, and this IPv4 socket picks 127.0.0.1.
        table.udp_send(h, "localhost", echo_port, b"ping").unwrap();
        let mut buf = [0u8; 16];
        let (n, from) = echo.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
        echo.send_to(b"pong", from).unwrap();
        let got = wait(|| table.udp_receive(h));
        assert_eq!(got.data, b"pong");
        assert_eq!(got.host, "127.0.0.1");
        assert_eq!(got.port, echo_port);
        assert!(table.udp_receive(h).is_none());
    }

    #[test]
    fn a_lookup_remembers_which_name_an_address_stands_for() {
        let mut table = AsyncSockets::new();
        let h = table.lookup("localhost".into()).unwrap();
        let ips = wait(|| match table.lookup_status(h) {
            LookupStatus::Ready(ips) => Some(ips),
            LookupStatus::Pending => None,
            other => panic!("{other:?}"),
        });
        assert!(!ips.is_empty());
        assert_eq!(table.names_for(&ips[0]), vec!["localhost".to_string()]);
        assert!(table.names_for("10.9.8.7").is_empty());
        assert_eq!(table.lookup_status(h), LookupStatus::UnknownHandle);
    }
}
