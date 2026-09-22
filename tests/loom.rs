//! Loom permutation tests.
//!
//! These tests run the *actual* `ArcSwapAny` implementation (not a rewritten
//! model of it) under `loom`, which enumerates the possible interleavings of
//! the atomic operations. The library is compiled with `--cfg loom`, which
//! makes it use loom's checked atomics through the thin shim in
//! `src/sync.rs`; the algorithm and its state transitions are identical to
//! the production build, only the interleavings are enumerated. For the same
//! reason the payloads are held in `loom::sync::Arc` (for which the crate
//! provides a `RefCnt` implementation under `--cfg loom`): every piece of
//! shared state inside a model must be loom-checked for the enumeration to
//! be meaningful.
//!
//! Each model uses payloads with unique generations and destructor counters
//! and asserts the happens-before invariants of the crate in *every*
//! enumerated interleaving:
//!
//! * a generation held by a `Guard` or `Arc` is never destructed,
//! * every generation is destructed exactly once in the end.
//!
//! Run with:
//!
//! ```sh
//! RUSTFLAGS="--cfg loom" cargo test --test loom
//! ```
//!
//! The models are intentionally tiny (2-3 threads, a handful of operations,
//! and the fast slot pool is shrunk to 2 under `--cfg loom`) to keep the
//! number of permutations tractable. `LOOM_MAX_PREEMPTIONS` can be set to
//! trade coverage for runtime.

#![cfg(loom)]

use std::sync::Arc;
use std::vec::Vec;

use arc_swap::{ArcSwapAny, DefaultStrategy};
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;

/// The Arc used inside the models: loom's checked one.
type LoomArc<T> = loom::sync::Arc<T>;
/// The ArcSwap instantiation under test.
type LoomArcSwap<T> = ArcSwapAny<LoomArc<T>, DefaultStrategy>;
type LoomArcSwapOption<T> = ArcSwapAny<Option<LoomArc<T>>, DefaultStrategy>;

/// Upper bound of generations created inside one model.
const GENS: usize = 8;

/// A payload with a unique generation and a destructor counter.
struct Counted {
    gen: usize,
    drops: Arc<Vec<AtomicUsize>>,
}

impl Drop for Counted {
    fn drop(&mut self) {
        eprintln!("DEBUG drop gen={} addr={:p}", self.gen, self);
        self.drops[self.gen].fetch_add(1, Ordering::SeqCst);
    }
}

/// Per-model environment: fresh for every permutation loom runs.
#[derive(Clone)]
struct Env {
    drops: Arc<Vec<AtomicUsize>>,
    next: Arc<AtomicUsize>,
}

impl Env {
    fn new() -> Self {
        Env {
            drops: Arc::new((0..GENS).map(|_| AtomicUsize::new(0)).collect()),
            next: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn payload(&self) -> LoomArc<Counted> {
        let gen = self.next.fetch_add(1, Ordering::SeqCst);
        assert!(gen < GENS, "model created too many generations");
        let arc = LoomArc::new(Counted {
            gen,
            drops: Arc::clone(&self.drops),
        });
        eprintln!("DEBUG new gen={} addr={:p}", gen, &*arc);
        arc
    }

    fn drops(&self, gen: usize) -> usize {
        self.drops[gen].load(Ordering::SeqCst)
    }

    /// Final invariant: every generation was destructed exactly once.
    fn check(&self) {
        let created = self.next.load(Ordering::SeqCst);
        for gen in 0..created {
            assert_eq!(
                1,
                self.drops(gen),
                "generation {} destructed a wrong number of times",
                gen
            );
        }
    }
}

/// A writer stores a new generation while a reader loads and holds a guard.
#[test]
fn store_vs_load() {
    loom::model(|| {
        let env = Env::new();
        let shared = Arc::new(LoomArcSwap::from(env.payload()));

        let writer = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                eprintln!("DEBUG writer {:?} storing", thread::current().id());
                shared.store(env.payload());
                eprintln!("DEBUG writer done");
            })
        };
        let reader = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                let guard = shared.load();
                eprintln!("DEBUG reader {:?} loaded gen {}", thread::current().id(), guard.gen);
                // Whatever generation we hold, it must not be destructed
                // while we hold it.
                assert_eq!(0, env.drops(guard.gen), "held generation destructed");
            })
        };
        writer.join().unwrap();
        reader.join().unwrap();
        eprintln!("DEBUG main {:?} dropping shared", thread::current().id());
        drop(shared);
        eprintln!("DEBUG main dropped shared");
        env.check();
    });
}

