//! A small concurrent state machine checked against a model.
//!
//! Several threads perform random sequences of actions -- `load`,
//! `load_full`, `Guard` clone/drop, `store`, `swap`, `compare_and_swap`,
//! `Cache` reads -- and the threads themselves are also created and destroyed
//! to cover the thread-exit paths. Every value carries a unique generation
//! and a destructor counter (see `tests/common`).
//!
//! The model tracks the set of reachable generations and asserts, at
//! quiescent checkpoints (barriers where no thread is inside an operation):
//!
//! * no generation is destructed while a `Guard` or `Arc` still holds it,
//! * no generation is destructed more than once,
//!
//! and, after everything is torn down:
//!
//! * every generation that became unreachable was destructed exactly once.
//!
//! The same state machine is run against all the protection strategies
//! available in the build configuration, so strategy-specific behaviour
//! (including the fallback-heavy testing strategy) is covered by the same
//! model.
//!
//! The randomness is a deterministic xorshift; every failure message
//! contains the seed, so any failure can be reproduced exactly.

mod common;

use std::sync::{Arc, Barrier};

use arc_swap::strategy::{CaS, Strategy};
use arc_swap::{ArcSwapAny, Cache, Guard};

use common::{Payload, Registry, Rng};

#[cfg(not(miri))]
const ROUNDS: usize = 8;
#[cfg(not(miri))]
const THREADS: usize = 4;
#[cfg(not(miri))]
const OPS: usize = 150;

#[cfg(miri)]
const ROUNDS: usize = 2;
#[cfg(miri)]
const THREADS: usize = 2;
#[cfg(miri)]
const OPS: usize = 8;

/// More guards than the 8 fast debt slots, to push some loads into the
/// fallback strategy even without the test hooks.
const GUARD_CAP: usize = 12;
const ARC_CAP: usize = 4;

/// Runs the state machine against strategy `S` with the given seed.
fn run<S>(seed: u64)
where
    S: Default + Send + Sync,
    S: Strategy<Arc<Payload>> + CaS<Arc<Payload>>,
{
    let ctx = format!(
        "state machine {} (seed {seed:#x})",
        std::any::type_name::<S>()
    );
    // Every store/swap/compare_and_swap creates one payload per operation.
    let capacity = ROUNDS * THREADS * OPS + 64;
    let registry = Arc::new(Registry::new(capacity));
    let shared = ArcSwapAny::<Arc<Payload>, S>::from(Payload::new(&registry));
    // Threads pause at these barriers so the main thread can check the
    // invariants while nobody is inside an operation.
    let checkpoint = Barrier::new(THREADS + 1);
    let resume = Barrier::new(THREADS + 1);

    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let shared = &shared;
            let registry = &registry;
            let checkpoint = &checkpoint;
            let resume = &resume;
            scope.spawn(move || {
                let mut rng = Rng::new(seed.wrapping_add(thread as u64 + 1));
                let mut guards: Vec<Guard<Arc<Payload>, S>> = Vec::new();
                let mut arcs: Vec<Arc<Payload>> = Vec::new();
                let mut cache = Cache::new(shared);
                for _round in 0..ROUNDS {
                    for _op in 0..OPS {
                        match rng.below(12) {
                            // load, keep the guard around
                            0 | 1 => {
                                let guard = shared.load();
                                registry.hold(guard.gen);
                                guards.push(guard);
                                if guards.len() > GUARD_CAP {
                                    let old = guards.remove(0);
                                    registry.release(old.gen);
                                    drop(old);
                                }
                            }
                            // load_full, keep the Arc
                            2 => {
                                let arc = shared.load_full();
                                registry.hold(arc.gen);
                                arcs.push(arc);
                                if arcs.len() > ARC_CAP {
                                    let old = arcs.remove(0);
                                    registry.release(old.gen);
                                    drop(old);
                                }
                            }
                            // clone an Arc out of a guard
                            3 => {
                                if let Some(guard) = guards.last() {
                                    let arc: Arc<Payload> = Arc::clone(guard);
                                    registry.hold(arc.gen);
                                    arcs.push(arc);
                                }
                            }
                            // drop a guard
                            4 => {
                                if !guards.is_empty() {
                                    let idx = rng.below(guards.len() as u64) as usize;
                                    let old = guards.swap_remove(idx);
                                    registry.release(old.gen);
                                    drop(old);
                                }
                            }
                            // drop an Arc
                            5 => {
                                if !arcs.is_empty() {
                                    let idx = rng.below(arcs.len() as u64) as usize;
                                    let old = arcs.swap_remove(idx);
                                    registry.release(old.gen);
                                    drop(old);
                                }
                            }
                            // store a fresh generation
                            6 | 7 => {
                                shared.store(Payload::new(registry));
                            }
                            // swap, keeping the old value for a while
                            8 => {
                                let old = shared.swap(Payload::new(registry));
                                registry.hold(old.gen);
                                arcs.push(old);
                                if arcs.len() > ARC_CAP {
                                    let old = arcs.remove(0);
                                    registry.release(old.gen);
                                    drop(old);
                                }
                            }
                            // compare_and_swap against the currently seen value
                            9 => {
                                let current = shared.load_full();
                                registry.hold(current.gen);
                                let prev =
                                    shared.compare_and_swap(&current, Payload::new(registry));
                                registry.hold(prev.gen);
                                guards.push(prev);
                                registry.release(current.gen);
                                drop(current);
                                if guards.len() > GUARD_CAP {
                                    let old = guards.remove(0);
                                    registry.release(old.gen);
                                    drop(old);
                                }
                            }
                            // read through the cache; the cache keeps its own
                            // reference alive internally
                            10 => {
                                let _gen = cache.load().gen;
                            }
                            // load and drop right away
                            _ => {
                                let guard = shared.load();
                                let gen = guard.gen;
                                registry.hold(gen);
                                registry.release(gen);
                                drop(guard);
                            }
                        }
                    }
                    // Quiescent checkpoint: everyone is out of their
                    // operations, the main thread validates the model.
                    checkpoint.wait();
                    resume.wait();
                }
                // Thread exit: release everything. This also sends the
                // thread's debt node into cooldown, to be reused by others.
                for guard in guards.drain(..) {
                    registry.release(guard.gen);
                    drop(guard);
                }
                for arc in arcs.drain(..) {
                    registry.release(arc.gen);
                    drop(arc);
                }
                drop(cache);
            });
        }
        for round in 0..ROUNDS {
            checkpoint.wait();
            registry.check_quiescent(&format!("{ctx}, checkpoint of round {round}"));
            resume.wait();
        }
    });

    drop(shared);
    registry.check_all_dropped_once(&ctx);
}

