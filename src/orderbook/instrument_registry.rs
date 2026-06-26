//! Instrument registry module.
//!
//! This module provides the [`InstrumentRegistry`] for allocating unique numeric
//! instrument IDs and maintaining a reverse index for O(1) lookup by ID.
//!
//! The registry is owned by
//! [`UnderlyingOrderBookManager`](super::underlying::UnderlyingOrderBookManager)
//! and propagated as `Arc<InstrumentRegistry>` through the hierarchy. When a new
//! [`StrikeOrderBook`](super::strike::StrikeOrderBook) is created via the hierarchy,
//! each call/put [`OptionOrderBook`](super::book::OptionOrderBook) is assigned a
//! unique ID from the registry.

use crate::error::{Error, Result};
use dashmap::DashMap;
use optionstratlib::{ExpirationDate, OptionStyle};
use std::sync::atomic::{AtomicU32, Ordering};

/// Compact instrument metadata stored in the reverse index.
///
/// Contains the coordinates needed to locate an instrument within the
/// order book hierarchy: symbol, expiration, strike, and option style.
#[derive(Debug, Clone)]
pub struct InstrumentInfo {
    /// The full option symbol (e.g., "BTC-20240329-50000-C").
    symbol: String,
    /// The expiration date of the option.
    expiration: ExpirationDate,
    /// The strike price.
    strike: u64,
    /// The option style (Call or Put).
    option_style: OptionStyle,
}

impl InstrumentInfo {
    /// Creates a new instrument info entry.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The full option symbol
    /// * `expiration` - The expiration date
    /// * `strike` - The strike price
    /// * `option_style` - Call or Put
    #[must_use]
    pub fn new(
        symbol: impl Into<String>,
        expiration: ExpirationDate,
        strike: u64,
        option_style: OptionStyle,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            expiration,
            strike,
            option_style,
        }
    }

    /// Returns the full option symbol.
    #[must_use]
    #[inline]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Returns the expiration date.
    // No `#[must_use]`: the underlying type `ExpirationDate` (the referent of the
    // returned `&ExpirationDate`) is itself `#[must_use]`, so a function-level
    // attribute would be a `clippy::double_must_use` error.
    #[inline]
    pub const fn expiration(&self) -> &ExpirationDate {
        &self.expiration
    }

    /// Returns the strike price.
    #[must_use]
    #[inline]
    pub const fn strike(&self) -> u64 {
        self.strike
    }

    /// Returns the option style (Call or Put).
    #[must_use]
    #[inline]
    pub const fn option_style(&self) -> OptionStyle {
        self.option_style
    }
}

impl std::fmt::Display for InstrumentInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (strike={}, style={:?})",
            self.symbol, self.strike, self.option_style
        )
    }
}

/// Thread-safe instrument ID allocator and reverse index.
///
/// Provides monotonically increasing `u32` IDs for each
/// [`OptionOrderBook`](super::book::OptionOrderBook) created through the hierarchy,
/// and maintains a `DashMap`-based reverse index for O(1) lookup by ID.
///
/// ## Thread Safety
///
/// The allocator uses [`AtomicU32`] for lock-free ID generation.
/// The reverse index uses [`DashMap`] for concurrent reads and writes via sharded locking.
///
/// ## Seed Support
///
/// Use [`new_with_seed`](Self::new_with_seed) to start IDs from a specific value,
/// allowing IDs to survive hierarchy rebuilds.
pub struct InstrumentRegistry {
    /// The next ID to allocate.
    next_id: AtomicU32,
    /// Reverse index mapping instrument ID → instrument info.
    index: DashMap<u32, InstrumentInfo>,
}

impl std::fmt::Debug for InstrumentRegistry {
    /// Diagnostic summary (next id + entry count). Deliberately lightweight and
    /// hand-rolled rather than a `#[derive(Debug)]` over the inner `DashMap`,
    /// whose `Debug` dumps entries in arbitrary shard order — a footgun if such
    /// output were ever folded into a hash or event. This avoids that
    /// shard-order nondeterminism; note the two scalar fields are each read
    /// independently, so under concurrent mutation the summary is a point-in-time
    /// approximation, not a consistent snapshot. Use [`iter`](Self::iter) for the
    /// full, id-ordered contents.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstrumentRegistry")
            .field("next_id", &self.next_id.load(Ordering::Relaxed))
            .field("len", &self.index.len())
            .finish()
    }
}

