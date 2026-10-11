//! Waking async tasks that wait on a socket.
//!
//! `krate:net/sockets` is asked, not told: there is no readiness event to
//! wait on. A future that found nothing hands its waker here, and one ticker
//! thread wakes every waiting task each millisecond, so it asks again. A
//! task that is not waiting costs nothing.

#![allow(dead_code)]

use std::sync::{Mutex, OnceLock};
use std::task::Waker;
use std::time::Duration;

static WAITING: Mutex<Vec<Waker>> = Mutex::new(Vec::new());
static TICKER: OnceLock<()> = OnceLock::new();

/// Wake this task again in about a millisecond.
pub(crate) fn soon(waker: &Waker) {
    TICKER.get_or_init(|| {
        std::thread::Builder::new()
            .name("net-krate-ticker".into())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_millis(1));
                let ready: Vec<Waker> = std::mem::take(&mut *WAITING.lock().unwrap());
                #[cfg(feature = "trace")]
                if !ready.is_empty() {
                    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1000 == 0 {
                        eprintln!("net-krate ticker: waking {}", ready.len());
                    }
                }
                for w in ready {
                    w.wake();
                }
            })
            .expect("a thread for the socket ticker (a threaded app)");
    });
    let mut waiting = WAITING.lock().unwrap();
    if !waiting.iter().any(|w| w.will_wake(waker)) {
        waiting.push(waker.clone());
    }
}
