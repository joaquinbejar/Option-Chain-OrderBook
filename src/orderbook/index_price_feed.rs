//! Index price feed implementations and mark-price wiring.
//!
//! This pricing-subsystem module provides the concrete price sources
//! ([`MockPriceFeed`], [`StaticPriceFeed`]) and the
//! [`wire_feed_to_calculator`] helper that feeds an external price source
//! (Chainlink, exchange feeds, etc.) into the [`MarkPriceCalculator`],
//! decoupling mark price calculation from the specific source implementation.
//!
//! The [`IndexPriceFeed`] trait itself — together with [`PriceUpdate`],
//! [`PriceUpdateListener`], and [`SubscriptionId`] — lives in the neutral,
//! dependency-light [`index_feed`](super::index_feed) module so the core
//! order-book hierarchy can hold an `Arc<dyn IndexPriceFeed>` without depending
//! on this pricing subsystem. This module depends downward on that trait and
//! implements it.
//!
//! ## Overview
//!
//! The [`IndexPriceFeed`] trait defines a pluggable interface for price sources:
//! - [`latest_price`](IndexPriceFeed::latest_price): Returns the most recent price
//! - [`subscribe`](IndexPriceFeed::subscribe): Registers a callback for price updates
//! - [`source`](IndexPriceFeed::source): Identifies the price source
//!
//! ## Implementations
//!
//! - [`MockPriceFeed`]: Programmatic price injection for testing
//! - [`StaticPriceFeed`]: Fixed price that never changes (manual injection)
//!
//! ## Wiring
//!
//! Use [`wire_feed_to_calculator`] to connect a feed to a [`MarkPriceCalculator`],
//! so that every price update automatically refreshes the calculator's index price.
//!
//! ## Example
//!
//! ```
//! use std::sync::Arc;
//! use option_chain_orderbook::orderbook::{
//!     IndexPriceFeed, MockPriceFeed, MarkPriceCalculator, wire_feed_to_calculator,
//! };
//!
//! let feed = Arc::new(MockPriceFeed::new());
//! let calculator = Arc::new(MarkPriceCalculator::with_default_config());
//!
//! // Wire feed → calculator
//! wire_feed_to_calculator(feed.as_ref(), Arc::clone(&calculator));
//!
//! // Update feed — calculator receives the price automatically
//! feed.set_price(50000);
//! assert_eq!(calculator.index_price(), 50000);
//! ```

use super::index_feed::{IndexPriceFeed, PriceUpdate, PriceUpdateListener, SubscriptionId};
use super::mark_price::MarkPriceCalculator;
use crate::utils::nanos_since_epoch;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ─── MockPriceFeed ───────────────────────────────────────────────────────────

/// Mock price feed for testing.
///
/// Allows programmatic price injection via [`set_price`](Self::set_price).
/// Every call to `set_price` notifies all registered listeners and updates
/// the latest price returned by [`latest_price`](IndexPriceFeed::latest_price).
///
/// ## Thread Safety
///
/// Uses [`AtomicU64`] for the price and a [`Mutex`] for the listener list,
/// making it safe for concurrent access from multiple threads.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::{IndexPriceFeed, MockPriceFeed};
///
/// let feed = MockPriceFeed::new();
/// feed.set_price(42000);
///
/// let update = feed.latest_price().unwrap();
/// assert_eq!(update.price, 42000);
/// assert_eq!(update.source, "mock");
/// ```
pub struct MockPriceFeed {
    /// Current price stored atomically.
    price: AtomicU64,
    /// Timestamp of the last price update in nanoseconds.
    timestamp_ns: AtomicU64,
    /// Monotonically increasing counter for subscription ids.
    next_id: AtomicU64,
    /// Registered listeners notified on each `set_price` call.
    listeners: Mutex<Vec<(SubscriptionId, PriceUpdateListener)>>,
}

impl MockPriceFeed {
    /// Creates a new mock feed with no initial price.
    #[must_use]
    pub fn new() -> Self {
        Self {
            price: AtomicU64::new(0),
            timestamp_ns: AtomicU64::new(0),
            next_id: AtomicU64::new(0),
            listeners: Mutex::new(Vec::new()),
        }
    }

