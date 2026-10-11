//! Threads in a Krate app (IC-929), written as for any machine: a
//! `std::thread`, a scoped thread pair sharing a counter, and rayon's global
//! pool sized by `available_parallelism`, timed against the same work on one
//! thread.
//!
//! Built as a threaded app (a core module over a shared memory), with the
//! nightly whose std asks `sysconf` for the processor count:
//!
//! ```text
//! TOOLCHAIN=nightly-2026-06-13 THREADS=1 \
//!   ~/Projects/krate-bevy/build-app.sh $PWD/apps/krate-threads-probe
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
        let jobs: Vec<u64> = (0..640).collect();
        let t0 = Instant::now();
        let one: u64 = jobs.iter().map(|&n| work(n)).fold(0, |a, b| a ^ b);
        let serial = t0.elapsed().as_secs_f64();
        let t1 = Instant::now();
        let many: u64 = jobs.par_iter().map(|&n| work(n)).reduce(|| 0, |a, b| a ^ b);
        let parallel = t1.elapsed().as_secs_f64();
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
