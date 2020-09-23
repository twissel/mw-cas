#![allow(unused_imports)]
#![allow(dead_code)]

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use crossbeam_epoch::{self, Owned};
use mw_cas::{cas2, Atomic};
use rand::{Rng, SeedableRng};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const ITER: u64 = 24 * 100_000;

fn cas2_attemts(atomics: Arc<[Atomic<u32>; 2]>, threads: usize) -> [Atomic<u32>; 2] {
    let mut handles = Vec::new();
    let per_thread = ITER / threads as u64;
    for thread in 0..threads {
        let atomics = atomics.clone();
        let h = std::thread::spawn(move || {
            let g = crossbeam_epoch::pin();
            let new_first = crossbeam_epoch::Owned::new(thread as u32).into_shared(&g);
            let new_second = crossbeam_epoch::Owned::new(thread as u32).into_shared(&g);
            let mut num_succeeded = 0;
            for _ in 0..per_thread {
                let first = atomics[0].load(&g);
                let second = atomics[1].load(&g);
                if unsafe {
                    cas2(
                        &atomics[0],
                        &atomics[1],
                        first,
                        second,
                        new_first,
                        new_second,
                    )
                } {
                    num_succeeded += 1;
                }
            }
            num_succeeded
        });

        handles.push(h);
    }

    let mut _total_succeed = 0;
    for h in handles {
        _total_succeed += h.join().unwrap();
    }

    //dbg!(_total_succeed);

    match Arc::try_unwrap(atomics) {
        Ok(a) => a,
        Err(_) => panic!("failed to unwrap"),
    }
}

fn cas2_array_alloc(
    atomics: Arc<Box<[Atomic<u32>]>>,
    els: Vec<Vec<Owned<u32>>>,
) -> Box<[Atomic<u32>]> {
    let mut handles = Vec::new();
    for (thread, mut els) in els.into_iter().enumerate() {
        let atomics = atomics.clone();
        let h = std::thread::spawn(move || {
            let s = thread as u32;
            let seed = [s + 1, s + 2, s + 3, s + 4];
            let mut rng = rand::XorShiftRng::from_seed(seed);
            let mut num_succeeded = 0;
            loop {
                let g = crossbeam_epoch::pin();
                let new = els.pop().and_then(|f| els.pop().map(|s| (f, s)));
                let (new_first, new_second) = match new {
                    Some((f, s)) => (f.into_shared(&g), s.into_shared(&g)),
                    None => break,
                };

                unsafe {
                    let first = rng.choose(&*atomics).unwrap();
                    let first_current_shared = first.load(&g);

                    let second = rng.choose(&*atomics).unwrap();
                    let second_current_shared = second.load(&g);

                    if cas2(
                        first,
                        second,
                        first_current_shared,
                        second_current_shared,
                        new_first,
                        new_second,
                    ) {
                        g.defer_destroy(first_current_shared);
                        g.defer_destroy(second_current_shared);
                        num_succeeded += 1;
                    } else {
                        new_first.into_owned();
                        new_second.into_owned();
                    }
                }
            }
            num_succeeded
        });

        handles.push(h);
    }

    let mut total_succeeded = 0;
    for h in handles {
        total_succeeded += h.join().unwrap();
    }
    assert!(total_succeeded > 0);
    let r = match Arc::try_unwrap(atomics) {
        Ok(a) => a,
        Err(_) => panic!("failed to unwrap"),
    };
    r
}

fn cas2_array(atomics: Arc<Box<[Atomic<u32>]>>, threads: usize) -> Box<[Atomic<u32>]> {
    let mut handles = Vec::new();
    let per_thread = ITER / threads as u64;
    for thread in 0..threads {
        let atomics = atomics.clone();
        let h = std::thread::spawn(move || {
            let s = thread as u32;
            let seed = [s + 1, s + 2, s + 3, s + 4];
            let g = crossbeam_epoch::pin();
            let mut rng = rand::XorShiftRng::from_seed(seed);
            let new_first = crossbeam_epoch::Owned::new(thread as u32).into_shared(&g);
            let new_second = crossbeam_epoch::Owned::new(thread as u32).into_shared(&g);
            let mut num_succeeded = 0;
            for _ in 0..per_thread {
                let g = crossbeam_epoch::pin();
                let first = rng.choose(&*atomics).unwrap();
                let first_current = first.load(&g);

                let second = rng.choose(&*atomics).unwrap();
                let second_current = second.load(&g);

                if unsafe {
                    cas2(
                        first,
                        second,
                        first_current,
                        second_current,
                        new_first,
                        new_second,
                    )
                } {
                    num_succeeded += 1;
                }
            }
            num_succeeded
        });

        handles.push(h);
    }

    for h in handles {
        h.join().unwrap();
    }

    match Arc::try_unwrap(atomics) {
        Ok(a) => a,
        Err(_) => panic!("failed to unwrap"),
    }
}