    /// Sets the current price and notifies all subscribers.
    ///
    /// The timestamp is recorded as the current wall-clock time in
    /// nanoseconds since Unix epoch.
    ///
    /// # Arguments
    ///
    /// * `price` - New price in smallest units
    pub fn set_price(&self, price: u64) {
        let ts = nanos_since_epoch();
        self.price.store(price, Ordering::Release);
        self.timestamp_ns.store(ts, Ordering::Release);

        let update = PriceUpdate {
            price,
            timestamp_ns: ts,
            source: "mock".to_string(),
        };

        // Clone the listener list while holding the lock, then drop
        // the lock before invoking callbacks. This prevents deadlocks
        // if a listener calls back into the feed (e.g., subscribe).
        let listeners = match self.listeners.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };

        for (_id, listener) in listeners.iter() {
            listener(&update);
        }
    }
}

impl Default for MockPriceFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MockPriceFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockPriceFeed")
            .field("price", &self.price.load(Ordering::Relaxed))
            .field("timestamp_ns", &self.timestamp_ns.load(Ordering::Relaxed))
            .finish()
    }
}

impl IndexPriceFeed for MockPriceFeed {
    fn latest_price(&self) -> Option<PriceUpdate> {
        let price = self.price.load(Ordering::Acquire);
        if price == 0 {
            return None;
        }
        Some(PriceUpdate {
            price,
            timestamp_ns: self.timestamp_ns.load(Ordering::Acquire),
            source: "mock".to_string(),
        })
    }

    fn subscribe(&self, listener: PriceUpdateListener) -> SubscriptionId {
        let id = SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        match self.listeners.lock() {
            Ok(mut listeners) => listeners.push((id, listener)),
            Err(poisoned) => poisoned.into_inner().push((id, listener)),
        }
        id
    }

    fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut guard = match self.listeners.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let before = guard.len();
        guard.retain(|(sub_id, _)| *sub_id != id);
        guard.len() < before
    }

    fn source(&self) -> &str {
        "mock"
    }
}

// ─── StaticPriceFeed ─────────────────────────────────────────────────────────

/// Static price feed that returns a fixed price.
///
/// Useful for manual price injection or deterministic testing where
/// the price should never change. Subscribers are accepted but never
/// notified since the price is immutable after construction.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::{IndexPriceFeed, StaticPriceFeed};
///
/// let feed = StaticPriceFeed::new(50000, "manual");
///
/// let update = feed.latest_price().unwrap();
/// assert_eq!(update.price, 50000);
/// assert_eq!(update.source, "manual");
/// ```
pub struct StaticPriceFeed {
    /// The fixed price value.
    price: u64,
    /// Source identifier.
    source: String,
    /// Timestamp recorded at construction time in nanoseconds.
    timestamp_ns: u64,
}

impl StaticPriceFeed {
    /// Creates a new static feed with a fixed price.
    ///
    /// # Arguments
    ///
    /// * `price` - Fixed price in smallest units
    /// * `source` - Human-readable source identifier
    #[must_use]
    pub fn new(price: u64, source: impl Into<String>) -> Self {
        Self {
            price,
            source: source.into(),
            timestamp_ns: nanos_since_epoch(),
        }
    }
}

impl std::fmt::Debug for StaticPriceFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticPriceFeed")
            .field("price", &self.price)
            .field("source", &self.source)
            .finish()
    }
}

impl IndexPriceFeed for StaticPriceFeed {
    fn latest_price(&self) -> Option<PriceUpdate> {
        if self.price == 0 {
            return None;
        }
        Some(PriceUpdate {
            price: self.price,
            timestamp_ns: self.timestamp_ns,
            source: self.source.clone(),
        })
    }

    fn subscribe(&self, _listener: PriceUpdateListener) -> SubscriptionId {
        // StaticPriceFeed never changes its price and never notifies listeners.
        // Accept the listener to satisfy the trait contract, then drop it to
        // avoid unbounded growth of an unused listener list.
        SubscriptionId(0)
    }

