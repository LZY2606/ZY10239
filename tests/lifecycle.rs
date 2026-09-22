//! Targeted lifecycle and controlled-interleaving tests.
//!
//! These do not rely on running "a million iterations without crashing".
//! Each test forces a specific interleaving with barriers (or by
//! construction) and asserts the destructor invariants at every step:
//!
//! * a panic unwinding a thread that still holds a `Guard`,
//! * `ArcSwapOption` transitions through `None`,
//! * `Weak` pointers whose upgrade fails after the value died,
//! * a `Cache` replacing its old generation with a newer one,
//! * a barrier-forced debt-repayment order, where the happens-before
//!   relation between guard drop and destruction is checked deterministically.

mod common;

use std::sync::{Arc, Barrier};

use arc_swap::{ArcSwap, ArcSwapOption, Cache};

use common::{Payload, Registry};

/// A thread panics while holding a guard. The unwinding must release the
/// debt, the value must be destructed exactly once, and the shared slot must
/// keep working for other threads (the panicked thread's debt node goes
/// through cooldown and can be reused).
#[test]
fn panic_while_holding_guard() {
    let registry = Arc::new(Registry::new(16));
    let shared = Arc::new(ArcSwap::from(Payload::new(&registry)));
    let held_gen = shared.load().gen;

    let panicked = {
        let shared = Arc::clone(&shared);
        let registry = Arc::clone(&registry);
        std::thread::spawn(move || {
            let guard = shared.load();
            registry.hold(guard.gen);
            let gen = guard.gen;
            // Panic with the guard still alive. The guard is dropped during
            // unwinding. We can't release the model mark from inside the
            // unwinding, so do it in a drop guard of our own.
            struct ReleaseOnUnwind<'a> {
                registry: &'a Registry,
                gen: usize,
            }
            impl Drop for ReleaseOnUnwind<'_> {
                fn drop(&mut self) {
                    self.registry.release(self.gen);
                }
            }
            let _release = ReleaseOnUnwind {
                registry: &registry,
                gen,
            };
            panic!("intentional panic while holding a guard of gen {}", gen);
        })
    };
    assert!(panicked.join().is_err(), "the thread was supposed to panic");
    // The guard was dropped during the unwind; the initial value is still
    // the current one, so it must not be destructed.
    assert_eq!(0, registry.drops(held_gen), "current value destructed");

    // The shared slot is still fully functional after the panic.
    let new = Payload::new(&registry);
    let new_gen = new.gen;
    shared.store(new);
    assert_eq!(1, registry.drops(held_gen), "old value not destructed once");
    assert_eq!(0, registry.drops(new_gen), "new value destructed");

    // A fresh thread can load without problems (debt node reuse after the
    // panicked thread's node went through cooldown).
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            for _ in 0..16 {
                let guard = shared.load();
                tx.send(guard.gen).unwrap();
            }
        })
        .join()
        .unwrap();
    }
    for _ in 0..16 {
        assert_eq!(new_gen, rx.recv().unwrap());
    }

    drop(shared);
    registry.check_all_dropped_once("panic_while_holding_guard");
}

/// `ArcSwapOption` transitions between `Some` and `None` under concurrency;
/// every `Some` generation must be destructed exactly once and `None` loads
/// must never observe a destructed generation.
#[test]
fn option_none_transitions() {
    #[cfg(not(miri))]
    const OPS: usize = 200;
    #[cfg(miri)]
    const OPS: usize = 10;

    let registry = Arc::new(Registry::new(OPS + 4));
    let shared = ArcSwapOption::<Payload>::from(None);

    {
        let shared = &shared;
        let registry = &registry;
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                for _ in 0..OPS {
                    shared.store(Some(Payload::new(registry)));
                    shared.store(None);
                }
            });
            let reader = scope.spawn(move || {
                for _ in 0..OPS {
                    let guard = shared.load();
                    if let Some(payload) = &*guard {
                        // A live guard: the generation must not be destructed.
                        assert_eq!(
                            0,
                            registry.drops(payload.gen),
                            "load returned a destructed generation {}",
                            payload.gen
                        );
                    }
                }
            });
            writer.join().unwrap();
            reader.join().unwrap();
        })
    }

    shared.store(None);
    drop(shared);
    registry.check_all_dropped_once("option_none_transitions");
}

