//! Thin abstraction over the atomic types used by the crate.
//!
//! In normal builds this simply re-exports the atomics from [`core`]. When the
//! crate is compiled with `--cfg loom` (done only by the permutation tests in
//! `tests/loom.rs`, never in production builds), the checked atomics from the
//! `loom` crate are used instead.
//!
//! Both backends expose the same operations with the same orderings, so the
//! *exact same* algorithm ‒ with the exact same state transitions ‒ is
//! executed either way. Loom only enumerates the possible interleavings of the
//! very same code; no simplified model of the algorithm is written for the
//! permutation tests.

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicPtr, AtomicUsize};
#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{AtomicPtr, AtomicUsize};

// Both loom and core re-export the very same Ordering type, so it can be
// shared between the two configurations.
pub(crate) use core::sync::atomic::Ordering;

/// Reads the current value out of an atomic pointer, given exclusive access.
///
/// This is `get_mut` on the standard atomics. Loom's atomics don't have
/// `get_mut`, but provide the equivalent `with_mut`.
#[cfg(not(loom))]
pub(crate) fn ptr_get_mut<T>(ptr: &mut AtomicPtr<T>) -> *mut T {
    *ptr.get_mut()
}

/// Reads the current value out of an atomic pointer, given exclusive access.
///
/// This is `get_mut` on the standard atomics. Loom's atomics don't have
/// `get_mut`, but provide the equivalent `with_mut`.
#[cfg(loom)]
pub(crate) fn ptr_get_mut<T>(ptr: &mut AtomicPtr<T>) -> *mut T {
    ptr.with_mut(|p| *p)
}

/// Writes a value into an atomic pointer, given exclusive access.
///
/// This is `get_mut` on the standard atomics. Loom's atomics don't have
/// `get_mut`, but provide the equivalent `with_mut`.
#[cfg(not(loom))]
pub(crate) fn ptr_set_mut<T>(ptr: &mut AtomicPtr<T>, val: *mut T) {
    *ptr.get_mut() = val;
}

/// Writes a value into an atomic pointer, given exclusive access.
///
/// This is `get_mut` on the standard atomics. Loom's atomics don't have
/// `get_mut`, but provide the equivalent `with_mut`.
#[cfg(loom)]
pub(crate) fn ptr_set_mut<T>(ptr: &mut AtomicPtr<T>, val: *mut T) {
    ptr.with_mut(|p| *p = val);
}
