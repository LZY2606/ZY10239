//! A thin compile-time switch between real atomics and loom-instrumented ones.
//!
//! When the crate is compiled normally, this simply re-exports the atomics
//! from [`core`]. When compiled with `--cfg loom` (see `TESTING.md`), the
//! [`loom`] crate's instrumented versions are used instead. These have the
//! exact same API and semantics, but allow loom to enumerate all the possible
//! thread interleavings in tests.
//!
//! The point is that the *same* algorithm code (the same source lines, the
//! same orderings) is executed in production and under the loom models ‒ we
//! deliberately do *not* maintain a separate, simplified re-implementation of
//! the algorithms for verification, as that could diverge from the real thing
//! and prove properties about something else than what actually ships.
//!
//! This module changes nothing in a non-loom build: the re-exports are the
//! very same types, so there is no runtime or layout difference whatsoever.

#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
