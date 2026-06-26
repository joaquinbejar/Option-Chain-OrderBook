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
//! ## Delivery model
//!
//! [`MockPriceFeed`] delivers updates **asynchronously**: each subscriber owns a
//! dedicated background drain thread that invokes its listener. The producer
//! ([`MockPriceFeed::set_price`]) only enqueues into a per-subscriber
//! latest-value mailbox and never calls a listener inline, so a slow listener
//! cannot stall the producer or any other subscriber. Under sustained overload
//! the policy is **keep-latest** (coalesce-to-latest): intermediate updates are
//! dropped in favour of the freshest value. See [`MockPriceFeed::set_price`].
//!
//! ## Example
//!
//! ```
//! use std::sync::Arc;
//! use std::time::Duration;
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
//! // Update feed — the calculator receives the price on a background drain
//! // thread, so wait briefly for the asynchronous delivery to land.
//! feed.set_price(50000);
//! for _ in 0..1000 {
//!     if calculator.index_price() == 50000 {
//!         break;
//!     }
//!     std::thread::sleep(Duration::from_millis(1));
//! }
//! assert_eq!(calculator.index_price(), 50000);
//! ```

use super::index_feed::{IndexPriceFeed, PriceUpdate, PriceUpdateListener, SubscriptionId};
use super::mark_price::MarkPriceCalculator;
use crate::utils::nanos_since_epoch;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Capacity of each subscriber's wakeup-signal channel.
///
/// One slot is sufficient: the channel only carries "a fresh update is pending"
/// wakeups, and additional wakeups coalesce (a `Full` send is dropped because a
/// wakeup is already queued). The actual price lives in the latest-value
/// mailbox, so this capacity does **not** bound how many price updates are
/// retained — only one (the freshest) is ever pending.
const SIGNAL_CHANNEL_CAPACITY: usize = 1;

/// Emit a throttled lag warning once every this many coalesced updates per
/// subscriber, so sustained overload does not produce a per-drop log storm.
const COALESCE_WARN_INTERVAL: u64 = 1024;

// ─── Subscriber ──────────────────────────────────────────────────────────────

/// Per-subscriber delivery state for [`MockPriceFeed`].
///
/// Each subscriber owns a latest-value mailbox, a bounded wakeup-signal channel,
/// and a dedicated background drain thread that invokes the listener. The
/// producer ([`MockPriceFeed::set_price`]) only overwrites the mailbox and pokes
/// the signal — it never runs the listener — so a slow listener cannot stall the
/// producer or any other subscriber.
struct Subscriber {
    /// Unique id handed back from [`IndexPriceFeed::subscribe`].
    id: SubscriptionId,
    /// Latest pending update (keep-latest). Overwritten on each `set_price`;
    /// the drain thread `take`s the freshest value. Older un-consumed updates
    /// are coalesced/dropped here.
    mailbox: Arc<Mutex<Option<PriceUpdate>>>,
    /// Bounded wakeup signal to the drain thread. `None` only after teardown.
    signal: Option<SyncSender<()>>,
    /// Count of coalesced (dropped) intermediate updates, for the lag metric.
    coalesced: Arc<AtomicU64>,
    /// Drain-thread handle, joined on teardown. `None` after a join or a failed
    /// spawn.
    handle: Option<JoinHandle<()>>,
}

