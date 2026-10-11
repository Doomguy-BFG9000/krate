//! TCP and UDP for Krate apps, shaped like `std::net` (IC-935).
//!
//! A Krate app reaches the network through `krate:net/sockets`, judged
//! against the person's `net.connect` grants exactly as a fetch is. std's own
//! `std::net` cannot get there (wasm32-wasip1 has no connect), so a program
//! that speaks its own protocol uses these types instead: the same calls,
//! the same `io::Result`s, over Krate's interface.
//!
//! - [`TcpStream`]: connect, `Read`/`Write` (blocking) and `try_read` /
//!   `try_write` (non-blocking, `WouldBlock` when there is nothing to do).
//! - [`UdpSocket`]: bind, `send_to`, `recv_from` / `try_recv_from`.
//! - [`lookup_host`]: a name's addresses. An address it returned stands for
//!   the name, so a grant for the name covers it.
//! - feature `tokio`: [`tokio::TcpStream`] (`AsyncRead` + `AsyncWrite`) and
//!   [`tokio::UdpSocket`].
//! - feature `quinn`: [`quinn::KrateRuntime`] and [`quinn::KrateUdpSocket`],
//!   so QUIC runs over Krate's UDP.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use krate::net::sockets as k;
use krate::net::NetError;

#[cfg(feature = "tokio")]
pub mod tokio;

#[cfg(feature = "quinn")]
pub mod quinn;

mod wake;

/// How long a blocking call sleeps between asks. Short: a game's round trip
/// is a few milliseconds.
const NAP: Duration = Duration::from_millis(1);

/// A Krate network error as an `io::Error` of the matching kind.
pub fn io_error(e: NetError) -> io::Error {
    let kind = match &e {
        NetError::PermissionDenied => io::ErrorKind::PermissionDenied,
        NetError::Timeout => io::ErrorKind::TimedOut,
        NetError::InvalidUrl => io::ErrorKind::InvalidInput,
        NetError::DnsFailure(_) => io::ErrorKind::NotFound,
        NetError::ConnectFailure(why) if why.contains("refused") => {
            io::ErrorKind::ConnectionRefused
        }
        NetError::ConnectFailure(why) if why.contains("broke") => io::ErrorKind::ConnectionReset,
        _ => io::ErrorKind::Other,
    };
    let text = match e {
        NetError::PermissionDenied => {
            "permission denied: the app has no net.connect grant for this host and port".to_string()
        }
        NetError::DnsFailure(why)
        | NetError::ConnectFailure(why)
        | NetError::TlsFailure(why)
        | NetError::Protocol(why)
        | NetError::Other(why) => why,
        other => format!("{other:?}"),
    };
    io::Error::new(kind, text)
}

/// Start a lookup; [`poll_lookup`] says how it is doing.
pub fn start_lookup(host: &str) -> io::Result<u64> {
    k::lookup(host).map_err(io_error)
}

/// How a lookup started with [`start_lookup`] is doing: `None` while pending.
pub fn poll_lookup(handle: u64) -> Option<io::Result<Vec<IpAddr>>> {
    match k::poll_lookup(handle) {
        k::LookupStatus::Pending => None,
        k::LookupStatus::Ready(ips) => {
            Some(Ok(ips.iter().filter_map(|ip| ip.parse().ok()).collect()))
        }
        k::LookupStatus::Failed(e) => Some(Err(io_error(e))),
        k::LookupStatus::UnknownHandle => Some(Err(io::Error::other("the lookup is gone"))),
    }
}

/// A name's addresses (an IP literal is its own answer). Blocks until known.
pub fn lookup_host(host: &str) -> io::Result<Vec<IpAddr>> {
    if let Ok(ip) = host
        .trim_matches(|c| c == '[' || c == ']')
        .parse::<IpAddr>()
    {
        return Ok(vec![ip]);
    }
    let h = start_lookup(host)?;
    loop {
        if let Some(answer) = poll_lookup(h) {
            return answer;
        }
        std::thread::sleep(NAP);
    }
}

/// A TCP connection through `krate:net/sockets`.
#[derive(Debug)]
pub struct TcpStream {
    handle: u64,
    peer: Option<SocketAddr>,
}

impl TcpStream {
    /// Start connecting and return at once; [`TcpStream::poll_connect`] (or
    /// any read or write) says when it is up. The grant is checked here.
    pub fn connect_nonblocking(host: &str, port: u16) -> io::Result<TcpStream> {
        let handle = k::tcp_connect(host, port).map_err(io_error)?;
        Ok(TcpStream { handle, peer: None })
    }

