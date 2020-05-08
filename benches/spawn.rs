use criterion::{black_box, criterion_group, criterion_main, Criterion};
use futures::future;
use smol::Task;

pub fn spawn_benchmark(c: &mut Criterion) {
    std::thread::spawn(|| smol::run(future::pending::<()>()));
    c.bench_function("spawn time", |b| {
        b.iter(|| {
            let x = black_box(5);
            smol::block_on(async {
                Task::spawn(async move {
                    let _ = x + 1;
                })
                .await;
            });
        })
    });
}

criterion_group!(benches, spawn_benchmark);
criterion_main!(benches);