impl Subscriber {
    /// Overwrites the latest-value mailbox and wakes the drain thread.
    ///
    /// Never blocks. The mailbox keeps only the most recent update — an
    /// un-consumed previous update is coalesced (keep-latest drop) and counted.
    /// The wakeup is a non-blocking `try_send`; a `Full` slot means a wakeup is
    /// already pending and this one coalesces into it.
    #[inline]
    fn enqueue(&self, update: &PriceUpdate) {
        // Clone outside the mailbox lock so the critical section is only the
        // pointer swap, never an allocation contended with the drain thread.
        let cloned = update.clone();
        let previous = match self.mailbox.lock() {
            Ok(mut guard) => guard.replace(cloned),
            Err(poisoned) => poisoned.into_inner().replace(cloned),
        };
        if previous.is_some() {
            // The drain thread had not yet consumed the prior update; it is now
            // coalesced into this fresher one (keep-latest).
            let n = self.coalesced.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(COALESCE_WARN_INTERVAL) {
                // Throttled: warn once per interval, never per drop.
                tracing::warn!(
                    subscription_id = self.id.0,
                    coalesced = n,
                    source = "mock",
                    "price-feed subscriber lagging; intermediate updates coalesced (keep-latest)",
                );
            }
        }
        if let Some(signal) = &self.signal {
            // Wake the drain thread without ever blocking the producer.
            //   Ok            → wakeup queued
            //   Err(Full)     → wakeup already pending; coalesced
            //   Err(Disconnected) → drain thread gone (failed spawn / shutdown)
            match signal.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
            }
        }
    }

    /// Stops the drain thread and joins it.
    ///
    /// Drops the signal sender so the drain thread's `recv` returns `Err` and
    /// its loop exits, then joins. Dropping the sender **before** the join is
    /// what guarantees the join terminates.
    fn shutdown_and_join(&mut self) {
        // Drop the only sender → drain `recv` returns `Err` → loop exits.
        self.signal = None;
        let Some(handle) = self.handle.take() else {
            return;
        };
        // Re-entrant teardown guard: a listener may unsubscribe itself (or drop
        // the feed) from inside its own callback, which runs on this drain
        // thread. Joining the current thread would deadlock — detach instead.
        // The sender was dropped above, so the loop exits and the thread unwinds
        // on its own once the callback returns.
        if handle.thread().id() == std::thread::current().id() {
            return;
        }
        if handle.join().is_err() {
            // The listener contract forbids panics; surface it if it happens.
            tracing::error!(
                subscription_id = self.id.0,
                source = "mock",
                "price-feed drain thread panicked during shutdown",
            );
        }
    }
}

/// Drain loop run on a subscriber's dedicated background thread.
///
/// Blocks on the wakeup signal; on each wake it delivers the freshest mailbox
/// value, looping until the mailbox drains so the most recent update always
/// wins even when several wakeups coalesced. `recv` returning `Err` (every
/// sender dropped) is the shutdown signal, and the loop exits.
fn drain_loop(
    signal_rx: &Receiver<()>,
    mailbox: &Mutex<Option<PriceUpdate>>,
    listener: &PriceUpdateListener,
) {
    while signal_rx.recv().is_ok() {
        loop {
            let pending = match mailbox.lock() {
                Ok(mut guard) => guard.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            };
            match pending {
                Some(update) => listener(&update),
                None => break,
            }
        }
    }
}

// ─── MockPriceFeed ───────────────────────────────────────────────────────────

