//! QUIC over Krate's UDP: quinn's runtime and socket traits.
//!
//! ```ignore
//! let socket = Arc::new(net_krate::quinn::KrateUdpSocket::bind(false)?);
//! let endpoint = quinn::Endpoint::new_with_abstract_socket(
//!     quinn::EndpointConfig::default(), None, socket,
//!     Arc::new(net_krate::quinn::KrateRuntime))?;
//! ```
//!
//! quinn built by make-quinn.py has no std sockets on wasi, so its
//! `Endpoint::client` / `server` constructors are gone and an endpoint is
//! always made this way. Timers and tasks are tokio's (the app's own
//! runtime); datagrams are
//! `krate:net/sockets`', so every address quinn sends to is judged against
//! the person's `net.connect` grants.

use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use ::quinn::udp::{RecvMeta, Transmit};
use ::quinn::{AsyncTimer, AsyncUdpSocket, Runtime, UdpPoller};

use crate::wake;

/// quinn's runtime for a Krate app: tokio's timers and tasks.
#[derive(Debug, Clone, Copy, Default)]
pub struct KrateRuntime;

struct Timer(Pin<Box<::tokio::time::Sleep>>);

impl fmt::Debug for Timer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Timer")
    }
}

impl AsyncTimer for Timer {
    fn reset(mut self: Pin<&mut Self>, i: Instant) {
        self.0.as_mut().reset(i.into());
    }

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.0.as_mut().poll(cx)
    }
}

impl Runtime for KrateRuntime {
    fn new_timer(&self, i: Instant) -> Pin<Box<dyn AsyncTimer>> {
        Box::pin(Timer(Box::pin(::tokio::time::sleep_until(i.into()))))
    }

    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        ::tokio::spawn(future);
    }
}

/// A UDP socket quinn can drive, over `krate:net/sockets`.
#[derive(Debug)]
pub struct KrateUdpSocket(crate::UdpSocket);

impl KrateUdpSocket {
    pub fn bind(ipv6: bool) -> io::Result<KrateUdpSocket> {
        crate::UdpSocket::bind(ipv6).map(KrateUdpSocket)
    }
}

/// Sending never waits: a full host queue drops the datagram, which QUIC
/// already expects of UDP.
#[derive(Debug)]
struct AlwaysWritable;

impl UdpPoller for AlwaysWritable {
    fn poll_writable(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncUdpSocket for KrateUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(AlwaysWritable)
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        #[cfg(feature = "trace")]
        eprintln!(
            "net-krate quic: send {} bytes to {}",
            transmit.contents.len(),
            transmit.destination
        );
        // One datagram per segment when quinn batches (it does not, as
        // max_transmit_segments is 1).
        let size = transmit.segment_size.unwrap_or(transmit.contents.len());
        for chunk in transmit.contents.chunks(size.max(1)) {
            self.0.send_to(chunk, transmit.destination)?;
        }
        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let (Some(buf), Some(m)) = (bufs.first_mut(), meta.first_mut()) else {
            return Poll::Ready(Ok(0));
        };
        match self.0.try_recv_from(buf) {
            Ok((len, addr)) => {
                #[cfg(feature = "trace")]
                eprintln!("net-krate quic: received {len} bytes from {addr}");
                let mut fresh = RecvMeta::default();
                fresh.addr = addr;
                fresh.len = len;
                fresh.stride = len;
                *m = fresh;
                Poll::Ready(Ok(1))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wake::soon(cx.waker());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}