    fn unsubscribe(&self, _id: SubscriptionId) -> bool {
        // StaticPriceFeed never stores listeners, so nothing to remove.
        false
    }

    fn source(&self) -> &str {
        &self.source
    }
}

// ─── Wiring Helper ───────────────────────────────────────────────────────────

/// Wires an [`IndexPriceFeed`] to a [`MarkPriceCalculator`].
///
/// Subscribes a listener on the feed that calls
/// [`update_index_price`](MarkPriceCalculator::update_index_price)
/// on the calculator whenever a new price arrives. Also seeds the
/// calculator with the feed's current latest price, if available.
///
/// Returns a [`SubscriptionId`] that can be passed to
/// [`IndexPriceFeed::unsubscribe`] to disconnect the wiring.
///
/// # Arguments
///
/// * `feed` - The price feed to subscribe to
/// * `calculator` - The mark price calculator to update
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use option_chain_orderbook::orderbook::{
///     IndexPriceFeed, MockPriceFeed, MarkPriceCalculator, wire_feed_to_calculator,
/// };
///
/// let feed = Arc::new(MockPriceFeed::new());
/// let calc = Arc::new(MarkPriceCalculator::with_default_config());
///
/// let sub_id = wire_feed_to_calculator(feed.as_ref(), Arc::clone(&calc));
///
/// feed.set_price(99000);
/// assert_eq!(calc.index_price(), 99000);
///
/// // Disconnect the wiring
/// feed.unsubscribe(sub_id);
/// ```
pub fn wire_feed_to_calculator(
    feed: &dyn IndexPriceFeed,
    calculator: Arc<MarkPriceCalculator>,
) -> SubscriptionId {
    // Seed with current price if available
    if let Some(update) = feed.latest_price() {
        calculator.update_index_price(update.price);
    }

    // Subscribe for future updates
    let listener: PriceUpdateListener = Arc::new(move |update: &PriceUpdate| {
        calculator.update_index_price(update.price);
    });

    feed.subscribe(listener)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    // ── MockPriceFeed ────────────────────────────────────────────────────

    #[test]
    fn test_mock_feed_no_initial_price() {
        let feed = MockPriceFeed::new();
        assert!(feed.latest_price().is_none());
    }

    #[test]
    fn test_mock_feed_set_price() {
        let feed = MockPriceFeed::new();
        feed.set_price(42000);

        let update = feed.latest_price().unwrap();
        assert_eq!(update.price, 42000);
        assert_eq!(update.source, "mock");
        assert!(update.timestamp_ns > 0);
    }

    #[test]
    fn test_mock_feed_set_price_overwrites() {
        let feed = MockPriceFeed::new();
        feed.set_price(100);
        feed.set_price(200);

        let update = feed.latest_price().unwrap();
        assert_eq!(update.price, 200);
    }

    #[test]
    fn test_mock_feed_subscribe_receives_updates() {
        let feed = MockPriceFeed::new();
        let received = Arc::new(AtomicU64::new(0));
        let received_clone = Arc::clone(&received);

        feed.subscribe(Arc::new(move |update: &PriceUpdate| {
            received_clone.store(update.price, Ordering::Release);
        }));

        feed.set_price(55000);
        assert_eq!(received.load(Ordering::Acquire), 55000);
    }

    #[test]
    fn test_mock_feed_multiple_subscribers() {
        let feed = MockPriceFeed::new();
        let count = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let count_clone = Arc::clone(&count);
            feed.subscribe(Arc::new(move |_: &PriceUpdate| {
                count_clone.fetch_add(1, Ordering::Relaxed);
            }));
        }

        feed.set_price(100);
        assert_eq!(count.load(Ordering::Relaxed), 3);

        feed.set_price(200);
        assert_eq!(count.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn test_mock_feed_source() {
        let feed = MockPriceFeed::new();
        assert_eq!(feed.source(), "mock");
    }

    #[test]
    fn test_mock_feed_default() {
        let feed = MockPriceFeed::default();
        assert!(feed.latest_price().is_none());
        assert_eq!(feed.source(), "mock");
    }

    #[test]
    fn test_mock_feed_debug() {
        let feed = MockPriceFeed::new();
        feed.set_price(123);
        let debug = format!("{:?}", feed);
        assert!(debug.contains("MockPriceFeed"));
        assert!(debug.contains("123"));
    }

    // ── StaticPriceFeed ──────────────────────────────────────────────────

    #[test]
    fn test_static_feed_returns_fixed_price() {
        let feed = StaticPriceFeed::new(50000, "manual");

        let update = feed.latest_price().unwrap();
        assert_eq!(update.price, 50000);
        assert_eq!(update.source, "manual");
        assert!(update.timestamp_ns > 0);

        // Second call returns the same price
        let update2 = feed.latest_price().unwrap();
        assert_eq!(update2.price, 50000);
    }

    #[test]
    fn test_static_feed_zero_price_returns_none() {
        let feed = StaticPriceFeed::new(0, "empty");
        assert!(feed.latest_price().is_none());
    }

    #[test]
    fn test_static_feed_subscribe_no_updates() {
        let feed = StaticPriceFeed::new(100, "static");
        let called = Arc::new(AtomicUsize::new(0));
        let called_clone = Arc::clone(&called);

        feed.subscribe(Arc::new(move |_: &PriceUpdate| {
            called_clone.fetch_add(1, Ordering::Relaxed);
        }));

        // No set_price method on StaticPriceFeed, so listener is never called
        assert_eq!(called.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_static_feed_source() {
        let feed = StaticPriceFeed::new(1, "chainlink");
        assert_eq!(feed.source(), "chainlink");
    }

    #[test]
    fn test_static_feed_debug() {
        let feed = StaticPriceFeed::new(999, "test_src");
        let debug = format!("{:?}", feed);
        assert!(debug.contains("StaticPriceFeed"));
        assert!(debug.contains("999"));
        assert!(debug.contains("test_src"));
    }

    // ── wire_feed_to_calculator ──────────────────────────────────────────

    #[test]
    fn test_wire_feed_to_calculator_propagates_updates() {
        let feed = MockPriceFeed::new();
        let calc = Arc::new(MarkPriceCalculator::with_default_config());

        let _listener = wire_feed_to_calculator(&feed, Arc::clone(&calc));

        feed.set_price(60000);
        assert_eq!(calc.index_price(), 60000);

        feed.set_price(61000);
        assert_eq!(calc.index_price(), 61000);
    }

    #[test]
    fn test_wire_feed_seeds_existing_price() {
        let feed = MockPriceFeed::new();
        feed.set_price(45000);

        let calc = Arc::new(MarkPriceCalculator::with_default_config());
        let _listener = wire_feed_to_calculator(&feed, Arc::clone(&calc));

        // Calculator should be seeded with the existing price
        assert_eq!(calc.index_price(), 45000);
    }

    #[test]
    fn test_wire_static_feed_to_calculator() {
        let feed = StaticPriceFeed::new(70000, "oracle");
        let calc = Arc::new(MarkPriceCalculator::with_default_config());

        let _listener = wire_feed_to_calculator(&feed, Arc::clone(&calc));

        // Seeded with the static price
        assert_eq!(calc.index_price(), 70000);
    }

    #[test]
    fn test_wire_feed_no_initial_price() {
        let feed = MockPriceFeed::new();
        let calc = Arc::new(MarkPriceCalculator::with_default_config());

        let _listener = wire_feed_to_calculator(&feed, Arc::clone(&calc));

        // No price set → calculator stays at 0
        assert_eq!(calc.index_price(), 0);
    }

    // ── Thread safety ────────────────────────────────────────────────────

    #[test]
    fn test_mock_feed_thread_safety() {
        let feed = Arc::new(MockPriceFeed::new());
        let total = Arc::new(AtomicUsize::new(0));
        let total_clone = Arc::clone(&total);

        feed.subscribe(Arc::new(move |_: &PriceUpdate| {
            total_clone.fetch_add(1, Ordering::Relaxed);
        }));

        let mut handles = vec![];
        for i in 0..4 {
            let feed_clone = Arc::clone(&feed);
            handles.push(thread::spawn(move || {
                for j in 0..50 {
                    feed_clone.set_price((i * 50 + j) as u64 * 100);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // 4 threads × 50 updates = 200 notifications
        assert_eq!(total.load(Ordering::Relaxed), 200);

        // Final price is some valid value (non-deterministic which thread wins)
        assert!(feed.latest_price().is_some());
    }

    #[test]
    fn test_wire_feed_concurrent_updates() {
        let feed = Arc::new(MockPriceFeed::new());
        let calc = Arc::new(MarkPriceCalculator::with_default_config());

        let _listener = wire_feed_to_calculator(feed.as_ref(), Arc::clone(&calc));

        let mut handles = vec![];
        for i in 1..=4 {
            let feed_clone = Arc::clone(&feed);
            handles.push(thread::spawn(move || {
                for j in 1..=25 {
                    feed_clone.set_price(i * 1000 + j);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Calculator should have received the last update
        assert!(calc.index_price() > 0);
    }

    // ── Unsubscribe ───────────────────────────────────────────────────────

    #[test]
    fn test_unsubscribe_stops_notifications() {
        let feed = MockPriceFeed::new();
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&count);

        let id = feed.subscribe(Arc::new(move |_: &PriceUpdate| {
            count_clone.fetch_add(1, Ordering::Relaxed);
        }));

        feed.set_price(100);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        // Unsubscribe
        assert!(feed.unsubscribe(id));

        // Should no longer receive updates
        feed.set_price(200);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_unsubscribe_returns_false_for_unknown_id() {
        let feed = MockPriceFeed::new();
        assert!(!feed.unsubscribe(SubscriptionId(999)));
    }

    #[test]
    fn test_unsubscribe_idempotent() {
        let feed = MockPriceFeed::new();
        let id = feed.subscribe(Arc::new(|_: &PriceUpdate| {}));

        assert!(feed.unsubscribe(id));
        // Second call returns false — already removed
        assert!(!feed.unsubscribe(id));
    }

    #[test]
    fn test_unsubscribe_one_of_many() {
        let feed = MockPriceFeed::new();
        let count_a = Arc::new(AtomicUsize::new(0));
        let count_b = Arc::new(AtomicUsize::new(0));

        let count_a_clone = Arc::clone(&count_a);
        let id_a = feed.subscribe(Arc::new(move |_: &PriceUpdate| {
            count_a_clone.fetch_add(1, Ordering::Relaxed);
        }));

        let count_b_clone = Arc::clone(&count_b);
        let _id_b = feed.subscribe(Arc::new(move |_: &PriceUpdate| {
            count_b_clone.fetch_add(1, Ordering::Relaxed);
        }));

        feed.set_price(100);
        assert_eq!(count_a.load(Ordering::Relaxed), 1);
        assert_eq!(count_b.load(Ordering::Relaxed), 1);

        // Remove only listener A
        feed.unsubscribe(id_a);

        feed.set_price(200);
        assert_eq!(count_a.load(Ordering::Relaxed), 1); // unchanged
        assert_eq!(count_b.load(Ordering::Relaxed), 2); // still active
    }

    #[test]
    fn test_wire_feed_unsubscribe_disconnects() {
        let feed = MockPriceFeed::new();
        let calc = Arc::new(MarkPriceCalculator::with_default_config());

        let sub_id = wire_feed_to_calculator(&feed, Arc::clone(&calc));

        feed.set_price(50000);
        assert_eq!(calc.index_price(), 50000);

        // Disconnect wiring
        assert!(feed.unsubscribe(sub_id));

        // Further updates should NOT propagate
        feed.set_price(99999);
        assert_eq!(calc.index_price(), 50000); // unchanged
    }

    #[test]
    fn test_static_feed_unsubscribe_returns_false() {
        let feed = StaticPriceFeed::new(100, "static");
        let id = feed.subscribe(Arc::new(|_: &PriceUpdate| {}));
        assert!(!feed.unsubscribe(id));
    }

    // ── nanos_since_epoch ────────────────────────────────────────────────

    #[test]
    fn test_nanos_since_epoch_is_positive() {
        let ns = nanos_since_epoch();
        assert!(ns > 0);
    }
}