/// Mock price feed for testing.
///
/// Allows programmatic price injection via [`set_price`](Self::set_price).
/// `set_price` updates the latest price (read by
/// [`latest_price`](IndexPriceFeed::latest_price)) and *asynchronously* delivers
/// the update to every subscriber.
///
/// ## Delivery model & drop/lag policy
///
/// Each subscriber owns a **latest-value mailbox**, a bounded wakeup
/// [`sync_channel`] of capacity 1 (`SIGNAL_CHANNEL_CAPACITY`), and a
/// dedicated background **drain thread** that invokes the listener.
/// [`set_price`](Self::set_price) only overwrites the mailbox and pokes the
/// signal (both non-blocking) — it **never** calls a listener inline — so a slow
/// or contended listener can never stall the producer or other subscribers.
///
/// The drop/lag policy is **keep-latest** (coalesce-to-latest): if a listener
/// falls behind, the mailbox retains only the freshest update and older
/// un-consumed updates are dropped/coalesced (counted per subscriber). For a
/// price feed the freshest value is what matters, so this is preferred over the
/// `sync_channel` default of dropping the *newest* on a full buffer (which would
/// make the mark lag behind on stale ticks). See [`set_price`](Self::set_price).
///
/// ## Thread Safety & lifecycle
///
/// Uses [`AtomicU64`] for the price and a [`Mutex`] for the subscriber list.
/// [`subscribe`](IndexPriceFeed::subscribe) spawns the drain thread;
/// [`unsubscribe`](IndexPriceFeed::unsubscribe) and `Drop` stop and join every
/// drain thread, so no threads leak and dropping the feed does not hang.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::{IndexPriceFeed, MockPriceFeed};
///
/// let feed = MockPriceFeed::new();
/// feed.set_price(42000);
///
/// // `latest_price` is updated synchronously by `set_price`.
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
    /// Registered subscribers, each with its own mailbox and drain thread.
    listeners: Mutex<Vec<Subscriber>>,
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

    /// Sets the current price and asynchronously delivers it to all subscribers.
    ///
    /// Updates [`latest_price`](IndexPriceFeed::latest_price) synchronously, then
    /// enqueues the update into each subscriber's latest-value mailbox and wakes
    /// its drain thread. This call **never blocks on a listener**: it never
    /// invokes a listener inline and only performs non-blocking mailbox writes
    /// and `try_send` wakeups.
    ///
    /// ## Drop / lag policy
    ///
    /// Delivery is **keep-latest** (coalesce-to-latest). Each subscriber retains
    /// only the most recent pending update; if its listener cannot keep up, the
    /// older un-consumed update is dropped/coalesced (counted, with a throttled
    /// `WARN` every `COALESCE_WARN_INTERVAL` drops). Under sustained overload a
    /// lagging subscriber therefore observes the *most recent deliverable*
    /// updates, not every intermediate tick. This deliberately departs from the
    /// `sync_channel` default of dropping the *newest* update on a full buffer,
    /// because for a price feed the freshest value is what drives the mark and
    /// processing stale intermediates first would make the mark lag.
    ///
    /// The timestamp is recorded as the current wall-clock time in nanoseconds
    /// since Unix epoch (observability only; the delivered price is the
    /// caller-supplied value).
    ///
    /// # Arguments
    ///
    /// * `price` - New price in smallest units
    pub fn set_price(&self, price: u64) {
        let ts = nanos_since_epoch();
        let update = PriceUpdate {
            price,
            timestamp_ns: ts,
            source: "mock".to_string(),
        };

        // Hold the lock only for cheap, non-blocking per-subscriber enqueues —
        // never across a listener invocation. Listeners run on their own drain
        // threads, so even if one is slow this loop returns promptly, and a
        // listener calling back into the feed cannot deadlock (it is never
        // invoked under this lock).
        //
        // Publish the atomic price *under* the lock, right before the mailbox
        // enqueues, so concurrent producers serialize on it: the last
        // lock-holder sets both `latest_price()` and every subscriber mailbox to
        // its own value. This keeps the synchronous `latest_price()` and the
        // async (mailbox-driven) mark price consistent — an older print can never
        // overwrite a newer one in a mailbox while `latest_price()` reports the
        // newer.
        let guard = match self.listeners.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.price.store(price, Ordering::Release);
        self.timestamp_ns.store(ts, Ordering::Release);
        for subscriber in guard.iter() {
            subscriber.enqueue(&update);
        }
    }
}

impl Default for MockPriceFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MockPriceFeed {
    fn drop(&mut self) {
        // Take ownership of every subscriber, then tear down its drain thread
        // outside the lock so no thread is leaked and the drop does not hang.
        let subscribers = {
            let mut guard = match self.listeners.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::take(&mut *guard)
        };
        for mut subscriber in subscribers {
            subscriber.shutdown_and_join();
        }
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

        let mailbox = Arc::new(Mutex::new(None::<PriceUpdate>));
        let coalesced = Arc::new(AtomicU64::new(0));
        let (signal_tx, signal_rx) = sync_channel::<()>(SIGNAL_CHANNEL_CAPACITY);

        // Spawn the dedicated drain thread. If the OS cannot create the thread,
        // degrade gracefully: the subscriber is registered (so the id stays
        // valid for `unsubscribe`) but receives no updates, and an ERROR is
        // logged. The dropped `signal_rx` makes later `try_send`s disconnect.
        let drain_mailbox = Arc::clone(&mailbox);
        let handle = match std::thread::Builder::new()
            .name(format!("price-feed-drain-{}", id.0))
            .spawn(move || drain_loop(&signal_rx, &drain_mailbox, &listener))
        {
            Ok(handle) => Some(handle),
            Err(error) => {
                tracing::error!(
                    subscription_id = id.0,
                    source = "mock",
                    %error,
                    "failed to spawn price-feed drain thread; subscriber will not receive updates",
                );
                None
            }
        };

        let subscriber = Subscriber {
            id,
            mailbox,
            signal: Some(signal_tx),
            coalesced,
            handle,
        };
        match self.listeners.lock() {
            Ok(mut listeners) => listeners.push(subscriber),
            Err(poisoned) => poisoned.into_inner().push(subscriber),
        }

        // Cold path: feed wiring, not the per-tick `set_price` notification.
        tracing::info!(
            subscription_id = id.0,
            source = "mock",
            "index feed subscribed"
        );
        id
    }

