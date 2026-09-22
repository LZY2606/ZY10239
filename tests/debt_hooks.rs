//! Tests for the `internal-test-hooks` feature.
//!
//! These verify that the instrumentation actually observes the interesting
//! internal paths of the debt machinery:
//!
//! * the fast slot path,
//! * slot exhaustion (forced deterministically with a tiny slot pool),
//! * the helping/fallback strategy,
//! * the writer-side `pay_all` sweep and individual debt payments.
//!
//! The hooks are compiled in only with the feature enabled; without it none
//! of this code (or its cost) exists.

#![cfg(feature = "internal-test-hooks")]

mod common;

use std::sync::{Arc, Barrier, Mutex};

use arc_swap::test_hooks::{self, DebtStats};
use arc_swap::ArcSwap;

use common::{Payload, Registry};

/// The counters are process-global, so the tests in here must not run
/// concurrently with each other.
static SERIAL: Mutex<()> = Mutex::new(());

fn with_hooks<F: FnOnce()>(f: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    test_hooks::set_fast_slot_limit(usize::MAX);
    test_hooks::reset();
    f();
    test_hooks::set_fast_slot_limit(usize::MAX);
}

/// A plain load goes through a fast slot.
#[test]
fn fast_path_is_used() {
    with_hooks(|| {
        let registry = Arc::new(Registry::new(4));
        let shared = ArcSwap::from(Payload::new(&registry));
        let guard = shared.load();
        let stats = test_hooks::snapshot();
        assert!(
            stats.fast_acquired >= 1,
            "expected the fast path to be used, got {:?}",
            stats
        );
        assert_eq!(0, stats.helping_used, "unexpected fallback: {:?}", stats);
        drop(guard);
        drop(shared);
        registry.check_all_dropped_once("fast_path_is_used");
    });
}

/// Shrinking the fast slot pool to zero forces every load into the
/// helping/fallback strategy.
#[test]
fn slot_exhaustion_forces_fallback() {
    with_hooks(|| {
        let registry = Arc::new(Registry::new(4));
        let shared = ArcSwap::from(Payload::new(&registry));
        test_hooks::set_fast_slot_limit(0);
        let guard = shared.load();
        let stats = test_hooks::snapshot();
        assert!(
            stats.fast_exhausted >= 1,
            "expected slot exhaustion to be reported, got {:?}",
            stats
        );
        assert!(
            stats.helping_used >= 1,
            "expected the fallback strategy to be used, got {:?}",
            stats
        );
        drop(guard);
        drop(shared);
        registry.check_all_dropped_once("slot_exhaustion_forces_fallback");
    });
}

/// A tiny (but non-zero) slot pool: the first loads fit, the rest falls
/// back. This mimics a thread holding more guards than there are fast slots.
#[test]
fn tiny_slot_pool_overflows() {
    with_hooks(|| {
        let registry = Arc::new(Registry::new(4));
        let shared = ArcSwap::from(Payload::new(&registry));
        test_hooks::set_fast_slot_limit(1);
        let first = shared.load();
        let second = shared.load();
        let stats = test_hooks::snapshot();
        assert_eq!(
            1, stats.fast_acquired,
            "exactly one load fits the tiny pool: {:?}",
            stats
        );
        assert!(
            stats.fast_exhausted >= 1 && stats.helping_used >= 1,
            "the second load had to fall back: {:?}",
            stats
        );
        drop(first);
        drop(second);
        drop(shared);
        registry.check_all_dropped_once("tiny_slot_pool_overflows");
    });
}

/// A store while a reader holds a debt: the writer sweeps the debt list
/// (`pay_all`) and pays the outstanding debt.
#[test]
fn writer_pays_outstanding_debt() {
    with_hooks(|| {
        let registry = Arc::new(Registry::new(8));
        let shared = ArcSwap::from(Payload::new(&registry));
        let old_gen = shared.load().gen;
        let barrier = Barrier::new(2);

        let reader = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let guard = shared.load();
                barrier.wait(); // guard is held now
                barrier.wait(); // wait for the store to happen
                drop(guard);
            });
            barrier.wait();
            // The reader holds a debt on the current value; replace it.
            shared.store(Payload::new(&registry));
            barrier.wait();
            reader.join().unwrap();
        });
        let _ = reader;

        let stats = test_hooks::snapshot();
        assert!(
            stats.pay_all >= 1,
            "expected a pay_all sweep, got {:?}",
            stats
        );
        assert!(
            stats.debts_paid >= 1,
            "expected the outstanding debt to be paid, got {:?}",
            stats
        );
        assert_eq!(
            1,
            registry.drops(old_gen),
            "the replaced value was not destructed exactly once"
        );
        drop(shared);
        registry.check_all_dropped_once("writer_pays_outstanding_debt");
    });
}

/// The counters start at zero after a reset (sanity check of the hook
/// itself, so a broken counter can't make the other tests vacuous).
#[test]
fn counters_are_live() {
    with_hooks(|| {
        assert_eq!(DebtStats::default(), test_hooks::snapshot());
        let registry = Arc::new(Registry::new(4));
        let shared = ArcSwap::from(Payload::new(&registry));
        let guard = shared.load();
        assert_ne!(
            DebtStats::default(),
            test_hooks::snapshot(),
            "counters did not move after a load"
        );
        drop(guard);
        drop(shared);
        registry.check_all_dropped_once("counters_are_live");
    });
}
