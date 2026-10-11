// The thread parker for wasm with atomics on stable Rust (IC-939), added to
// parking_lot_core by crates/parking-lot-krate/make-parking-lot.py.
//
// parking_lot_core's own wasm parker calls `core::arch::wasm32::
// memory_atomic_wait32`, which is nightly-only, and without its `nightly`
// feature it falls back to one that panics "Parking not supported on this
// platform". std's own `thread::park` is built on the same atomic wait inside
// std, where it is allowed on stable -- so this parker parks through it.

use core::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, Thread};
use std::time::Instant;

// Helper type for putting a thread to sleep until some other thread wakes it up
pub struct ThreadParker {
    parked: AtomicBool,
    thread: Thread,
}

impl super::ThreadParkerT for ThreadParker {
    type UnparkHandle = UnparkHandle;

    const IS_CHEAP_TO_CONSTRUCT: bool = true;

    // Always made on the thread that will park (parking_lot keeps one per
    // thread), so this is the thread an unpark must wake.
    #[inline]
    fn new() -> ThreadParker {
        ThreadParker {
            parked: AtomicBool::new(false),
            thread: thread::current(),
        }
    }

    #[inline]
    unsafe fn prepare_park(&self) {
        self.parked.store(true, Ordering::Relaxed);
    }

    #[inline]
    unsafe fn timed_out(&self) -> bool {
        self.parked.load(Ordering::Relaxed)
    }

    #[inline]
    unsafe fn park(&self) {
        // std's park may return spuriously; the flag is the truth.
        while self.parked.load(Ordering::Acquire) {
            thread::park();
        }
    }

    #[inline]
    unsafe fn park_until(&self, timeout: Instant) -> bool {
        while self.parked.load(Ordering::Acquire) {
            match timeout.checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => thread::park_timeout(left),
                _ => return false,
            }
        }
        true
    }

    #[inline]
    unsafe fn unpark_lock(&self) -> UnparkHandle {
        // Clear the flag first: std's park token means an unpark that lands
        // before the park is not lost.
        self.parked.store(false, Ordering::Release);
        UnparkHandle(self.thread.clone())
    }
}

pub struct UnparkHandle(Thread);

impl super::UnparkHandleT for UnparkHandle {
    #[inline]
    unsafe fn unpark(self) {
        self.0.unpark();
    }
}

#[inline]
pub fn thread_yield() {
    thread::yield_now();
}