/// Short-lived threads coming and going, each doing a handful of operations.
///
/// This exercises the thread-exit path: debt nodes are sent into cooldown
/// and reused by later threads, while writers traverse nodes in every
/// ownership state.
fn thread_exit_churn<S>(seed: u64)
where
    S: Default + Send + Sync,
    S: Strategy<Arc<Payload>> + CaS<Arc<Payload>>,
{
    let ctx = format!(
        "thread exit churn {} (seed {seed:#x})",
        std::any::type_name::<S>()
    );
    #[cfg(not(miri))]
    const WAVES: usize = 20;
    #[cfg(miri)]
    const WAVES: usize = 2;
    let capacity = WAVES * THREADS * 8 + 64;
    let registry = Arc::new(Registry::new(capacity));
    let shared = ArcSwapAny::<Arc<Payload>, S>::from(Payload::new(&registry));
    for wave in 0..WAVES {
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let shared = &shared;
                let registry = &registry;
                scope.spawn(move || {
                    let mut rng = Rng::new(seed ^ (wave * THREADS + thread) as u64);
                    // Each thread holds a few guards (sometimes overflowing
                    // the fast slots), then exits with them still alive.
                    let mut guards = Vec::new();
                    for _ in 0..4 {
                        let guard = shared.load();
                        registry.hold(guard.gen);
                        guards.push(guard);
                    }
                    if rng.below(2) == 0 {
                        shared.store(Payload::new(registry));
                    } else {
                        let current = shared.load_full();
                        registry.hold(current.gen);
                        let prev = shared.compare_and_swap(&current, Payload::new(registry));
                        registry.hold(prev.gen);
                        guards.push(prev);
                        registry.release(current.gen);
                        drop(current);
                    }
                    for guard in guards.drain(..) {
                        registry.release(guard.gen);
                        drop(guard);
                    }
                });
            }
        });
        registry.check_quiescent(&format!("{ctx}, wave {wave}"));
    }
    drop(shared);
    registry.check_all_dropped_once(&ctx);
}

macro_rules! state_machine_tests {
    ($mod_name:ident, $strategy:ty) => {
        mod $mod_name {
            use super::*;

            #[allow(deprecated)] // internal testing strategies
            type S = $strategy;

            #[test]
            fn randomized_state_machine() {
                run::<S>(0xA11C_E5EED_0001);
                run::<S>(0xA11C_E5EED_0002);
            }

            #[test]
            fn thread_exit_churn() {
                super::thread_exit_churn::<S>(0xC0FF_EE00_0042);
            }
        }
    };
}

state_machine_tests!(default_strategy, arc_swap::DefaultStrategy);

#[cfg(feature = "internal-test-strategies")]
state_machine_tests!(
    fallback_heavy,
    arc_swap::strategy::test_strategies::FillFastSlots
);

// The RwLock strategy needs std, so it is not available with
// experimental-thread-local.
#[cfg(all(
    feature = "internal-test-strategies",
    not(feature = "experimental-thread-local")
))]
state_machine_tests!(rw_lock, std::sync::RwLock<()>);
