//! A tokio multi-thread runtime in a Krate app (IC-929), written as for any
//! machine: CPU-bound tasks spread over the workers, a timer, an mpsc
//! channel between tasks, and spawn_blocking.
//!
//! Built as a threaded app (a core module over a shared memory), with the
//! nightly whose std asks `sysconf` for the processor count:
//!
//! ```text
//! TOOLCHAIN=nightly-2026-06-13 THREADS=1 EXTRA_RUSTFLAGS="--cfg tokio_unstable" \
//!   ~/Projects/krate-bevy/build-app.sh $PWD/apps/krate-tokio-probe
//! ```
//!
//! The result is crates/cli/tests/fixtures/tokio-probe.wasm.

use std::collections::HashSet;
use std::time::{Duration, Instant};

fn work(n: u64) -> u64 {
    let mut x = n;
    for _ in 0..4_000_000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    x
}

async fn spread(jobs: u64) -> (u64, usize) {
    let mut handles = Vec::new();
    for n in 0..jobs {
        handles.push(tokio::spawn(async move {
            (work(n), format!("{:?}", std::thread::current().id()))
        }));
    }
    let mut acc = 0;
    let mut threads = HashSet::new();
    for h in handles {
        let (v, t) = h.await.unwrap();
        acc ^= v;
        threads.insert(t);
    }
    (acc, threads.len())
}

struct App;

impl krate::Guest for App {
    fn run() -> i32 {
        krate_c_runtime::start();
        let multi = tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .expect("a multi-thread runtime");
        let single = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime");
        let t0 = Instant::now();
        let (a, _) = single.block_on(spread(128));
        let one = t0.elapsed().as_secs_f64();
        let t1 = Instant::now();
        let (b, threads) = multi.block_on(spread(128));
        let many = t1.elapsed().as_secs_f64();
        println!(
            "tokio: current-thread {:.0} ms; multi-thread {:.0} ms on {threads} worker threads; speed-up {:.2}x; same answer: {}",
            one * 1000.0,
            many * 1000.0,
            one / many,
            a == b
        );
        multi.block_on(async {
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let producer = tokio::spawn(async move {
                for i in 0..5u32 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    tx.send(i).await.unwrap();
                }
            });
            let mut got = Vec::new();
            while let Some(i) = rx.recv().await {
                got.push(i);
            }
            producer.await.unwrap();
            let blocking = tokio::task::spawn_blocking(|| work(3)).await.unwrap();
            let t = Instant::now();
            tokio::time::sleep(Duration::from_millis(100)).await;
            println!(
                "channel got {got:?}; spawn_blocking returned {blocking:x}; a 100 ms timer took {} ms",
                t.elapsed().as_millis()
            );
        });
        krate_c_runtime::finish();
        0
    }
}

krate::export!(App);