    /// Connect and wait until it is up (or failed).
    pub fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
        let mut s = Self::connect_nonblocking(host, port)?;
        loop {
            match s.poll_connect() {
                Ok(Some(_)) => return Ok(s),
                Ok(None) => std::thread::sleep(NAP),
                Err(e) => return Err(e),
            }
        }
    }

    /// Connect to an address.
    pub fn connect_addr(addr: SocketAddr) -> io::Result<TcpStream> {
        Self::connect(&addr.ip().to_string(), addr.port())
    }

    /// `Some(peer)` once connected, `None` while still dialling.
    pub fn poll_connect(&mut self) -> io::Result<Option<SocketAddr>> {
        if let Some(peer) = self.peer {
            return Ok(Some(peer));
        }
        match k::tcp_status(self.handle) {
            k::ConnectStatus::Pending => Ok(None),
            k::ConnectStatus::Connected(peer) => {
                let peer = peer
                    .parse()
                    .map_err(|_| io::Error::other(format!("a peer address {peer:?}")))?;
                self.peer = Some(peer);
                Ok(Some(peer))
            }
            k::ConnectStatus::Failed(e) => Err(io_error(e)),
            k::ConnectStatus::UnknownHandle => Err(io::ErrorKind::NotConnected.into()),
        }
    }

    /// The address connected to.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.peer.ok_or_else(|| io::ErrorKind::NotConnected.into())
    }

    /// Krate sets no-delay on every connection: always true.
    pub fn set_nodelay(&self, _nodelay: bool) -> io::Result<()> {
        Ok(())
    }

    /// Read what has arrived: `Ok(0)` at the end, `WouldBlock` when nothing
    /// has (yet).
    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match k::tcp_read(self.handle, buf.len().min(u32::MAX as usize) as u32) {
            k::ReadResult::Data(bytes) => {
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            k::ReadResult::Pending => Err(io::ErrorKind::WouldBlock.into()),
            k::ReadResult::Closed => Ok(0),
            k::ReadResult::Failed(e) => Err(io_error(e)),
        }
    }

    /// Queue bytes to send: how many were taken, `WouldBlock` when none.
    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match k::tcp_write(self.handle, buf) {
            Ok(0) => Err(io::ErrorKind::WouldBlock.into()),
            Ok(n) => Ok(n as usize),
            Err(e) => Err(io_error(e)),
        }
    }

    /// The handle in `krate:net/sockets`.
    pub fn handle(&self) -> u64 {
        self.handle
    }
}

impl Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.try_read(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(NAP),
                other => return other,
            }
        }
    }
}

impl Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.try_write(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(NAP),
                other => return other,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        k::close(self.handle);
    }
}

/// A UDP socket through `krate:net/sockets`. It hears only addresses it has
/// sent to.
#[derive(Debug)]
pub struct UdpSocket {
    handle: u64,
    ipv6: bool,
}

impl UdpSocket {
    /// A socket on a port the host picks.
    pub fn bind(ipv6: bool) -> io::Result<UdpSocket> {
        let handle = k::udp_bind(ipv6).map_err(io_error)?;
        Ok(UdpSocket { handle, ipv6 })
    }

    /// The local address: the unspecified address and the port given.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        let port = k::udp_local_port(self.handle).map_err(io_error)?;
        let ip: IpAddr = if self.ipv6 {
            "::".parse().expect("an address")
        } else {
            "0.0.0.0".parse().expect("an address")
        };
        Ok(SocketAddr::new(ip, port))
    }

    /// Send one datagram to a host (name or IP) and port.
    pub fn send_to_host(&self, buf: &[u8], host: &str, port: u16) -> io::Result<usize> {
        k::udp_send(self.handle, host, port, buf).map_err(io_error)?;
        Ok(buf.len())
    }

    /// Send one datagram to an address.
    pub fn send_to(&self, buf: &[u8], addr: SocketAddr) -> io::Result<usize> {
        self.send_to_host(buf, &addr.ip().to_string(), addr.port())
    }

    /// The next datagram, or `WouldBlock`. A datagram longer than `buf` is
    /// cut to fit, as UDP does.
    pub fn try_recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let Some(d) = k::udp_receive(self.handle) else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        let from = SocketAddr::new(
            d.host
                .parse()
                .map_err(|_| io::Error::other(format!("a sender {:?}", d.host)))?,
            d.port,
        );
        let n = d.data.len().min(buf.len());
        buf[..n].copy_from_slice(&d.data[..n]);
        Ok((n, from))
    }

    /// Wait for the next datagram.
    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        loop {
            match self.try_recv_from(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(NAP),
                other => return other,
            }
        }
    }

    /// The handle in `krate:net/sockets`.
    pub fn handle(&self) -> u64 {
        self.handle
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        k::close(self.handle);
    }
}
