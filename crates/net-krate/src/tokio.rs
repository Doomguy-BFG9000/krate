//! tokio's async I/O over Krate's sockets.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};

use ::tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::wake;

fn ready<T>(r: io::Result<T>, cx: &Context<'_>) -> Poll<io::Result<T>> {
    match r {
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            wake::soon(cx.waker());
            Poll::Pending
        }
        other => Poll::Ready(other),
    }
}

/// A name's addresses, without blocking the runtime.
pub async fn lookup_host(host: &str) -> io::Result<Vec<IpAddr>> {
    if let Ok(ip) = host
        .trim_matches(|c| c == '[' || c == ']')
        .parse::<IpAddr>()
    {
        return Ok(vec![ip]);
    }
    let h = crate::start_lookup(host)?;
    std::future::poll_fn(|cx| match crate::poll_lookup(h) {
        Some(answer) => Poll::Ready(answer),
        None => {
            wake::soon(cx.waker());
            Poll::Pending
        }
    })
    .await
}

/// A TCP connection with tokio's `AsyncRead` and `AsyncWrite`.
#[derive(Debug)]
pub struct TcpStream(crate::TcpStream);

impl TcpStream {
    /// Connect to a host (name or IP) and port.
    pub async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
        let mut s = crate::TcpStream::connect_nonblocking(host, port)?;
        std::future::poll_fn(|cx| match s.poll_connect() {
            Ok(Some(_)) => Poll::Ready(Ok(())),
            Ok(None) => {
                wake::soon(cx.waker());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        })
        .await?;
        Ok(TcpStream(s))
    }

    /// Connect to an address.
    pub async fn connect_addr(addr: SocketAddr) -> io::Result<TcpStream> {
        Self::connect(&addr.ip().to_string(), addr.port()).await
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.peer_addr()
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.0.set_nodelay(nodelay)
    }

    /// The std-like stream underneath.
    pub fn get_ref(&self) -> &crate::TcpStream {
        &self.0
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let space = buf.initialize_unfilled();
        match ready(self.0.try_read(space), cx) {
            Poll::Ready(Ok(n)) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ready(self.0.try_write(buf), cx)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// A UDP socket with async receive.
#[derive(Debug)]
pub struct UdpSocket(crate::UdpSocket);

impl UdpSocket {
    pub fn bind(ipv6: bool) -> io::Result<UdpSocket> {
        crate::UdpSocket::bind(ipv6).map(UdpSocket)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    pub async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> io::Result<usize> {
        self.0.send_to(buf, addr)
    }

    pub fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>> {
        ready(self.0.try_recv_from(buf), cx)
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        std::future::poll_fn(|cx| self.poll_recv_from(cx, buf)).await
    }

    /// The std-like socket underneath.
    pub fn get_ref(&self) -> &crate::UdpSocket {
        &self.0
    }
}
