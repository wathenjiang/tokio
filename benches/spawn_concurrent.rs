use std::time::Instant;

use criterion::{measurement::WallTime, *};

fn spawn_tasks_current_thread(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    c.bench_function("spawn_tasks_current_thread", move |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            runtime.block_on(async {
                black_box(spawn_job(iters as usize, 1).await);
            });
            start.elapsed()
        })
    });
}

fn spawn_tasks_current_thread_parallel(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    c.bench_function("spawn_tasks_current_thread_parallel", move |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            runtime.block_on(async {
                black_box(spawn_job(iters as usize, num_cpus::get_physical()).await);
            });
            start.elapsed()
        })
    });
}

fn bench_create_runtime_multi_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("create_runtime_multi_thread");
    create_multi_thread_runtime::<1>(&mut group);
    create_multi_thread_runtime::<8>(&mut group);
    create_multi_thread_runtime::<32>(&mut group);
    create_multi_thread_runtime::<64>(&mut group);
    create_multi_thread_runtime::<128>(&mut group);
    create_multi_thread_runtime::<256>(&mut group);
    create_multi_thread_runtime::<512>(&mut group);
    create_multi_thread_runtime::<1024>(&mut group);
    create_multi_thread_runtime::<2048>(&mut group);
    create_multi_thread_runtime::<4096>(&mut group);
}

fn create_multi_thread_runtime<const S: usize>(g: &mut BenchmarkGroup<WallTime>) {
    g.bench_function(format!("{:04}", S), |b| {
        b.iter(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .spawn_concurrency_level(black_box(S))
                .build()
                .unwrap();
            drop(runtime);
        })
    });
}

fn bench_parallel_spawn_multi_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("spawn_parallel_multi_thread");
    spawn_tasks_parallel_multi_thread::<16,1>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,8>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,32>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,64>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,128>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,256>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,512>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,1024>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,2048>(&mut group);
    spawn_tasks_parallel_multi_thread::<16,4096>(&mut group);
}

fn bench_parallel_spawn_multi_thread2(c: &mut Criterion) {
    let mut group = c.benchmark_group("spawn_parallel_multi_thread2");
    let ws:[usize;8] = [1,2,4,8,16,32,64,128];
    let ss:[usize;12] = [1,2,4,8,16,32,64,128,256,512,1024,2048];

    for w in ws{
        for s in ss{
            spawn_tasks_parallel_multi_thread2(&mut group,w,s);
        }
    }
}


fn spawn_tasks_parallel_multi_thread<const W: usize,const S: usize>(g: &mut BenchmarkGroup<WallTime>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(W)
        .spawn_concurrency_level(black_box(S))
        .build()
        .unwrap();
    g.bench_function(format!("{:02}-{:04}",W, S), |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            runtime.block_on(async {
                black_box(spawn_job(iters as usize, W).await);
            });
            start.elapsed()
        })
    });
}

fn spawn_tasks_parallel_multi_thread2(g: &mut BenchmarkGroup<WallTime>, w : usize, s:usize) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(w)
        .spawn_concurrency_level(black_box(s))
        .build()
        .unwrap();
    g.bench_function(format!("{:03}-{:04}",w, s), |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            runtime.block_on(async {
                black_box(spawn_job(iters as usize, w).await);
            });
            start.elapsed()
        })
    });
}

async fn spawn_job(iters: usize, procs: usize) {
    for _ in 0..procs {
        let mut threads_handles = Vec::with_capacity(procs);
        threads_handles.push(tokio::spawn(async move {
            let mut thread_handles = Vec::with_capacity(iters / procs);
            for _ in 0..iters / procs {
                thread_handles.push(tokio::spawn(async {
                    let val = 1 + 1;
                    tokio::task::yield_now().await;
                    black_box(val)
                }));
            }
            for handle in thread_handles {
                handle.await.unwrap();
            }
        }));
        for handle in threads_handles {
            handle.await.unwrap();
        }
    }
}

fn bench_shutdown_parallel_multi_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("shutdown_runtime_multi_thread");
    shutdown_tasks_parallel::<16,1>(&mut group);
    shutdown_tasks_parallel::<16,8>(&mut group);
    shutdown_tasks_parallel::<16,32>(&mut group);
    shutdown_tasks_parallel::<16,64>(&mut group);
    shutdown_tasks_parallel::<16,128>(&mut group);
    shutdown_tasks_parallel::<16,256>(&mut group);
    shutdown_tasks_parallel::<16,512>(&mut group);
    shutdown_tasks_parallel::<16,1024>(&mut group);
    shutdown_tasks_parallel::<16,2048>(&mut group);
    shutdown_tasks_parallel::<16,4096>(&mut group);
    group.finish();
}

fn bench_shutdown_parallel_multi_thread2(c: &mut Criterion) {
    let mut group = c.benchmark_group("shutdown_runtime_multi_thread2");
    shutdown_tasks_parallel::<1,8>(&mut group);
    shutdown_tasks_parallel::<2,8>(&mut group);
    shutdown_tasks_parallel::<4,8>(&mut group);
    shutdown_tasks_parallel::<8,8>(&mut group);
    shutdown_tasks_parallel::<16,8>(&mut group);
    shutdown_tasks_parallel::<32,8>(&mut group);
    shutdown_tasks_parallel::<64, 8>(&mut group);
    group.finish();
}

fn shutdown_tasks_parallel<const W: usize,const S: usize>(g: &mut BenchmarkGroup<WallTime>) {
    g.bench_function(format!("{:02}-{:04}",W, S), |b| {
        b.iter_custom(|iters| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .spawn_concurrency_level(black_box(S))
                .enable_time()
                .build()
                .unwrap();
            runtime.block_on(async {
                black_box(job_shutdown(iters as usize, W).await);
            });
            let start = Instant::now();
            drop(runtime);
            start.elapsed()
        })
    });
}

async fn job_shutdown(iters: usize, procs: usize) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for _ in 0..procs {
        let tx = tx.clone();
        tokio::spawn(async move {
            for _ in 0..iters / procs {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let val = 1 + 1;
                    tx.send(()).unwrap();
                    tokio::time::sleep(tokio::time::Duration::from_secs(1000)).await; // it will never return
                    black_box(val)
                });
            }
        });
    }
    for _ in 0..(iters / procs) * procs {
        rx.recv().await;
    }
}

criterion_group!(
    benches,
    // spawn_tasks_current_thread,
    // spawn_tasks_current_thread_parallel,
    // bench_create_runtime_multi_thread,
    // bench_parallel_spawn_multi_thread,
    bench_parallel_spawn_multi_thread2,
    // bench_shutdown_parallel_multi_thread,
    // bench_shutdown_parallel_multi_thread2
);
criterion_main!(benches);