fn cas2_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("cas2");
    let num_threads = 24;
    let per_thread = 50000;
    group.throughput(Throughput::Elements(num_threads * per_thread));
    group.bench_function("cas2_sum", |b| {
        b.iter_batched(
            || {
                Arc::new(
                    (0..24000)
                        .map(|_| Atomic::new(0))
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                )
            },
            |map| {
                let m = cas2_sum(map, num_threads as usize, per_thread as usize);
                m
            },
            BatchSize::LargeInput,
        )
    });

    /*group.bench_function("cas2", |b| {
        b.iter_batched(
            || Arc::new([Atomic::new(0), Atomic::new(0)]),
            |map| {
                let m = cas2_attemts(map, 24);
                m
            },
            BatchSize::SmallInput,
        )
    });*/

    /*group.bench_function("cas2_array", |b| {
        b.iter_batched(
            || {
                Arc::new(
                    (0..24000)
                        .map(|_| Atomic::new(0))
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                )
            },
            |map| {
                let m = cas2_array(map, 24);
                m
            },
            BatchSize::SmallInput,
        )
    });

    group.bench_function("cas2_prealloc_array", |b| {
        b.iter_batched(
            || {
                let atoms = Arc::new(
                    (0..24000)
                        .map(|_| Atomic::new(0))
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                );
                let els = (0..24)
                    .map(|_| (0..50000).map(|e| Owned::new(e)).collect())
                    .collect();
                (atoms, els)
            },
            |(atoms, els)| cas2_array_alloc(atoms, els),
            BatchSize::SmallInput,
        )
    });*/

    group.finish();
}

fn cas2_sum(
    atomics: Arc<Box<[Atomic<u32>]>>,
    threads: usize,
    per_thread: usize,
) -> Box<[Atomic<u32>]> {
    let mut handles = Vec::new();
    for thread in 0..threads {
        let atomics = atomics.clone();
        let h = std::thread::spawn(move || {
            let s = thread as u32;
            let seed = [s + 1, s + 2, s + 3, s + 4];
            let mut rng = rand::XorShiftRng::from_seed(seed);
            let mut num_succeeded = 0;
            for _ in 0..per_thread {
                unsafe {
                    let g = crossbeam_epoch::pin();
                    let first = rng.choose(&*atomics).unwrap();
                    let first_current = first.load(&g);
                    let new_first = Atomic::new(*first_current.deref() + 1)
                        .into_owned()
                        .into_shared(&g);

                    let second = rng.choose(&*atomics).unwrap();
                    let second_current = second.load(&g);
                    let new_second = Atomic::new(*second_current.deref() + 1)
                        .into_owned()
                        .into_shared(&g);
                    let succeeded = cas2(
                        first,
                        second,
                        first_current,
                        second_current,
                        new_first,
                        new_second,
                    );

                    if succeeded {
                        g.defer_destroy(first_current);
                        g.defer_destroy(second_current);
                        num_succeeded += 1;
                    } else {
                        new_first.into_owned();
                        new_second.into_owned();
                    }
                }
            }

            num_succeeded
        });

        handles.push(h);
    }

    let total_succeeded: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    match Arc::try_unwrap(atomics) {
        Ok(a) => {
            let g = crossbeam_epoch::pin();
            let sum: usize = a
                .iter()
                .map(|el| {
                    let v = unsafe { el.load(&g).deref() };
                    *v as usize
                })
                .sum();
            assert_eq!(sum, total_succeeded * 2);
            a
        }
        Err(_) => panic!("failed to unwrap"),
    }
}

criterion_group!(benches, cas2_benchmark);
criterion_main!(benches);
