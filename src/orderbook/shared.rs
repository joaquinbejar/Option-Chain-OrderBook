//! Generic thread-safe shared-configuration wrapper.
//!
//! [`Shared<T>`] centralizes the `RwLock`-backed "store a config behind `&self`"
//! pattern used throughout the hierarchy. Managers need to propagate
//! configuration (fee schedule, STP mode, validation rules, contract specs,
//! expiry-cycle config) to children created *after* the config is set, without
//! taking `&mut self`. Each such holder used to be a hand-written wrapper with
//! identical `new`/`set`/`get`/`Default`/`Debug` boilerplate and the same
//! poison-recovery policy; this module collapses them into one generic so the
//! locking and poison policy live in a single place.
//!
//! Poison policy: a poisoned lock is recovered via
//! `unwrap_or_else(|p| p.into_inner())` on both read and write paths, so a panic
//! while another thread held the lock never drops a stored value or wedges the
//! holder.
//!
//! This is a dependency-light leaf utility (depends only on `std`); the
//! fees / stp / validation / contract-specs / expiry-cycle holders use it
//! downward — nothing in here reaches back into the hierarchy.

use std::sync::RwLock;

/// Thread-safe shared wrapper over a single value.
///
/// Wraps a `T` in a [`RwLock`] so hierarchy managers can store and update it
/// through `&self` setters. [`get`](Self::get) returns a clone of the stored
/// value; for `Copy` types the clone is a plain copy, preserving the exact
/// copy-vs-clone semantics of the bespoke wrappers this type replaced.
pub(crate) struct Shared<T> {
    /// The inner value, protected by a read-write lock.
    inner: RwLock<T>,
}

impl<T> Shared<T> {
    /// Creates a new shared wrapper holding `value`.
    #[must_use]
    #[inline]
    pub(crate) fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
        }
    }

    /// Replaces the stored value.
    ///
    /// Recovers from a poisoned lock to ensure the value is always written.
    pub(crate) fn set(&self, value: T) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = value;
    }
}

impl<T: Clone> Shared<T> {
    /// Returns a clone of the stored value.
    ///
    /// Recovers from a poisoned lock to avoid silently dropping a stored value.
    /// For `Copy` types this clone is a plain copy, matching the bespoke
    /// wrappers' `*guard` reads exactly.
    #[must_use]
    #[inline]
    pub(crate) fn get(&self) -> T {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Clone + std::fmt::Debug> std::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Clone the value out (releasing the read lock) BEFORE formatting, so a
        // `T::fmt` that re-enters this lock cannot deadlock and a slow format
        // cannot block writers — matching the bespoke wrappers this replaced,
        // which formatted a `get()` clone rather than holding the guard.
        let value = self.get();
        f.debug_struct("Shared").field("inner", &value).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_then_get_returns_initial_value() {
        let shared = Shared::new(7u32);
        assert_eq!(shared.get(), 7);
    }

    #[test]
    fn test_set_then_get_returns_new_value() {
        let shared = Shared::new(0u32);
        shared.set(42);
        assert_eq!(shared.get(), 42);
    }

    #[test]
    fn test_overwrite_keeps_latest() {
        let shared = Shared::new(1u32);
        shared.set(2);
        shared.set(3);
        assert_eq!(shared.get(), 3);
    }

    #[test]
    fn test_default_uses_inner_default() {
        let shared: Shared<u32> = Shared::default();
        assert_eq!(shared.get(), 0);
    }

    #[test]
    fn test_option_default_is_none() {
        let shared: Shared<Option<u32>> = Shared::default();
        assert!(shared.get().is_none());
    }

    #[test]
    fn test_option_set_some_then_none() {
        let shared: Shared<Option<u32>> = Shared::new(None);
        shared.set(Some(5));
        assert_eq!(shared.get(), Some(5));
        shared.set(None);
        assert!(shared.get().is_none());
    }

    #[test]
    fn test_clone_type_round_trips() {
        // A non-`Copy`, `Clone` value clones on `get`, matching the validation /
        // contract-specs / fee-schedule semantics.
        let shared = Shared::new(String::from("a"));
        let first = shared.get();
        shared.set(String::from("b"));
        assert_eq!(first, "a");
        assert_eq!(shared.get(), "b");
    }

    #[test]
    fn test_debug_contains_struct_name_and_inner() {
        let shared = Shared::new(Some(9u32));
        let debug = format!("{shared:?}");
        assert!(debug.contains("Shared"));
        assert!(debug.contains("inner"));
    }

    #[test]
    fn test_get_recovers_from_poisoned_lock() {
        use std::sync::Arc;
        let shared = Arc::new(Shared::new(11u32));
        let clone = Arc::clone(&shared);
        // Poison the lock by panicking while holding the write guard.
        let handle = std::thread::spawn(move || {
            let _guard = clone
                .inner
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            panic!("poison the lock");
        });
        assert!(handle.join().is_err());
        // The value is still readable after poisoning.
        assert_eq!(shared.get(), 11);
        // And still writable.
        shared.set(22);
        assert_eq!(shared.get(), 22);
    }
}
