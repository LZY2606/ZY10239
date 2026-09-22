//! Test hooks into the internal debt machinery.
//!
//! This module exists only with the `internal-test-hooks` feature and is
//! meant strictly for the crate's own test suite. It comes with **no
//! stability guarantees** and may change or disappear in any release. Do not
//! use in production code.
//!
//! It allows tests to:
//!
//! * Observe which internal paths were exercised (fast slot, slot exhaustion,
//!   helping fallback, writer-side `pay_all`), proving the interesting
//!   interleavings really happened instead of merely hoping for them.
//! * Artificially shrink the fast slot pool, to deterministically force the
//!   slot-exhaustion and fallback paths.
//!
//! Enabling the feature adds a few relaxed atomic operations to the loading
//! and storing paths, but does not change any semantics, public API or type
//! layout. Without the feature, none of this code (including the counters) is
//! compiled in at all.

use crate::debt::stats;

/// A snapshot of the internal debt-machinery event counters.
///
/// All events are counted since the last reset (or since program start).
/// The counters are process-global and relaxed; concurrent activity in other
/// threads may influence them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DebtStats {
    /// A debt was successfully placed into a fast slot (the fast path).
    pub fast_acquired: usize,
    /// The fast slot pool was exhausted (or limited to nothing) during a load
    /// attempt, so the load had to fall back to the helping strategy.
    pub fast_exhausted: usize,
    /// The helping/fallback strategy was used for a load.
    pub helping_used: usize,
    /// A writer offered a replacement pointer to a colliding reader (the
    /// writer-reader collision path of the helping strategy).
    pub helped: usize,
    /// A writer performed a `pay_all` sweep over the debt nodes.
    pub pay_all: usize,
    /// A writer paid an individual debt.
    pub debts_paid: usize,
}

/// Takes a snapshot of the internal event counters.
pub fn snapshot() -> DebtStats {
    let [fast_acquired, fast_exhausted, helping_used, helped, pay_all, debts_paid] =
        stats::snapshot();
    DebtStats {
        fast_acquired,
        fast_exhausted,
        helping_used,
        helped,
        pay_all,
        debts_paid,
    }
}

/// Resets all the event counters to 0.
///
/// Note this does not reset the fast slot limit set by
/// [`set_fast_slot_limit`].
pub fn reset() {
    stats::reset();
}

/// Artificially limits the number of usable fast debt slots per thread.
///
/// This allows tests to deterministically force slot exhaustion (and
/// therefore the fallback strategy) without having to hold many guards.
/// Pass `usize::MAX` to disable the limit (the default). Values larger than
/// the real number of slots behave as if no limit was set.
///
/// The limit is process-global; tests using it should serialise themselves
/// and restore the default afterwards.
pub fn set_fast_slot_limit(limit: usize) {
    stats::set_slot_limit(limit);
}

/// The real number of fast debt slots per thread (without any artificial
/// limit applied).
pub fn fast_slot_count() -> usize {
    crate::debt::fast::DEBT_SLOT_CNT
}
