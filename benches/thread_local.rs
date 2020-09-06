use criterion::{black_box, Criterion, criterion_group, criterion_main};
use mw_cas::thread_local::{ThreadLocal, THREAD_ID};

fn thread_local_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("thread_local_get");
    let map = ThreadLocal::<usize>::new();
    let id = THREAD_ID.with(|id| *id);
    group.bench_function("thread_local_get", |bencher| {
        bencher.iter(|| black_box(map.get_for_thread(id)));
    });
    group.finish();
}

criterion_group!(
    benches,
    thread_local_get
);
criterion_main!(benches);

