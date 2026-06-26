//! Neutral index price feed abstraction.
//!
//! This module owns the [`IndexPriceFeed`] trait and the minimal value types its
//! signature requires ([`PriceUpdate`], [`PriceUpdateListener`],
//! [`SubscriptionId`]). It is intentionally **dependency-light**: it depends only
//! on `std`, with no link to the Greeks / mark-price subsystem.
//!
//! ## Why a neutral module
//!
//! The core order-book hierarchy is layered one-way: a layer composes the layer
//! below and aggregates upward, and the pricing subsystem (`greeks_engine`,
//! `greeks_aggregator`, `mark_price`, `index_price_feed`) *reads* the hierarchy —
//! the hierarchy must never depend on pricing.
//!
//! [`UnderlyingOrderBook`](super::underlying::UnderlyingOrderBook) (the top of the
//! core hierarchy) needs to hold a pluggable index source as
//! `Arc<dyn IndexPriceFeed>`. Keeping the trait in the pricing module
//! (`index_price_feed`) would invert that edge and force pricing to compile even
//! for pure book usage. Placing the trait here — alongside the other shared
//! lookup/state services (`symbol_index`, `instrument_registry`) that any layer
//! may depend on — lets the hierarchy reference the trait without pulling in
//! pricing.
//!
//! The concrete implementations ([`MockPriceFeed`](super::index_price_feed::MockPriceFeed),
//! [`StaticPriceFeed`](super::index_price_feed::StaticPriceFeed)) and the
//! calculator wiring
//! ([`wire_feed_to_calculator`](super::index_price_feed::wire_feed_to_calculator))
//! stay in the pricing module, which depends downward on this neutral trait.

use std::sync::Arc;

/// A price update from an external source.
///
/// Carries the price value, a nanosecond-precision timestamp for
/// observability, and the identifier of the source that produced it.
///
/// ## Fields
///
/// - `price`: Price in smallest units (e.g., satoshis, cents)
/// - `timestamp_ns`: Timestamp in nanoseconds since Unix epoch
/// - `source`: Human-readable source identifier (e.g., `"chainlink"`, `"binance"`)
#[derive(Debug, Clone)]
pub struct PriceUpdate {
    /// Price in smallest units (e.g., satoshis, cents).
    pub price: u64,
    /// Timestamp in nanoseconds since Unix epoch.
    pub timestamp_ns: u64,
    /// Identifier of the price source.
    pub source: String,
}

/// Opaque handle returned by [`IndexPriceFeed::subscribe`].
///
/// Pass this to [`IndexPriceFeed::unsubscribe`] to remove the listener.
/// Each id is unique within a single feed instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub(crate) u64);

/// Callback invoked when a new price update is available.
///
/// Receives a shared reference to the [`PriceUpdate`] to avoid cloning
/// when multiple listeners are registered.
///
/// ## Non-blocking, fast contract
///
/// A `PriceUpdateListener` **must be non-blocking and fast**. Implementations
/// such as [`MockPriceFeed`](super::index_price_feed::MockPriceFeed) invoke the
/// listener on a dedicated per-subscriber *background drain thread*, never
/// inline on the price producer's thread, so the producer is never stalled by a
/// slow listener. A listener that blocks (long I/O, lock contention, sleeping)
/// only slows *its own* delivery; other subscribers and the producer are
/// unaffected.
///
/// ### Drop / lag policy under overload
///
/// Delivery uses a **keep-latest** policy: each subscriber holds only the most
/// recent pending update. If a listener cannot keep up, intermediate updates
/// are coalesced (the freshest value wins) rather than queued unboundedly — so
/// a lagging listener observes the *most recent deliverable* updates, not every
/// intermediate tick. A listener must therefore tolerate gaps in the update
/// sequence and must not assume it sees every price.
///
/// # Panics
///
/// Listener implementations **must not panic**. A panic aborts that
/// subscriber's drain thread; other subscribers are unaffected. Callers are
/// responsible for ensuring their callbacks are panic-free.
///
/// # Re-entrancy
///
/// A listener may call back into the feed (including unsubscribing itself or
/// dropping the feed) from within its callback without deadlocking — teardown
/// detaches the current drain thread rather than joining it, so the thread
/// simply exits once the callback returns.
pub type PriceUpdateListener = Arc<dyn Fn(&PriceUpdate) + Send + Sync>;

/// Trait for external index price sources.
///
/// Implementations provide a latest-price query and a pub/sub model
/// for real-time updates. The trait is object-safe so it can be stored
/// as `Arc<dyn IndexPriceFeed>`.
///
/// ## Layering
///
/// This trait lives in a neutral, dependency-light module so the core
/// order-book hierarchy can store an `Arc<dyn IndexPriceFeed>` without
/// depending on the Greeks / mark-price subsystem. The concrete feeds and the
/// `MarkPriceCalculator` wiring live in the pricing module
/// (`index_price_feed`), which depends downward on this trait.
///
/// ## Thread Safety
///
/// All methods must be safe to call from any thread. Implementations
/// should use interior mutability (atomics, `Mutex`, etc.) as needed.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::{IndexPriceFeed, MockPriceFeed};
///
/// let feed = MockPriceFeed::new();
/// assert!(feed.latest_price().is_none()); // no price set yet
/// assert_eq!(feed.source(), "mock");
/// ```
pub trait IndexPriceFeed: Send + Sync {
    /// Returns the most recent price update, or `None` if no price
    /// has been published yet.
    fn latest_price(&self) -> Option<PriceUpdate>;

    /// Registers a listener that will be called on every subsequent
    /// price update.
    ///
    /// Returns a [`SubscriptionId`] that can be passed to
    /// [`unsubscribe`](Self::unsubscribe) to remove the listener.
    fn subscribe(&self, listener: PriceUpdateListener) -> SubscriptionId;

    /// Removes a previously registered listener.
    ///
    /// Returns `true` if the listener was found and removed, `false`
    /// if the id was not present (e.g., already unsubscribed).
    fn unsubscribe(&self, id: SubscriptionId) -> bool;

    /// Returns a human-readable identifier for this price source
    /// (e.g., `"chainlink"`, `"binance"`, `"mock"`).
    fn source(&self) -> &str;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    // ── PriceUpdate ──────────────────────────────────────────────────────

    #[test]
    fn test_price_update_creation() {
        let update = PriceUpdate {
            price: 50000,
            timestamp_ns: 1_000_000_000,
            source: "test".to_string(),
        };
        assert_eq!(update.price, 50000);
        assert_eq!(update.timestamp_ns, 1_000_000_000);
        assert_eq!(update.source, "test");
    }

    #[test]
    fn test_price_update_clone() {
        let update = PriceUpdate {
            price: 42000,
            timestamp_ns: 999,
            source: "clone_test".to_string(),
        };
        let cloned = update.clone();
        assert_eq!(cloned.price, 42000);
        assert_eq!(cloned.source, "clone_test");
    }

    #[test]
    fn test_price_update_debug() {
        let update = PriceUpdate {
            price: 100,
            timestamp_ns: 0,
            source: "dbg".to_string(),
        };
        let debug = format!("{:?}", update);
        assert!(debug.contains("PriceUpdate"));
        assert!(debug.contains("100"));
    }

    // ── SubscriptionId ───────────────────────────────────────────────────

    #[test]
    fn test_subscription_id_equality() {
        let a = SubscriptionId(1);
        let b = SubscriptionId(1);
        let c = SubscriptionId(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