impl InstrumentRegistry {
    /// Creates a new instrument registry with IDs starting from 1.
    ///
    /// ID 0 is reserved for standalone `OptionOrderBook` instances
    /// created outside the hierarchy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            index: DashMap::new(),
        }
    }

    /// Creates a new instrument registry with IDs starting from the given seed.
    ///
    /// Use this to resume ID allocation after a hierarchy rebuild.
    /// The seed is clamped to a minimum of 1 since ID 0 is reserved
    /// for standalone books.
    ///
    /// # Arguments
    ///
    /// * `seed` - The starting ID value (clamped to >= 1)
    #[must_use]
    pub fn new_with_seed(seed: u32) -> Self {
        Self {
            next_id: AtomicU32::new(seed.max(1)),
            index: DashMap::new(),
        }
    }

    /// Allocates the next unique instrument ID.
    ///
    /// IDs are monotonically increasing and never reused.
    /// Uses `Ordering::Relaxed` since the only invariant is uniqueness,
    /// not ordering relative to other memory operations.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InstrumentIdExhausted`] if the `u32` counter has
    /// reached its `u32::MAX` ceiling (after roughly 4 billion allocations).
    /// The instrument ID is protocol state, so exhaustion is surfaced as a
    /// recoverable typed error rather than a panic. Reaching this ceiling is
    /// astronomically unlikely in practice.
    #[inline]
    pub fn allocate(&self) -> Result<u32> {
        self.next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| Error::instrument_id_exhausted())
    }

    /// Registers an instrument in the reverse index.
    ///
    /// # Arguments
    ///
    /// * `id` - The instrument ID (from [`allocate`](Self::allocate))
    /// * `info` - The instrument metadata
    pub fn register(&self, id: u32, info: InstrumentInfo) {
        self.index.insert(id, info);
    }

    /// Looks up instrument info by ID.
    ///
    /// Returns `None` if the ID is not registered.
    ///
    /// # Arguments
    ///
    /// * `id` - The instrument ID to look up
    #[must_use]
    #[inline]
    pub fn get(&self, id: u32) -> Option<InstrumentInfo> {
        self.index.get(&id).map(|entry| entry.value().clone())
    }

    /// Returns the number of registered instruments.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Returns `true` if no instruments are registered.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Returns the current ID counter value without advancing it.
    ///
    /// This is the next ID that will be allocated. Useful for persisting
    /// the counter state before shutdown.
    #[must_use]
    #[inline]
    pub fn current_id(&self) -> u32 {
        self.next_id.load(Ordering::Relaxed)
    }

    /// Returns a snapshot of all registered `(id, InstrumentInfo)` pairs.
    ///
    /// Yields entries in ascending instrument-id order (deterministic).
    /// Instrument IDs are unique, so the ordering is total and stable across
    /// calls — this matters because the registry is exposed to external
    /// sequencer/journal consumers, where an arbitrary `DashMap` shard order
    /// would be a determinism footgun.
    ///
    /// The returned `Vec` is a point-in-time snapshot — concurrent
    /// registrations that occur after the call begins may or may not
    /// be included.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use option_chain_orderbook::orderbook::{InstrumentInfo, InstrumentRegistry};
    /// use optionstratlib::{ExpirationDate, OptionStyle};
    /// use optionstratlib::prelude::pos_or_panic;
    ///
    /// let registry = InstrumentRegistry::new();
    /// let id = registry.allocate().expect("fresh registry has IDs available");
    /// registry.register(id, InstrumentInfo::new(
    ///     "BTC-20240329-50000-C",
    ///     ExpirationDate::Days(pos_or_panic!(30.0)),
    ///     50000,
    ///     OptionStyle::Call,
    /// ));
    ///
    /// let entries = registry.iter();
    /// assert_eq!(entries.len(), 1);
    /// assert_eq!(entries[0].0, id);
    /// ```
    #[must_use]
    pub fn iter(&self) -> Vec<(u32, InstrumentInfo)> {
        let mut entries: Vec<(u32, InstrumentInfo)> = self
            .index
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect();
        // Ids are unique, so the key yields a total order: unstable sort is
        // both deterministic and cheaper than a stable sort here.
        entries.sort_unstable_by_key(|(id, _)| *id);
        entries
    }
}