/// A reader holds a guard across two swaps of the writer. The debt of the
/// reader is paid by the writer, but the generation may only be destructed
/// after the guard is released.
#[test]
fn held_guard_across_swaps() {
    loom::model(|| {
        let env = Env::new();
        let shared = Arc::new(LoomArcSwap::from(env.payload()));

        let writer = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                let old1 = shared.swap(env.payload());
                let old2 = shared.swap(env.payload());
                drop(old1);
                drop(old2);
            })
        };
        let reader = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                let guard = shared.load();
                let gen = guard.gen;
                // Give the writer a chance to run both swaps in between.
                thread::yield_now();
                assert_eq!(0, env.drops(gen), "held generation destructed");
                drop(guard);
            })
        };
        writer.join().unwrap();
        reader.join().unwrap();
        drop(shared);
        env.check();
    });
}

/// compare_and_swap racing with loads.
#[test]
fn cas_vs_load() {
    loom::model(|| {
        let env = Env::new();
        let shared = Arc::new(LoomArcSwap::from(env.payload()));

        let cas = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                let current = shared.load_full();
                let prev = shared.compare_and_swap(&current, env.payload());
                drop(prev);
                drop(current);
            })
        };
        let reader = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                let one = shared.load_full();
                let two = shared.load_full();
                drop(one);
                drop(two);
            })
        };
        cas.join().unwrap();
        reader.join().unwrap();
        drop(shared);
        env.check();
    });
}

/// Storing `None` into an `ArcSwapOption`-like slot racing with loads.
#[test]
fn option_none() {
    loom::model(|| {
        let env = Env::new();
        let shared = Arc::new(LoomArcSwapOption::from(Some(env.payload())));

        let writer = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                shared.store(None);
            })
        };
        let reader = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                let guard = shared.load();
                if let Some(payload) = &*guard {
                    assert_eq!(0, env.drops(payload.gen), "held generation destructed");
                }
            })
        };
        writer.join().unwrap();
        reader.join().unwrap();
        drop(shared);
        env.check();
    });
}

/// A reader holding more guards than there are fast slots (the pool is
/// shrunk to 2 under `--cfg loom`) while a writer stores. This forces the
/// helping fallback strategy and the writer's helping/pay_all paths to be
/// enumerated.
#[test]
fn slot_exhaustion_fallback() {
    loom::model(|| {
        let env = Env::new();
        let shared = Arc::new(LoomArcSwap::from(env.payload()));

        let writer = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                shared.store(env.payload());
            })
        };
        let reader = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                // One more than the (loom-shrunk) fast slot pool.
                let guards: Vec<_> = (0..3).map(|_| shared.load()).collect();
                for guard in &guards {
                    assert_eq!(0, env.drops(guard.gen), "held generation destructed");
                }
                drop(guards);
            })
        };
        writer.join().unwrap();
        reader.join().unwrap();
        drop(shared);
        env.check();
    });
}

/// Two writers publishing different generations while a reader loads.
#[test]
fn two_writers() {
    loom::model(|| {
        let env = Env::new();
        let shared = Arc::new(LoomArcSwap::from(env.payload()));

        let mut writers = Vec::new();
        for _ in 0..2 {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            writers.push(thread::spawn(move || {
                shared.store(env.payload());
            }));
        }
        let reader = {
            let shared = Arc::clone(&shared);
            let env = env.clone();
            thread::spawn(move || {
                let guard = shared.load();
                assert_eq!(0, env.drops(guard.gen), "held generation destructed");
            })
        };
        for writer in writers {
            writer.join().unwrap();
        }
        reader.join().unwrap();
        drop(shared);
        env.check();
    });
}
