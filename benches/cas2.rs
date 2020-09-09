use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use crossbeam_epoch;
use mw_cas::mcas::{cas2, Atomic};
use rand::{Rng, SeedableRng};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const ITER: u64 = 24 * 200_000;

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
                if cas2(
                    &atomics[0],
                    &atomics[1],
                    first,
                    second,
                    new_first,
                    new_second,
                ) {
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

fn cas2_random(atomics: Arc<Box<[Atomic<u32>]>>, threads: usize) -> Box<[Atomic<u32>]> {
    let mut handles = Vec::new();
    let per_thread = ITER / threads as u64;
    for thread in 0..threads {
        let atomics = atomics.clone();
        let h = std::thread::spawn(move || {
            let s = thread as u32;
            let seed = [s + 1, s + 2, s + 3, s + 4];
            let mut rng = rand::XorShiftRng::from_seed(seed);
            let g = crossbeam_epoch::pin();
            let new_first = crossbeam_epoch::Owned::new(thread as u32).into_shared(&g);
            let new_second = crossbeam_epoch::Owned::new(thread as u32).into_shared(&g);
            let mut num_succeeded = 0;
            for _ in 0..per_thread {
                let first = rng.choose(&*atomics).unwrap();
                let first_current = first.load(&g);

                let second = rng.choose(&*atomics).unwrap();
                let second_current = second.load(&g);

                if cas2(
                    first,
                    second,
                    first_current,
                    second_current,
                    new_first,
                    new_second,
                ) {
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

fn cas_attemts(atomics: Arc<[AtomicPtr<u32>; 2]>, threads: usize) -> [AtomicPtr<u32>; 2] {
    let per_thread = ITER / threads as u64;
    let mut handles = Vec::new();
    for thread in 0..threads {
        let atoms = atomics.clone();
        let h = std::thread::spawn(move || {
            let desired_first = Box::into_raw(Box::new(thread as u32));
            let desired_second = Box::into_raw(Box::new(thread as u32));
            for _ in 0..per_thread {
                let curr_first = atoms[0].load(Ordering::SeqCst);
                let curr_second = atoms[0].load(Ordering::SeqCst);
                atoms[0].compare_exchange(
                    curr_first,
                    desired_first,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                atoms[1].compare_exchange(
                    curr_second,
                    desired_second,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            }
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
    group.throughput(Throughput::Elements(ITER as u64));

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

    group.bench_function("cas2_random", |b| {
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
                let m = cas2_random(map, 24);
                m
            },
            BatchSize::SmallInput,
        )
    });

    /*group.bench_function("native_cas", |b| {
        b.iter_batched(
            || Arc::new([AtomicPtr::default(), AtomicPtr::default()]),
            |atom| {
                let m = cas_attemts(atom, 24);
                m
            },
            BatchSize::SmallInput,
        )
    });*/
    group.finish();
}

criterion_group!(benches, cas2_benchmark);
criterion_main!(benches);
