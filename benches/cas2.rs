#![allow(unused_imports)]
#![allow(dead_code)]

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use crossbeam_epoch::{self, pin, unprotected, Owned};
use mw_cas::{cas2, Atomic, AtomicUsize};
use rand::prelude::SliceRandom;
use rand::rngs::SmallRng;
use rand::{thread_rng, Rng, SeedableRng};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn cas2_sum(
    atoms: Arc<Vec<AtomicUsize>>,
    threads: usize,
    per_thread_attempts: usize,
) -> Vec<AtomicUsize> {
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let mut num_succeeded = 0;
        let atoms = atoms.clone();
        let h = std::thread::spawn(move || {
            let mut thread_rng = thread_rng();
            let mut rng = SmallRng::from_rng(&mut thread_rng).unwrap();
            unsafe {
                for _ in 0..per_thread_attempts {
                    let first = atoms.choose(&mut rng).unwrap();
                    let second = atoms.choose(&mut rng).unwrap();
                    let first_current = first.load();
                    let second_current = second.load();
                    let success = cas2(
                        first,
                        second,
                        first_current,
                        second_current,
                        first_current + 1,
                        second_current + 1,
                    );
                    if success {
                        num_succeeded += 1;
                    }
                }
            }

            num_succeeded
        });

        handles.push(h);
    }

    let total_succeeded: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let (sum, ret) = match Arc::try_unwrap(atoms) {
        Ok(atoms) => {
            let sum: u64 = atoms.iter().map(|e| e.load() as u64).sum();
            (sum, atoms)
        }
        Err(_) => panic!(),
    };
    assert_ne!(total_succeeded, 0);
    assert_eq!(total_succeeded * 2, sum);
    ret
}

fn cas2_sum_alloc(
    atoms: Arc<Vec<Atomic<u64>>>,
    threads: usize,
    per_thread_attempts: usize,
) -> Vec<Atomic<u64>> {
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let mut num_succeeded = 0;
        let atoms = atoms.clone();
        let h = std::thread::spawn(move || {
            let mut thread_rng = thread_rng();
            let mut rng = SmallRng::from_rng(&mut thread_rng).unwrap();

            unsafe {
                for _ in 0..per_thread_attempts {
                    let first = atoms.choose(&mut rng).unwrap();
                    let second = atoms.choose(&mut rng).unwrap();

                    let g = pin();
                    let first_current = first.load(&g);
                    let second_current = second.load(&g);
                    let new_first = Owned::new(*first_current.deref() + 1).into_shared(&g);
                    let new_second = Owned::new(*second_current.deref() + 1).into_shared(&g);
                    let success = cas2(
                        first,
                        second,
                        first_current,
                        second_current,
                        new_first,
                        new_second,
                    );
                    if success {
                        num_succeeded += 1;
                        g.defer_destroy(first_current);
                        g.defer_destroy(second_current);
                    } else {
                        let _ = new_first.into_owned();
                        let _ = new_second.into_owned();
                    }
                }
            }

            num_succeeded
        });

        handles.push(h);
    }

    let total_succeeded: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let (sum, ret) = match Arc::try_unwrap(atoms) {
        Ok(atoms) => unsafe {
            let g = unprotected();
            let sum: u64 = atoms.iter().map(|e| *e.load(&g).deref()).sum();
            (sum, atoms)
        },
        Err(_) => panic!(),
    };
    assert_ne!(total_succeeded, 0);
    assert_eq!(total_succeeded * 2, sum);
    ret
}

fn cas2_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("cas2");
    let threads = 24;
    let per_thread_attempts = 200_000;
    group.throughput(Throughput::Elements(threads * per_thread_attempts));

    group.bench_function("cas2_sum_alloc", |b| {
        b.iter_batched(
            || Arc::new((0..10).map(|_| Atomic::new(0)).collect::<Vec<_>>()),
            |atoms| cas2_sum_alloc(atoms, threads as usize, per_thread_attempts as usize),
            BatchSize::SmallInput,
        )
    });

    group.bench_function("cas2_sum", |b| {
        b.iter_batched(
            || Arc::new((0..8000).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>()),
            |atoms| cas2_sum(atoms, threads as usize, per_thread_attempts as usize),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(benches, cas2_benchmark);
criterion_main!(benches);
