//! Shared infrastructure for the concurrency model tests.
//!
//! The [`Registry`] tracks, for every payload generation ever created, how
//! many times it was destructed and how many references (Guards or Arcs) the
//! test model currently holds to it. This allows asserting the two central
//! invariants of the crate:
//!
//! * A generation is never destructed while a `Guard` or `Arc` still holds
//!   it (no premature drop, no use-after-free).
//! * Every generation that becomes unreachable is eventually destructed
//!   exactly once (no leak, no double drop).

#![allow(dead_code)]

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Sentinel for "no violation recorded" in the violation slots.
const NONE: usize = usize::MAX;

/// Tracks destructor counts and model-held references per generation.
pub struct Registry {
    created: AtomicUsize,
    drops: Vec<AtomicUsize>,
    held: Vec<AtomicUsize>,
    double_drop: AtomicUsize,
    drop_while_held: AtomicUsize,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Registry({} generations)", self.created())
    }
}

impl Registry {
    pub fn new(capacity: usize) -> Self {
        Registry {
            created: AtomicUsize::new(0),
            drops: (0..capacity).map(|_| AtomicUsize::new(0)).collect(),
            held: (0..capacity).map(|_| AtomicUsize::new(0)).collect(),
            double_drop: AtomicUsize::new(NONE),
            drop_while_held: AtomicUsize::new(NONE),
        }
    }

    /// Allocates a fresh, unique generation id.
    pub fn next_gen(&self) -> usize {
        let gen = self.created.fetch_add(1, Ordering::Relaxed);
        assert!(
            gen < self.drops.len(),
            "registry capacity {} exceeded",
            self.drops.len()
        );
        gen
    }

    pub fn created(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }

    /// How many times the given generation was destructed so far.
    pub fn drops(&self, gen: usize) -> usize {
        self.drops[gen].load(Ordering::SeqCst)
    }

    /// Model bookkeeping: a Guard or Arc holding `gen` was acquired.
    pub fn hold(&self, gen: usize) {
        self.held[gen].fetch_add(1, Ordering::SeqCst);
    }

    /// Model bookkeeping: a Guard or Arc holding `gen` is about to be
    /// released. Must be called *before* the actual drop, so the payload's
    /// destructor never observes a stale "held" mark for the reference that
    /// is being dropped right now.
    pub fn release(&self, gen: usize) {
        self.held[gen].fetch_sub(1, Ordering::SeqCst);
    }

    /// Quiescent-state invariant check.
    ///
    /// To be called when no thread is in the middle of an operation (eg.
    /// after a barrier or a join): no generation may be destructed while the
    /// model still holds a reference to it, and no generation may have been
    /// destructed more than once.
    pub fn check_quiescent(&self, ctx: &str) {
        let gen = self.double_drop.load(Ordering::SeqCst);
        assert_eq!(
            NONE, gen,
            "{}: generation {} was destructed twice",
            ctx, gen
        );
        let gen = self.drop_while_held.load(Ordering::SeqCst);
        assert_eq!(
            NONE, gen,
            "{}: generation {} was destructed while still held",
            ctx, gen
        );
        for gen in 0..self.created() {
            let drops = self.drops[gen].load(Ordering::SeqCst);
            let held = self.held[gen].load(Ordering::SeqCst);
            assert!(
                drops <= 1,
                "{}: generation {} destructed {} times",
                ctx, gen, drops
            );
            assert!(
                held == 0 || drops == 0,
                "{}: generation {} destructed with {} live references",
                ctx, gen, held
            );
        }
    }

    /// Final invariant check: everything ever created was destructed exactly
    /// once (nothing leaked, nothing dropped twice).
    pub fn check_all_dropped_once(&self, ctx: &str) {
        self.check_quiescent(ctx);
        let created = self.created();
        let missing: Vec<usize> = (0..created)
            .filter(|&gen| self.drops[gen].load(Ordering::SeqCst) != 1)
            .collect();
        assert!(
            missing.is_empty(),
            "{}: {} of {} generations were not destructed exactly once (first few: {:?})",
            ctx,
            missing.len(),
            created,
            &missing[..missing.len().min(10)],
        );
    }
}

/// A payload with a unique generation id, registered in the destructor
/// counters of its [`Registry`].
pub struct Payload {
    pub gen: usize,
    registry: Arc<Registry>,
}

impl Payload {
    /// Creates a new payload with a fresh generation, wrapped in an [`Arc`].
    pub fn new(registry: &Arc<Registry>) -> Arc<Payload> {
        Arc::new(Payload {
            gen: registry.next_gen(),
            registry: Arc::clone(registry),
        })
    }
}

impl Drop for Payload {
    fn drop(&mut self) {
        let gen = self.gen;
        let prev = self.registry.drops[gen].fetch_add(1, Ordering::SeqCst);
        if prev != 0 {
            let _ = self.registry.double_drop.compare_exchange(
                NONE,
                gen,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
        if self.registry.held[gen].load(Ordering::SeqCst) != 0 {
            let _ = self.registry.drop_while_held.compare_exchange(
                NONE,
                gen,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
    }
}

/// A deterministic xorshift RNG, so failures are reproducible from the
/// printed seed. No clocks, no environment, no external crates.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Avoid the all-zero fixed point.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}