    fn unsubscribe(&self, id: SubscriptionId) -> bool {
        // Remove the subscriber under the lock, then tear its drain thread down
        // OUTSIDE the lock — joining a drain thread can block on an in-flight
        // listener call, and holding the listeners lock there would stall
        // `set_price`. Removal first guarantees no further updates are enqueued.
        let removed = {
            let mut guard = match self.listeners.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard
                .iter()
                .position(|subscriber| subscriber.id == id)
                .map(|pos| guard.remove(pos))
        };

        match removed {
            Some(mut subscriber) => {
                subscriber.shutdown_and_join();
                tracing::info!(
                    subscription_id = id.0,
                    removed = true,
                    source = "mock",
                    "index feed unsubscribed",
                );
                true
            }
            None => {
                tracing::info!(
                    subscription_id = id.0,
                    removed = false,
                    source = "mock",
                    "index feed unsubscribed",
                );
                false
            }
        }
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
/// use std::time::Duration;
/// use option_chain_orderbook::orderbook::{
///     IndexPriceFeed, MockPriceFeed, MarkPriceCalculator, wire_feed_to_calculator,
/// };
///
/// let feed = Arc::new(MockPriceFeed::new());
/// let calc = Arc::new(MarkPriceCalculator::with_default_config());
///
/// let sub_id = wire_feed_to_calculator(feed.as_ref(), Arc::clone(&calc));
///
/// // The update is delivered on a background drain thread, so wait briefly.
/// feed.set_price(99000);
/// for _ in 0..1000 {
///     if calc.index_price() == 99000 {
///         break;
///     }
///     std::thread::sleep(Duration::from_millis(1));
/// }
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

    let subscription_id = feed.subscribe(listener);
    // Cold path: one-time wiring of a feed to a mark-price calculator.
    tracing::info!(
        subscription_id = subscription_id.0,
        source = feed.source(),
        "index feed wired to mark-price calculator",
    );
    subscription_id
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::{Duration, Instant};

    /// Generous upper bound for any single async delivery to land.
    const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

    /// Polls `predicate` until it holds or `timeout` elapses.
    ///
    /// Delivery is asynchronous (a background drain thread), so tests must wait
    /// for a side effect to land. Polling with a generous timeout — rather than
    /// a fixed sleep then assert — keeps these tests deterministic and not flaky.
    fn wait_until<F: Fn() -> bool>(timeout: Duration, predicate: F) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        predicate()
    }

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
        // The listener pushes to a channel so the test can deterministically
        // wait for the asynchronous delivery via `recv_timeout`.
        let (tx, rx) = channel::<u64>();
        feed.subscribe(Arc::new(move |update: &PriceUpdate| {
            let _ = tx.send(update.price);
        }));

