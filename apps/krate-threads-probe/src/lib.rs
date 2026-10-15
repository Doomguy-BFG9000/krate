//! Threads in a Krate app (IC-929), written as for any machine: a
//! `std::thread`, a scoped thread pair sharing a counter, and rayon's global
//! pool sized by `available_parallelism`, timed against the same work on one
//! thread.
//!
//! Built as a threaded app (a core module over a shared memory) on stable
//! Rust: from 1.99 std asks `sysconf` for the processor count, which
//! krate-c-runtime answers from the host (K-1055). Krate's own build makes it:
//!
//! ```text
//! krate check-app apps/krate-threads-probe --no-run
//! ```
//!
//! The result is crates/cli/tests/fixtures/threads-probe.wasm.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rayon::prelude::*;

fn work(n: u64) -> u64 {
    let mut x = n;
    for _ in 0..4_000_000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    x
}

struct App;

impl krate::Guest for App {
    fn run() -> i32 {
        krate_c_runtime::start();
        println!(
            "available_parallelism: {:?}",
            std::thread::available_parallelism().map(|n| n.get())
        );
        let t = std::thread::spawn(|| (std::thread::current().id(), work(7)));
        let (id, v) = t.join().unwrap();
        println!("std::thread {id:?} returned {v:x}");
        let counter = AtomicU64::new(0);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..100_000 {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
        println!("4 scoped threads counted {}", counter.load(Ordering::Relaxed));
        // parking_lot under contention: its threads park and unpark, which
        // on wasm needs make-parking-lot.py's std parker (IC-939).
        let total = parking_lot::Mutex::new(0u64);
        let turn = (parking_lot::Mutex::new(0u32), parking_lot::Condvar::new());
        std::thread::scope(|s| {
            for i in 0..8u32 {
                let (total, turn) = (&total, &turn);
                s.spawn(move || {
                    // Wait for this thread's turn, then hand it on.
                    let mut t = turn.0.lock();
                    while *t != i {
                        turn.1.wait(&mut t);
                    }
                    *t += 1;
                    turn.1.notify_all();
                    drop(t);
                    for _ in 0..50_000 {
                        *total.lock() += 1;
                    }
                });
            }
        });
        println!(
            "parking_lot: 8 threads took their turns ({}) and counted {}",
            *turn.0.lock(),
            *total.lock()
        );
        let jobs: Vec<u64> = (0..640).collect();
        let t0 = Instant::now();
        let one: u64 = jobs.iter().map(|&n| work(n)).fold(0, |a, b| a ^ b);
        let serial = t0.elapsed().as_secs_f64();
        // How many pool threads were working at the same moment: proof of
        // real parallelism that does not depend on how busy the machine is,
        // which a speed-up does.
        let active = std::sync::atomic::AtomicUsize::new(0);
        let most = std::sync::atomic::AtomicUsize::new(0);
        let t1 = Instant::now();
        let many: u64 = jobs
            .par_iter()
            .map(|&n| {
                use std::sync::atomic::Ordering::SeqCst;
                let now = active.fetch_add(1, SeqCst) + 1;
                most.fetch_max(now, SeqCst);
                let r = work(n);
                active.fetch_sub(1, SeqCst);
                r
            })
            .reduce(|| 0, |a, b| a ^ b);
        let parallel = t1.elapsed().as_secs_f64();
        println!(
            "rayon: at most {} threads working at once",
            most.load(std::sync::atomic::Ordering::SeqCst)
        );
        println!(
            "one thread {:.0} ms; rayon on {} threads {:.0} ms; speed-up {:.2}x; same answer: {}",
            serial * 1000.0,
            rayon::current_num_threads(),
            parallel * 1000.0,
            serial / parallel,
            one == many
        );
        krate_c_runtime::finish();
        0
    }
}

krate::export!(App);