/// Weak pointers: once the last strong reference is gone, the value is
/// destructed exactly once and upgrades from the shared slot fail.
#[cfg(feature = "weak")]
#[test]
fn weak_upgrade_failure() {
    let registry = Arc::new(Registry::new(16));
    let shared = arc_swap::ArcSwapWeak::default();

    let strong = Payload::new(&registry);
    let gen = strong.gen;
    shared.store(Arc::downgrade(&strong));
    assert!(shared.load().upgrade().is_some());

    drop(strong);
    // The weak inside does not keep the value alive.
    assert_eq!(1, registry.drops(gen), "weak kept the value alive");
    assert!(
        shared.load().upgrade().is_none(),
        "upgrade succeeded after the value died"
    );

    // Replace the dead weak with a live one and back to an empty weak.
    let strong = Payload::new(&registry);
    let gen2 = strong.gen;
    shared.store(Arc::downgrade(&strong));
    assert!(shared.load().upgrade().is_some());
    shared.store(Default::default());
    drop(strong);
    assert_eq!(1, registry.drops(gen2), "second value not destructed once");
    assert!(shared.load().upgrade().is_none());

    drop(shared);
    registry.check_all_dropped_once("weak_upgrade_failure");
}

/// A cache holds on to its generation; when a newer generation replaces it
/// during revalidation, the old one is released and destructed exactly once.
#[test]
fn cache_replaces_old_generation() {
    let registry = Arc::new(Registry::new(16));
    let shared = ArcSwap::from(Payload::new(&registry));
    let gen1 = shared.load().gen;

    let mut cache = Cache::new(&shared);
    assert_eq!(gen1, cache.load().gen);

    let replacement = Payload::new(&registry);
    let gen2 = replacement.gen;
    shared.store(replacement);
    // The cache still holds the old generation alive.
    assert_eq!(0, registry.drops(gen1), "cache-held generation destructed");

    // Revalidation replaces the cached (old) generation with the new one.
    assert_eq!(gen2, cache.load().gen);
    assert_eq!(
        1,
        registry.drops(gen1),
        "replaced cached generation not destructed"
    );
    assert_eq!(0, registry.drops(gen2), "cached generation destructed");

    drop(cache);
    drop(shared);
    registry.check_all_dropped_once("cache_replaces_old_generation");
}

/// A barrier-forced interleaving checking the happens-before chain of debt
/// repayment and destruction, deterministically:
///
/// 1. Thread A loads and holds a guard of gen0.
/// 2. The main thread swaps twice (gen0 -> gen1 -> gen2), paying A's debt.
///    gen0 must still be alive, because A's guard protects it.
/// 3. A drops the guard. Only then may gen0 be destructed, and the
///    destruction must be visible to the main thread after the barrier.
#[test]
fn debt_repayment_happens_before() {
    #[cfg(not(miri))]
    const ITERS: usize = 50;
    #[cfg(miri)]
    const ITERS: usize = 3;

    for iter in 0..ITERS {
        let ctx = || format!("debt_repayment_happens_before, iteration {}", iter);
        let registry = Arc::new(Registry::new(8));
        let shared = ArcSwap::from(Payload::new(&registry));
        let gen0 = shared.load().gen;
        // One barrier per direction and step; both sides rendezvous.
        let barrier = Barrier::new(2);

        let (reader, gen2) = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let guard = shared.load();
                let gen = guard.gen;
                registry.hold(gen);
                // Step 1 done: we hold the guard.
                barrier.wait();
                // Main swapped twice. Our guard still protects gen0.
                barrier.wait();
                assert_eq!(
                    0,
                    registry.drops(gen0),
                    "{}: gen0 destructed while a guard holds it",
                    ctx()
                );
                registry.release(gen);
                drop(guard);
                // Guard dropped; let main observe the destruction.
                barrier.wait();
                gen
            });
            // Step 1: reader holds the guard.
            barrier.wait();
            let replacement1 = Payload::new(&registry);
            let gen1 = replacement1.gen;
            shared.store(replacement1);
            let replacement2 = Payload::new(&registry);
            let gen2 = replacement2.gen;
            shared.store(replacement2);
            // The debt of the reader was paid (possibly), but the guard
            // still protects gen0 from being destructed.
            assert_eq!(
                0,
                registry.drops(gen0),
                "{}: gen0 destructed while a guard holds it",
                ctx()
            );
            assert_eq!(1, registry.drops(gen1), "{}: gen1 not destructed", ctx());
            // Step 2: let the reader check and drop its guard.
            barrier.wait();
            // Step 3: the reader dropped the guard; the destruction of gen0
            // must be visible to us now (the barrier synchronizes us).
            barrier.wait();
            assert_eq!(
                1,
                registry.drops(gen0),
                "{}: gen0 not destructed after the last guard was dropped",
                ctx()
            );
            (reader.join().unwrap(), gen2)
        });
        assert_eq!(gen0, reader);
        assert_eq!(0, registry.drops(gen2), "{}: gen2 destructed", ctx());

        drop(shared);
        registry.check_all_dropped_once(&ctx());
    }
}