        feed.set_price(55000);
        let received = rx.recv_timeout(DELIVERY_TIMEOUT).unwrap();
        assert_eq!(received, 55000);
    }

    #[test]
    fn test_mock_feed_multiple_subscribers() {
        let feed = MockPriceFeed::new();
        let total = Arc::new(AtomicUsize::new(0));
        // Track the latest value each subscriber has observed.
        let latest: Vec<Arc<AtomicU64>> = (0..3).map(|_| Arc::new(AtomicU64::new(0))).collect();

        for slot in &latest {
            let total_clone = Arc::clone(&total);
            let slot_clone = Arc::clone(slot);
            feed.subscribe(Arc::new(move |update: &PriceUpdate| {
                // Count first so that, once the latest value is observable, the
                // delivery that produced it is already reflected in `total`.
                total_clone.fetch_add(1, Ordering::Relaxed);
                slot_clone.store(update.price, Ordering::Release);
            }));
        }

        feed.set_price(100);
        feed.set_price(200);

        // Keep-latest: each subscriber eventually converges to the freshest
        // value, even if the intermediate 100 was coalesced.
        assert!(wait_until(DELIVERY_TIMEOUT, || {
            latest.iter().all(|s| s.load(Ordering::Acquire) == 200)
        }));

        // Coalescing only reduces deliveries: between 3 (each saw only 200) and
        // 6 (each saw both 100 and 200).
        let total = total.load(Ordering::Relaxed);
        assert!(
            (3..=6).contains(&total),
            "unexpected delivery count {total}"
        );
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

        // Let each delivery land before sending the next, so the keep-latest
        // coalescing does not skip an intermediate value we are asserting on.
        feed.set_price(60000);
        assert!(wait_until(DELIVERY_TIMEOUT, || calc.index_price() == 60000));

        feed.set_price(61000);
        assert!(wait_until(DELIVERY_TIMEOUT, || calc.index_price() == 61000));
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

        // Final price is some valid value (non-deterministic which thread wins).
        assert!(feed.latest_price().is_some());

        // Delivery is asynchronous and keep-latest: the drain thread coalesces,
        // so the number of deliveries is at most the 4 × 50 = 200 updates and at
        // least one. Wait for the drain to quiesce, then assert the bound.
        assert!(wait_until(DELIVERY_TIMEOUT, || total
            .load(Ordering::Relaxed)
            >= 1));
        let total = total.load(Ordering::Relaxed);
        assert!(
            (1..=200).contains(&total),
            "unexpected delivery count {total}"
        );
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

        // Calculator should eventually receive a delivered update.
        assert!(wait_until(DELIVERY_TIMEOUT, || calc.index_price() > 0));
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
        // Wait for the first delivery to land before unsubscribing.
        assert!(wait_until(DELIVERY_TIMEOUT, || count
            .load(Ordering::Relaxed)
            == 1));

        // Unsubscribe joins the drain thread, so no further delivery is possible.
        assert!(feed.unsubscribe(id));

        // Should no longer receive updates — deterministic, the thread is gone.
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
        // Wait for both deliveries before unsubscribing A.
        assert!(wait_until(DELIVERY_TIMEOUT, || {
            count_a.load(Ordering::Relaxed) == 1 && count_b.load(Ordering::Relaxed) == 1
        }));

        // Remove only listener A — joins A's drain thread.
        feed.unsubscribe(id_a);

        feed.set_price(200);
        // B is still active and converges to a second delivery.
        assert!(wait_until(DELIVERY_TIMEOUT, || count_b
            .load(Ordering::Relaxed)
            == 2));
        // A's thread is gone, so its count cannot change.
        assert_eq!(count_a.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_wire_feed_unsubscribe_disconnects() {
        let feed = MockPriceFeed::new();
        let calc = Arc::new(MarkPriceCalculator::with_default_config());

        let sub_id = wire_feed_to_calculator(&feed, Arc::clone(&calc));

        feed.set_price(50000);
        assert!(wait_until(DELIVERY_TIMEOUT, || calc.index_price() == 50000));

        // Disconnect wiring — joins the drain thread.
        assert!(feed.unsubscribe(sub_id));

        // Further updates should NOT propagate — deterministic, thread is gone.
        feed.set_price(99999);
        assert_eq!(calc.index_price(), 50000); // unchanged
    }

    #[test]
    fn test_static_feed_unsubscribe_returns_false() {
        let feed = StaticPriceFeed::new(100, "static");
        let id = feed.subscribe(Arc::new(|_: &PriceUpdate| {}));
        assert!(!feed.unsubscribe(id));
    }

    // ── Slow-listener isolation (non-blocking feed) ──────────────────────

    #[test]
    fn test_slow_subscriber_does_not_stall_producer_or_others() {
        /// Per-call sleep of the slow listener.
        const SLOW_DELAY_MS: u64 = 25;
        /// Number of rapid producer updates.
        const N: u64 = 20;

        let feed = MockPriceFeed::new();

        // Fast subscriber: records the latest price it has seen.
        let fast_latest = Arc::new(AtomicU64::new(0));
        let fast_clone = Arc::clone(&fast_latest);
        feed.subscribe(Arc::new(move |update: &PriceUpdate| {
            fast_clone.store(update.price, Ordering::Release);
        }));

        // Slow subscriber: each callback sleeps, so it cannot keep up and must
        // coalesce (keep-latest) rather than block the producer.
        let slow_latest = Arc::new(AtomicU64::new(0));
        let slow_count = Arc::new(AtomicUsize::new(0));
        let slow_latest_clone = Arc::clone(&slow_latest);
        let slow_count_clone = Arc::clone(&slow_count);
        feed.subscribe(Arc::new(move |update: &PriceUpdate| {
            thread::sleep(Duration::from_millis(SLOW_DELAY_MS));
            slow_count_clone.fetch_add(1, Ordering::Relaxed);
            slow_latest_clone.store(update.price, Ordering::Release);
        }));

        // Producer: many rapid updates. With a synchronous fan-out this loop
        // would take >= N * SLOW_DELAY_MS = 500ms; it must return promptly.
        let start = Instant::now();
        for i in 1..=N {
            feed.set_price(i * 1000);
        }
        let producer_elapsed = start.elapsed();

        let final_price = N * 1000;
        let sync_fanout = Duration::from_millis(N * SLOW_DELAY_MS);

        // The producer was NOT blocked by the slow listener: a generous bound
        // far below the synchronous-fan-out cost, with margin against CI noise.
        assert!(
            producer_elapsed < sync_fanout / 2,
            "set_price loop took {producer_elapsed:?}; producer blocked by slow listener \
             (synchronous fan-out would be {sync_fanout:?})",
        );

        // The fast co-subscriber converges to the freshest price promptly,
        // unaffected by the slow one.
        assert!(
            wait_until(DELIVERY_TIMEOUT, || {
                fast_latest.load(Ordering::Acquire) == final_price
            }),
            "fast subscriber did not converge to the latest price",
        );

        // The slow subscriber also converges to the freshest price eventually
        // (keep-latest), but with FEWER deliveries than N because intermediate
        // ticks were coalesced/dropped while it was busy.
        assert!(
            wait_until(DELIVERY_TIMEOUT, || {
                slow_latest.load(Ordering::Acquire) == final_price
            }),
            "slow subscriber did not eventually converge to the latest price",
        );
        let slow = slow_count.load(Ordering::Relaxed) as u64;
        assert!(slow >= 1, "slow subscriber received no updates");
        assert!(
            slow < N,
            "slow subscriber did not coalesce any updates (got {slow} of {N})",
        );
    }

    #[test]
    fn test_drop_tears_down_drain_threads_without_hanging() {
        // Dropping a feed with a (briefly) slow subscriber must join every
        // drain thread cleanly and promptly — no leaked threads, no hang. Run
        // the drop on a worker and assert it completes within a bound.
        let (done_tx, done_rx) = channel::<()>();
        thread::spawn(move || {
            let feed = MockPriceFeed::new();
            feed.subscribe(Arc::new(|_: &PriceUpdate| {
                thread::sleep(Duration::from_millis(10));
            }));
            feed.set_price(123);
            drop(feed); // tears down + joins the drain thread
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(DELIVERY_TIMEOUT).is_ok(),
            "dropping the feed hung instead of joining its drain thread",
        );
    }

    // ── nanos_since_epoch ────────────────────────────────────────────────

    #[test]
    fn test_nanos_since_epoch_is_positive() {
        let ns = nanos_since_epoch();
        assert!(ns > 0);
    }
}
