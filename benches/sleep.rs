use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tokio::{time::Instant, runtime::Runtime};

fn bench_sleep_accuracy_concurrent_thread(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    sleep_accuracy(c, rt, "bench_sleep_accuracy_concurrent_thread");
}

fn bench_sleep_accuracy_multi(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .unwrap();
    sleep_accuracy(c, rt, "bench_sleep_accuracy_multi_thread");
}


fn sleep_accuracy(c: &mut Criterion, rt : Runtime, bench_id: &str) {
    c.bench_function(bench_id, |b| {
        b.iter_custom(|iters| {
            let start:Instant = Instant::now();
            rt.block_on(async {
                for _ in 0..iters {
                    tokio::time::sleep(tokio::time::Duration::from_millis(black_box(10))).await;
                }
                start.elapsed()
            })
        })
    });
}

criterion_group!(sleep, bench_sleep_accuracy_concurrent_thread,bench_sleep_accuracy_multi);

criterion_main!(sleep);