impl Default for InstrumentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optionstratlib::prelude::pos_or_panic;

    fn test_expiration() -> ExpirationDate {
        ExpirationDate::Days(pos_or_panic!(30.0))
    }

    #[test]
    fn test_registry_new_starts_from_one() {
        let registry = InstrumentRegistry::new();
        assert_eq!(registry.current_id(), 1);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_registry_new_with_seed() {
        let registry = InstrumentRegistry::new_with_seed(100);
        assert_eq!(registry.current_id(), 100);
    }

    #[test]
    fn test_allocate_monotonic() {
        let registry = InstrumentRegistry::new();
        let id1 = registry.allocate().expect("id available");
        let id2 = registry.allocate().expect("id available");
        let id3 = registry.allocate().expect("id available");

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
        assert_eq!(registry.current_id(), 4);
    }

    #[test]
    fn test_allocate_with_seed() {
        let registry = InstrumentRegistry::new_with_seed(50);
        assert_eq!(registry.allocate().expect("id available"), 50);
        assert_eq!(registry.allocate().expect("id available"), 51);
        assert_eq!(registry.current_id(), 52);
    }

    #[test]
    fn test_allocate_exhaustion_returns_typed_error() {
        // Seeding at u32::MAX means checked_add(1) immediately overflows: the
        // very first allocation must surface a typed error, never panic.
        let registry = InstrumentRegistry::new_with_seed(u32::MAX);
        let result = registry.allocate();
        assert!(matches!(result, Err(Error::InstrumentIdExhausted)));
    }

    #[test]
    fn test_allocate_exhaustion_after_last_id() {
        // Seeding one below the ceiling allows exactly one successful allocation
        // (returning u32::MAX - 1, advancing the counter to u32::MAX); the next
        // allocation overflows and returns the typed exhaustion error.
        let registry = InstrumentRegistry::new_with_seed(u32::MAX - 1);
        assert_eq!(registry.allocate().expect("one id remains"), u32::MAX - 1);
        assert_eq!(registry.current_id(), u32::MAX);
        assert!(matches!(
            registry.allocate(),
            Err(Error::InstrumentIdExhausted)
        ));
        // Counter stays pinned at the ceiling; repeated calls keep erroring.
        assert_eq!(registry.current_id(), u32::MAX);
        assert!(matches!(
            registry.allocate(),
            Err(Error::InstrumentIdExhausted)
        ));
    }

    #[test]
    fn test_register_and_get() {
        let registry = InstrumentRegistry::new();
        let id = registry.allocate().expect("id available");

        let info = InstrumentInfo::new(
            "BTC-20240329-50000-C",
            test_expiration(),
            50000,
            OptionStyle::Call,
        );
        registry.register(id, info);

        let retrieved = registry.get(id);
        assert!(retrieved.is_some());

        let retrieved = match retrieved {
            Some(r) => r,
            None => panic!("expected instrument info"),
        };
        assert_eq!(retrieved.symbol(), "BTC-20240329-50000-C");
        assert_eq!(retrieved.strike(), 50000);
        assert_eq!(retrieved.option_style(), OptionStyle::Call);
    }

    #[test]
    fn test_get_missing_returns_none() {
        let registry = InstrumentRegistry::new();
        assert!(registry.get(999).is_none());
    }

    #[test]
    fn test_len_and_is_empty() {
        let registry = InstrumentRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        let id = registry.allocate().expect("id available");
        registry.register(
            id,
            InstrumentInfo::new(
                "BTC-20240329-50000-C",
                test_expiration(),
                50000,
                OptionStyle::Call,
            ),
        );

        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_multiple_registrations() {
        let registry = InstrumentRegistry::new();

        for i in 0..10 {
            let id = registry.allocate().expect("id available");
            registry.register(
                id,
                InstrumentInfo::new(
                    format!("BTC-20240329-{}-C", 50000 + i * 1000),
                    test_expiration(),
                    50000 + i * 1000,
                    OptionStyle::Call,
                ),
            );
        }

        assert_eq!(registry.len(), 10);
        assert_eq!(registry.current_id(), 11);

        // Verify first and last
        let first = registry.get(1);
        assert!(first.is_some());
        let first = match first {
            Some(f) => f,
            None => panic!("expected instrument info"),
        };
        assert_eq!(first.strike(), 50000);

        let last = registry.get(10);
        assert!(last.is_some());
        let last = match last {
            Some(l) => l,
            None => panic!("expected instrument info"),
        };
        assert_eq!(last.strike(), 59000);
    }

    #[test]
    fn test_concurrent_allocation() {
        use std::sync::Arc;
        use std::thread;

        let registry = Arc::new(InstrumentRegistry::new());
        let mut handles = vec![];

        for _ in 0..4 {
            let reg = Arc::clone(&registry);
            handles.push(thread::spawn(move || {
                let mut ids = Vec::new();
                for _ in 0..100 {
                    ids.push(reg.allocate().expect("id available"));
                }
                ids
            }));
        }

        let mut all_ids: Vec<u32> = handles
            .into_iter()
            .flat_map(|h| match h.join() {
                Ok(ids) => ids,
                Err(_) => panic!("thread panicked"),
            })
            .collect();

        all_ids.sort();
        all_ids.dedup();

        // 4 threads × 100 allocations = 400 unique IDs
        assert_eq!(all_ids.len(), 400);
        assert_eq!(registry.current_id(), 401);
    }

    #[test]
    fn test_instrument_info_display() {
        let info = InstrumentInfo::new(
            "BTC-20240329-50000-C",
            test_expiration(),
            50000,
            OptionStyle::Call,
        );
        let display = info.to_string();
        assert!(display.contains("BTC-20240329-50000-C"));
        assert!(display.contains("50000"));
    }

    #[test]
    fn test_instrument_info_clone() {
        let info = InstrumentInfo::new(
            "BTC-20240329-50000-C",
            test_expiration(),
            50000,
            OptionStyle::Call,
        );
        let cloned = info.clone();
        assert_eq!(info.symbol(), cloned.symbol());
        assert_eq!(info.strike(), cloned.strike());
        assert_eq!(info.option_style(), cloned.option_style());
    }

    #[test]
    fn test_default_registry() {
        let registry = InstrumentRegistry::default();
        assert_eq!(registry.current_id(), 1);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_seed_zero_clamped_to_one() {
        let registry = InstrumentRegistry::new_with_seed(0);
        assert_eq!(registry.current_id(), 1);
        assert_eq!(registry.allocate().expect("id available"), 1);
    }

    #[test]
    fn test_iter_empty() {
        let registry = InstrumentRegistry::new();
        let entries = registry.iter();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_iter_returns_all_entries() {
        let registry = InstrumentRegistry::new();

        for i in 0..5 {
            let id = registry.allocate().expect("id available");
            registry.register(
                id,
                InstrumentInfo::new(
                    format!("BTC-20240329-{}-C", 50000 + i * 1000),
                    test_expiration(),
                    50000 + i * 1000,
                    OptionStyle::Call,
                ),
            );
        }

        let entries = registry.iter();
        assert_eq!(entries.len(), 5);

        // iter() guarantees ascending instrument-id order (deterministic), so
        // the ids appear already sorted without any post-processing.
        let ids: Vec<u32> = entries.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);

        // Verify info is correct for one entry
        let entry = entries.iter().find(|(id, _)| *id == 1);
        assert!(entry.is_some());
        let (_, info) = entry.expect("entry should exist");
        assert_eq!(info.strike(), 50000);
    }
}
