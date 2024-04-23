//! Benchmark spawning a task onto the basic and threaded Tokio executors.
//! This essentially measure the time to enqueue a task in the local and remote
//! case.

use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tokio::time::timeout;

// a vevry quick async task, but might timeout
async fn quick_job() -> usize {
    1
}

fn single_thread_scheduler_timeout(c: &mut Criterion) {
    do_test(c, 1, "single_thread_timeout");
}

fn multi_thread_scheduler_timeout(c: &mut Criterion) {
    do_test(c, 8, "multi_thread_timeout-8");
}

fn do_test(c: &mut Criterion, workers: usize, name: &str) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(workers)
        .build()
        .unwrap();

    c.bench_function(name, |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            runtime.block_on(async {
                black_box(spawn_job(iters as usize, workers).await);
            });
            start.elapsed()
        })
    });
}

async fn spawn_job(iters: usize, procs: usize) {
    let mut handles = Vec::with_capacity(procs);
    for _ in 0..procs {
        handles.push(tokio::spawn(async move {
            for _ in 0..iters / procs {
                let h = timeout(Duration::from_secs(1), quick_job());
                assert_eq!(black_box(h.await.unwrap()), 1);
            }
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
}

criterion_group!(
    timeout_benchmark,
    single_thread_scheduler_timeout,
    multi_thread_scheduler_timeout,
);

criterion_main!(timeout_benchmark);