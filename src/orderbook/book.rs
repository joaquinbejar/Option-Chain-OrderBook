//! Option order book wrapper.
//!
//! This module provides the [`OptionOrderBook`] structure that wraps the
//! OrderBook-rs `OrderBook<T>` implementation with option-specific functionality.

use super::instrument_status::InstrumentStatus;
use super::quote::Quote;
use super::validation::ValidationConfig;
use crate::Result;
use crate::error::Error;
use optionstratlib::OptionStyle;
#[cfg(feature = "nats")]
use orderbook_rs::PriceLevelChangedListener;
use orderbook_rs::{
    Clock, DefaultOrderBook, FeeSchedule, MassCancelResult, OrderBookSnapshot, OrderId,
    OrderStateTracker, OrderStatus, OrderType, STPMode, Side, TimeInForce, TradeListener,
    TradeResult,
};
use pricelevel::{Hash32, MatchResult, OrderUpdate, Price, Quantity, TimestampMs};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Pre-built NATS publisher listeners to install on the inner order book
/// *before* it is wrapped in `Arc`.
///
/// `orderbook_rs` only allows a single trade listener and a single book-change
/// listener, and both must be registered while the `OrderBook<T>` is still
/// owned mutably (pre-`Arc`). This struct ferries the already-constructed
/// listeners — typed purely as `orderbook_rs`-native callbacks — into
/// [`OptionOrderBook::new_with_config`] so the leaf never imports the eventing
/// `nats` module. The dependency direction is therefore `nats` → `book`, never
/// the reverse.
///
/// Constructed by the `nats` module's `build_option_order_book_with_nats`
/// free function (kept as a plain reference, not an intra-doc link, so the leaf
/// carries no path into the eventing layer).
#[cfg(feature = "nats")]
#[derive(Clone)]
pub(crate) struct PreparedNatsListeners {
    /// Trade publisher listener, multiplexed with the internal trade-capture
    /// listener into a single [`TradeListener`].
    pub trade_listener: TradeListener,
    /// Book-change publisher listener installed via `set_price_level_listener`.
    pub book_listener: PriceLevelChangedListener,
}

/// Factory that builds the per-contract NATS publisher listeners for one option
/// contract, given its symbol.
///
/// The hierarchy threads this factory down to the leaf so that every contract
/// book it lazily creates installs its own publishers *before* the inner book
/// is wrapped in `Arc` (the only valid install point — the `orderbook_rs`
/// listener setters take `&mut self`). The factory is typed purely in terms of
/// `orderbook_rs`-native listeners (via [`PreparedNatsListeners`]) so the core
/// hierarchy never imports the eventing `nats` module: it only invokes this
/// `book`-layer callback with each contract's symbol and installs whatever
/// listeners come back. The dependency direction stays `nats` → hierarchy →
/// `book`, never the reverse.
///
/// Returns `None` when no publishers should be attached for the given symbol
/// (e.g. the symbol cannot be turned into a valid subject); the contract book
/// is then built exactly as on the non-NATS path.
///
/// The concrete factory is constructed by the `nats` module's
/// `build_underlying_manager_with_nats` builder (kept as a plain reference, not
/// an intra-doc link, so the leaf carries no path into the eventing layer).
#[cfg(feature = "nats")]
pub(crate) type ContractNatsListenerFactory =
    Arc<dyn Fn(&str) -> Option<PreparedNatsListeners> + Send + Sync>;

/// Thread-safe holder for an optional [`ContractNatsListenerFactory`].
///
/// Kept as a bespoke holder rather than the generic
/// [`Shared`](super::shared::Shared)`<T>`: the inner factory is an
/// `Arc<dyn Fn(..) -> ..>` which is not `Debug`, so this type carries a custom
/// `Debug` that surfaces only whether a factory is configured (a `bool`) instead
/// of the inner value. Like the generic holder, managers store the factory
/// behind a lock so it can be propagated to children through `&self` setters —
/// exactly like the STP mode and fee schedule — without threading it through
/// every constructor signature (keeping the public constructor surface
/// additive). The factory is an `Arc`, so [`get`](Self::get) is a cheap clone.
#[cfg(feature = "nats")]
#[derive(Default)]
pub(crate) struct SharedNatsFactory {
    /// The inner factory, protected by a read-write lock.
    inner: std::sync::RwLock<Option<ContractNatsListenerFactory>>,
}

#[cfg(feature = "nats")]
impl SharedNatsFactory {
    /// Creates an empty holder (no factory configured).
    #[must_use]
    #[inline]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Stores (or clears) the factory. Recovers from a poisoned lock so the
    /// factory is always written.
    pub(crate) fn set(&self, factory: Option<ContractNatsListenerFactory>) {
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = factory;
    }

    /// Returns a clone of the current factory, or `None` if none is configured.
    #[must_use]
    pub(crate) fn get(&self) -> Option<ContractNatsListenerFactory> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(feature = "nats")]
impl std::fmt::Debug for SharedNatsFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedNatsFactory")
            .field("configured", &self.get().is_some())
            .finish()
    }
}

/// Internal configuration for constructing an [`OptionOrderBook`].
///
/// Consolidates all optional configuration (instrument ID, validation,
/// STP mode, fee schedule) into a single struct, avoiding constructor
/// explosion as new features are added.
#[derive(Clone)]
pub(crate) struct BookConfig {
    /// Numeric instrument ID (0 for standalone books).
    pub instrument_id: u32,
    /// Optional pre-trade validation rules.
    pub validation: Option<ValidationConfig>,
    /// Self-trade prevention mode ([`STPMode::None`] by default).
    pub stp_mode: STPMode,
    /// Optional fee schedule for maker/taker fees.
    pub fee_schedule: Option<FeeSchedule>,
    /// Whether to enable order state tracking (default: true).
    ///
    /// Enabling state tracking introduces a small overhead per order operation
    /// due to status recording. For extreme low-latency scenarios where every
    /// microsecond matters, set this to `false` to disable tracking entirely.
    pub enable_state_tracking: bool,
    /// Engine clock installed on the inner book before it is frozen in `Arc`.
    ///
    /// `None` keeps the upstream default `MonotonicClock` (wall-clock time).
    /// Inject a deterministic clock (e.g. `StubClock`) to make time-in-force
    /// admission (`GTD`/`Day`) reproducible for sequencer replay.
    pub clock: Option<Arc<dyn Clock>>,
    /// Pre-built NATS publisher listeners installed before the inner book is
    /// wrapped in `Arc` (feature `nats`). `None` for every non-NATS path.
    #[cfg(feature = "nats")]
    pub nats_listeners: Option<PreparedNatsListeners>,
}

impl std::fmt::Debug for BookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("BookConfig");
        dbg.field("instrument_id", &self.instrument_id)
            .field("validation", &self.validation)
            .field("stp_mode", &self.stp_mode)
            .field("fee_schedule", &self.fee_schedule)
            .field("enable_state_tracking", &self.enable_state_tracking)
            .field("clock", &self.clock);
        // The NATS listeners are boxed closures and carry no `Debug`; surface
        // only whether they are present so the impl stays informative.
        #[cfg(feature = "nats")]
        dbg.field("nats_listeners", &self.nats_listeners.is_some());
        dbg.finish()
    }
}

impl Default for BookConfig {
    fn default() -> Self {
        Self {
            instrument_id: 0,
            validation: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
            enable_state_tracking: true,
            clock: None,
            #[cfg(feature = "nats")]
            nats_listeners: None,
        }
    }
}

/// Cumulative counts of terminal order transitions.
///
/// These counts represent the lifetime totals of orders that have transitioned
/// to each terminal state. They are not adjusted when terminal states are
/// purged or evicted from the tracker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalOrderSummary {
    /// Number of orders that reached the Filled state.
    pub filled: usize,
    /// Number of orders that reached the Cancelled state.
    pub cancelled: usize,
    /// Number of orders that reached the Rejected state.
    pub rejected: usize,
}

impl TerminalOrderSummary {
    /// Returns the total number of terminal orders.
    #[must_use]
    #[inline]
    pub fn total(&self) -> usize {
        self.filled
            .saturating_add(self.cancelled)
            .saturating_add(self.rejected)
    }
}

impl std::ops::Add for TerminalOrderSummary {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            filled: self.filled.saturating_add(other.filled),
            cancelled: self.cancelled.saturating_add(other.cancelled),
            rejected: self.rejected.saturating_add(other.rejected),
        }
    }
}

impl std::iter::Sum for TerminalOrderSummary {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |acc, x| acc + x)
    }
}

/// Internal atomic counters for terminal state tracking via listener.
pub(crate) struct TerminalCounters {
    filled: AtomicUsize,
    cancelled: AtomicUsize,
    rejected: AtomicUsize,
}

impl TerminalCounters {
    /// Creates a new set of zero-initialized counters.
    pub fn new() -> Self {
        Self {
            filled: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
            rejected: AtomicUsize::new(0),
        }
    }

    /// Increments the appropriate counter based on the order status.
    #[inline]
    pub fn increment(&self, status: &OrderStatus) {
        match status {
            OrderStatus::Filled { .. } => {
                self.filled.fetch_add(1, Ordering::Relaxed);
            }
            OrderStatus::Cancelled { .. } => {
                self.cancelled.fetch_add(1, Ordering::Relaxed);
            }
            OrderStatus::Rejected { .. } => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Returns a snapshot of the current counts.
    #[must_use]
    pub fn snapshot(&self) -> TerminalOrderSummary {
        TerminalOrderSummary {
            filled: self.filled.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}

/// Order book for a single option contract.
///
/// Wraps the high-performance `OrderBook<T>` from OrderBook-rs and provides
/// option-specific functionality. The underlying OrderBook uses `u64` for
/// prices (representing price in smallest units, e.g., cents or satoshis).
///
/// ## Architecture
///
/// This struct sits at the bottom of the option chain hierarchy:
/// ```text
/// UnderlyingOrderBookManager
///   └── UnderlyingOrderBook
///         └── ExpirationOrderBookManager
///               └── ExpirationOrderBook
///                     └── OptionChainOrderBook
///                           └── StrikeOrderBook
///                                 └── OptionOrderBook ← This struct
///                                       └── OrderBook<T> (from OrderBook-rs)
/// ```
pub struct OptionOrderBook {
    /// The option contract symbol.
    symbol: String,
    /// Hash of the symbol for efficient comparison.
    symbol_hash: u64,
    /// The underlying order book from OrderBook-rs.
    book: Arc<DefaultOrderBook>,
    /// The option style (Call or Put).
    option_style: OptionStyle,
    /// Unique identifier for this order book.
    id: OrderId,
    /// Lifecycle status of this instrument, stored as atomic u8.
    status: AtomicU8,
    /// Numeric instrument ID for fast lookups and compact wire representation.
    /// Stored as `AtomicU32` so it can be assigned after construction
    /// without requiring `&mut self`.
    instrument_id: AtomicU32,
    /// Captured trade result from the last order submission, for the opt-in
    /// continuous-capture accessor [`last_trade_result`](Self::last_trade_result).
    ///
    /// Populated by the internal trade listener when a match occurs *and*
    /// continuous trade capture is armed. This is a single-slot, last-write-wins
    /// poll and is **not** used by the `_full` order methods, which attribute
    /// their fills per-call from the engine's
    /// `add_*_with_result` return value (no shared slot).
    last_trade_result: Arc<Mutex<Option<TradeResult>>>,
    /// Cheap armed flag gating the continuous-capture clone+lock on the match
    /// hot path.
    ///
    /// Disarmed by default: the always-installed trade listener performs a
    /// single relaxed atomic load and returns without cloning the
    /// [`TradeResult`] or taking the capture lock.
    /// [`arm_trade_capture`](Self::arm_trade_capture) opts a book into continuous
    /// capture for [`last_trade_result`](Self::last_trade_result). The `_full`
    /// order methods never touch this flag.
    trade_capture_armed: Arc<AtomicBool>,
    /// Cumulative terminal order counters maintained by the state listener.
    ///
    /// `None` when state tracking is disabled via `BookConfig`.
    terminal_counters: Option<Arc<TerminalCounters>>,
    /// Crate-side maximum order price (smallest price units); orders priced
    /// above are rejected before reaching the engine. The upstream engine has
    /// no price-bound hook, so this bound cannot be delegated and is enforced
    /// here. `None` disables the upper bound.
    max_price: Option<u128>,
    /// Crate-side minimum order price (smallest price units); orders priced
    /// below are rejected before reaching the engine. Same rationale as
    /// [`max_price`](Self::max_price): the engine has no price-bound hook.
    /// `None` disables the lower bound. Together the two form an inclusive
    /// `[min_price, max_price]` band checked in
    /// [`check_price_band`](Self::check_price_band).
    min_price: Option<u128>,
}

impl OptionOrderBook {
    // ── Core constructor ────────────────────────────────────────────────

    /// Creates an option order book from a [`BookConfig`].
    ///
    /// This is the single internal constructor that all other constructors
    /// delegate to. It creates a `DefaultOrderBook`, applies STP, validation,
    /// and fee schedule, installs the trade-capture listener, and wraps the
    /// result in `Arc`.
    #[must_use]
    pub(crate) fn new_with_config(
        symbol: impl Into<String>,
        option_style: OptionStyle,
        config: BookConfig,
    ) -> Self {
        let symbol = symbol.into();
        let symbol_hash = Self::hash_symbol(&symbol);

        let mut book = if config.stp_mode != STPMode::None {
            DefaultOrderBook::with_stp_mode(&symbol, config.stp_mode)
        } else {
            DefaultOrderBook::new(&symbol)
        };

        if let Some(ref validation) = config.validation {
            Self::apply_validation(&mut book, validation);
        }
        // The price band is enforced crate-side (no engine hook), so keep a copy
        // of each bound on the book rather than delegating it to the inner order
        // book.
        let max_price = config
            .validation
            .as_ref()
            .and_then(ValidationConfig::max_price);
        let min_price = config
            .validation
            .as_ref()
            .and_then(ValidationConfig::min_price);
        if let Some(schedule) = config.fee_schedule {
            book.set_fee_schedule(Some(schedule));
        }
        // Install the injected engine clock (if any) while the book is still
        // owned mutably, before it is frozen in `Arc`. This works for both
        // constructor branches above (STP and non-STP). `None` keeps the
        // upstream default `MonotonicClock`.
        if let Some(clock) = config.clock.clone() {
            book.set_clock(clock);
        }

        // Install the continuous-capture listener backing the opt-in
        // `arm_trade_capture` / `last_trade_result` accessor. The clone+lock is
        // gated behind a single cheap relaxed atomic (`armed`), so the common
        // (unarmed) match path pays only the atomic load and never clones the
        // TradeResult or takes the lock. This slot is independent of the `_full`
        // order methods, which attribute their fills per-call from the engine's
        // `add_*_with_result` return value.
        let capture: Arc<Mutex<Option<TradeResult>>> = Arc::new(Mutex::new(None));
        let armed: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let capture_clone = Arc::clone(&capture);
        let armed_clone = Arc::clone(&armed);
        let capture_listener: TradeListener = Arc::new(move |tr: &TradeResult| {
            // Record only while continuous capture is armed.
            if !armed_clone.load(Ordering::Relaxed) {
                return;
            }
            let mut guard = capture_clone
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(tr.clone());
        });

        // When NATS publishers were prepared (feature `nats`), multiplex the
        // trade-capture listener with the NATS trade publisher into the single
        // `TradeListener` that `orderbook_rs` supports, and install the
        // book-change publisher listener. This is done here — before the book
        // is wrapped in `Arc` — because that is the only window in which the
        // inner `OrderBook<T>` can have listeners registered.
        #[cfg(feature = "nats")]
        match config.nats_listeners {
            Some(prepared) => {
                let nats_trade = prepared.trade_listener;
                book.set_trade_listener(Arc::new(move |tr: &TradeResult| {
                    capture_listener(tr);
                    nats_trade(tr);
                }));
                book.set_price_level_listener(prepared.book_listener);
            }
            None => book.set_trade_listener(capture_listener),
        }
        #[cfg(not(feature = "nats"))]
        book.set_trade_listener(capture_listener);

        // Install order state tracker for lifecycle tracking (enabled by default)
        let terminal_counters = if config.enable_state_tracking {
            let counters = Arc::new(TerminalCounters::new());
            let counters_clone = Arc::clone(&counters);

            let mut tracker = OrderStateTracker::new();
            tracker.set_listener(Arc::new(move |_id, _old, new| {
                // Count when reaching terminal states. The upstream tracker
                // currently emits one transition per order, so double-counting
                // is not a concern with the current orderbook-rs behavior.
                if new.is_terminal() {
                    counters_clone.increment(new);
                }
            }));
            book.set_order_state_tracker(tracker);
            Some(counters)
        } else {
            None
        };

        Self {
            symbol,
            symbol_hash,
            book: Arc::new(book),
            option_style,
            id: OrderId::new(),
            status: AtomicU8::new(InstrumentStatus::Active as u8),
            instrument_id: AtomicU32::new(config.instrument_id),
            last_trade_result: capture,
            trade_capture_armed: armed,
            terminal_counters,
            max_price,
            min_price,
        }
    }

    // ── Public constructors ─────────────────────────────────────────────

    /// Creates a new option order book for the given symbol.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The option contract symbol (e.g., "BTC-20240329-50000-C")
    /// * `option_style` - The option style (Call or Put)
    #[must_use]
    pub fn new(symbol: impl Into<String>, option_style: OptionStyle) -> Self {
        Self::new_with_config(symbol, option_style, BookConfig::default())
    }

    /// Creates a new option order book with a pre-assigned instrument ID.
    ///
    /// Used internally by the hierarchy when an [`InstrumentRegistry`](super::instrument_registry::InstrumentRegistry)
    /// is available.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The option contract symbol
    /// * `option_style` - The option style (Call or Put)
    /// * `instrument_id` - The unique numeric instrument ID
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn new_with_id(
        symbol: impl Into<String>,
        option_style: OptionStyle,
        instrument_id: u32,
    ) -> Self {
        Self::new_with_config(
            symbol,
            option_style,
            BookConfig {
                instrument_id,
                ..BookConfig::default()
            },
        )
    }

    /// Creates a new option order book with pre-trade validation configured.
    ///
    /// Validation rules are applied to the underlying `OrderBook` before it is
    /// wrapped in `Arc`, so they cannot be changed after construction.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The option contract symbol (e.g., "BTC-20240329-50000-C")
    /// * `option_style` - The option style (Call or Put)
    /// * `config` - Validation configuration (tick size, lot size, min/max order size)
    #[must_use]
    pub fn new_with_validation(
        symbol: impl Into<String>,
        option_style: OptionStyle,
        config: &ValidationConfig,
    ) -> Self {
        Self::new_with_config(
            symbol,
            option_style,
            BookConfig {
                validation: Some(config.clone()),
                ..BookConfig::default()
            },
        )
    }

    /// Creates a new option order book with STP mode configured.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The option contract symbol
    /// * `option_style` - The option style (Call or Put)
    /// * `stp_mode` - Self-trade prevention mode
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn new_with_stp(
        symbol: impl Into<String>,
        option_style: OptionStyle,
        stp_mode: STPMode,
    ) -> Self {
        Self::new_with_config(
            symbol,
            option_style,
            BookConfig {
                stp_mode,
                ..BookConfig::default()
            },
        )
    }

    /// Applies validation config to a mutable order book before wrapping in `Arc`.
    fn apply_validation(book: &mut DefaultOrderBook, config: &ValidationConfig) {
        if let Some(tick) = config.tick_size() {
            book.set_tick_size(tick);
        }
        if let Some(lot) = config.lot_size() {
            book.set_lot_size(lot);
        }
        if let Some(min) = config.min_order_size() {
            book.set_min_order_size(min);
        }
        if let Some(max) = config.max_order_size() {
            book.set_max_order_size(max);
        }
        // The price band (`min_price` / `max_price`) is intentionally NOT
        // delegated to the engine: `orderbook_rs` exposes no price-bound setter,
        // so the bounds are stored on the leaf and enforced crate-side in
        // `check_price_band`.
    }

    /// Returns the current validation configuration read back from the underlying book,
    /// or `None` if no validation rules are configured.
    #[must_use]
    pub fn validation_config(&self) -> Option<ValidationConfig> {
        let mut config = ValidationConfig::new();
        if let Some(tick) = self.book.tick_size() {
            config = config.with_tick_size(tick);
        }
        if let Some(lot) = self.book.lot_size() {
            config = config.with_lot_size(lot);
        }
        if let Some(min) = self.book.min_order_size() {
            config = config.with_min_order_size(min);
        }
        if let Some(max) = self.book.max_order_size() {
            config = config.with_max_order_size(max);
        }
        // The price band lives on the leaf, not the engine, so merge both bounds
        // back in before the emptiness check so a book configured with only a
        // band still reports its config.
        if let Some(bound) = self.max_price {
            config = config.with_max_price(bound);
        }
        if let Some(bound) = self.min_price {
            config = config.with_min_price(bound);
        }
        if config.is_empty() {
            None
        } else {
            Some(config)
        }
    }

    /// Returns the option style (Call or Put).
    #[must_use]
    pub const fn option_style(&self) -> OptionStyle {
        self.option_style
    }

    /// Computes a hash for the symbol.
    fn hash_symbol(symbol: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        symbol.hash(&mut hasher);
        hasher.finish()
    }

    /// Returns the option contract symbol.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Returns the symbol hash.
    #[must_use]
    pub const fn symbol_hash(&self) -> u64 {
        self.symbol_hash
    }

    /// Returns the unique identifier for this order book.
    #[must_use]
    pub const fn id(&self) -> OrderId {
        self.id
    }

    /// Returns a reference to the underlying OrderBook from OrderBook-rs.
    #[must_use]
    pub fn inner(&self) -> &DefaultOrderBook {
        &self.book
    }

    /// Returns an Arc reference to the underlying OrderBook.
    #[must_use]
    pub fn inner_arc(&self) -> Arc<DefaultOrderBook> {
        Arc::clone(&self.book)
    }

    /// Returns the numeric instrument ID.
    ///
    /// Returns 0 for standalone books created outside the hierarchy.
    /// Hierarchy-created books get unique IDs from the
    /// [`InstrumentRegistry`](super::instrument_registry::InstrumentRegistry).
    #[must_use]
    #[inline]
    pub fn instrument_id(&self) -> u32 {
        self.instrument_id.load(Ordering::Relaxed)
    }

    /// Sets the instrument ID after construction.
    ///
    /// Used by the hierarchy to assign IDs only after confirming the book
    /// won the insertion race in [`StrikeOrderBookManager::get_or_create`](super::strike::StrikeOrderBookManager::get_or_create).
    #[inline]
    pub(crate) fn set_instrument_id(&self, id: u32) {
        self.instrument_id.store(id, Ordering::Relaxed);
    }

    /// Returns the configured self-trade prevention mode.
    ///
    /// [`STPMode::None`] means STP is disabled (default).
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.book.stp_mode()
    }

    /// Returns the configured fee schedule, or `None` if no fees are applied.
    ///
    /// When `Some`, maker and taker fees (in basis points) are applied to
    /// trades. Use [`TradeResult`] from `_full` order methods to access
    /// computed fee amounts.
    #[must_use]
    #[inline]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.book.fee_schedule()
    }

    /// Returns the trade result captured from the last order submission,
    /// or `None` if no match occurred *while trade capture was armed*.
    ///
    /// Trade capture is **disarmed by default** to keep the per-match hot path
    /// allocation-free: the internal listener only records a [`TradeResult`]
    /// when the book is armed. To populate this accessor, call
    /// [`arm_trade_capture(true)`](Self::arm_trade_capture) first; with capture
    /// disarmed this returns `None` (or the last value captured while armed).
    ///
    /// This accessor is **independent of the `_full` order methods**, which
    /// return their own [`TradeResult`] per-call and never read this slot.
    ///
    /// **Note:** concurrent calls to order methods on the same book may
    /// overwrite this value before it is read — it is a single-slot,
    /// last-write-wins poll, never a per-fill feed. For per-submission
    /// attribution use the `_full` order methods' return values; for a reliable
    /// real-time trade/fill stream use the NATS trade publisher (feature
    /// `nats`); for risk/PnL counters use the order-state tracker. All are
    /// independent of this arm flag.
    ///
    /// **Changed in 0.5.0:** trade capture is now disarmed by default, so the
    /// plain order path no longer auto-populates this accessor. Pre-0.5.0 code
    /// that polled `last_trade_result()` after a plain `add_*`/`cancel` must now
    /// call [`arm_trade_capture(true)`](Self::arm_trade_capture) first, or switch
    /// to one of the feeds above.
    #[must_use]
    pub fn last_trade_result(&self) -> Option<TradeResult> {
        self.last_trade_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Arms or disarms continuous trade capture for this book.
    ///
    /// When armed, the internal trade listener clones each matching
    /// [`TradeResult`] under a lock so it can be read back via
    /// [`last_trade_result`](Self::last_trade_result). When disarmed (the
    /// default), the listener short-circuits on a single relaxed atomic load,
    /// adding no clone or lock cost to the match path.
    ///
    /// This toggle only affects the [`last_trade_result`](Self::last_trade_result)
    /// poll accessor. The `_full` order methods do not use it: they return their
    /// own [`TradeResult`] per-call from the engine's `add_*_with_result`
    /// primitive, so they observe their trade regardless of this setting.
    ///
    /// Intended for a single controller. The slot is single-value and
    /// last-write-wins, so arming it while multiple threads submit crossing
    /// orders to the same book yields a last-writer value that may not
    /// correspond to any one submission — for per-submission attribution under
    /// concurrency, use the `_full` order methods' return values instead.
    #[inline]
    pub fn arm_trade_capture(&self, armed: bool) {
        self.trade_capture_armed.store(armed, Ordering::Relaxed);
    }

    /// Returns whether continuous trade capture is currently armed.
    #[must_use]
    #[inline]
    pub fn is_trade_capture_armed(&self) -> bool {
        self.trade_capture_armed.load(Ordering::Relaxed)
    }

    /// Consumes and returns the captured trade result, emptying the slot.
    ///
    /// Unlike [`last_trade_result`](Self::last_trade_result), which clones and
    /// leaves the value in place, this is a read-once venue-poll primitive: it
    /// takes the captured [`TradeResult`] out and leaves the slot empty, so a
    /// subsequent call returns `None` until the next armed match writes a new
    /// value. The slot is single-value and last-write-wins (see
    /// [`last_trade_result`](Self::last_trade_result) for the concurrency
    /// caveats); this take does not disarm capture.
    #[must_use]
    pub fn take_trade_result(&self) -> Option<TradeResult> {
        self.last_trade_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Empties the trade-capture slot without disarming capture.
    ///
    /// Clears any value left by an earlier armed match so the next
    /// [`last_trade_result`](Self::last_trade_result) /
    /// [`take_trade_result`](Self::take_trade_result) reflects only matches that
    /// occur after this call. Capture stays armed if it was armed — this is
    /// distinct from [`arm_trade_capture(false)`](Self::arm_trade_capture), which
    /// disarms future capture but does NOT clear the slot.
    pub fn clear_trade_capture(&self) {
        let mut guard = self
            .last_trade_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = None;
    }

    /// Returns the current lifecycle status of this instrument.
    #[must_use]
    #[inline]
    pub fn status(&self) -> InstrumentStatus {
        let raw = self.status.load(Ordering::Acquire);
        // SAFETY: we only ever store valid InstrumentStatus u8 values.
        // Fail closed: corrupted values reject orders instead of accepting them.
        InstrumentStatus::from_u8(raw).unwrap_or(InstrumentStatus::Halted)
    }

    /// Stores the lifecycle status without validating the transition.
    ///
    /// Test-only raw setter used to seed lifecycle states that the enforced
    /// state machine cannot reach from a constructed book (e.g.
    /// [`Pending`](InstrumentStatus::Pending): books are constructed as
    /// [`Active`](InstrumentStatus::Active) and no legal edge leads back to
    /// `Pending`). It bypasses the transition check entirely, so it must never be
    /// used on the production order path — [`set_status`](Self::set_status) is the
    /// only validated, race-free way to mutate status.
    #[cfg(test)]
    #[inline]
    fn store_status(&self, status: InstrumentStatus) {
        self.status.store(status as u8, Ordering::Release);
    }

    /// Sets the lifecycle status of this instrument, enforcing the lifecycle
    /// state machine atomically.
    ///
    /// The transition from the current status to `status` must be a legal edge
    /// in the state machine documented on [`InstrumentStatus`]. A self-transition
    /// (`X -> X`) is a legal no-op. Any illegal transition (e.g. reactivating an
    /// [`Expired`](InstrumentStatus::Expired) book, or pulling a
    /// [`Settling`](InstrumentStatus::Settling) book back to
    /// [`Halted`](InstrumentStatus::Halted)) leaves the status unchanged and
    /// returns an error.
    ///
    /// Validation and write form a single atomic compare-and-swap loop over the
    /// underlying status, so this is race-free with respect to concurrent status
    /// updates — for example the expiry-lifecycle CAS-forward setter
    /// ([`compare_and_set_status`](Self::compare_and_set_status)) running on
    /// another thread. In particular, the terminal
    /// [`Expired`](InstrumentStatus::Expired) status is never overwritten: if a
    /// concurrent thread advances the book to `Expired` after this call loads an
    /// earlier status, the lost CAS forces a re-load, the loop re-validates
    /// against `Expired`, finds no legal edge to the requested target, and
    /// returns [`Error::IllegalStatusTransition`] instead of clobbering it.
    ///
    /// # Arguments
    ///
    /// * `status` - The target status to transition to
    ///
    /// # Errors
    ///
    /// Returns [`Error::IllegalStatusTransition`]
    /// if the current status cannot legally transition to `status`.
    #[inline]
    pub fn set_status(&self, status: InstrumentStatus) -> Result<()> {
        // CAS loop closing the former check-then-act race. It is lock-free with
        // guaranteed system-wide progress: every real (non-spurious) CAS failure
        // reflects another thread's *successful* transition, so the system always
        // advances. The status space is finite (5 states); the only cycle is the
        // rare operator-driven `Active <-> Halted` (halt/resume), and the terminal
        // `Expired` is absorbing, so once expiry wins the loop cannot spin. A
        // single caller is only starvable under an adversarial unbounded stream of
        // external halt/resume calls, which are infrequent operator events, not a
        // tight loop — hence bounded in practice.
        loop {
            let current = self.status();
            if !current.can_transition(status) {
                return Err(Error::illegal_status_transition(current, status));
            }
            // Legal self-transition: idempotent no-op, skip the redundant store.
            if current == status {
                return Ok(());
            }
            match self.status.compare_exchange_weak(
                current as u8,
                status as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                // Lost the race: another thread advanced the status. Re-load and
                // re-validate against the new current on the next iteration.
                Err(_) => continue,
            }
        }
    }

    /// Atomically compares and sets the lifecycle status, validating the target.
    ///
    /// If the current status equals `expected` **and** `expected -> new` is a
    /// legal edge in the lifecycle state machine, sets the status to `new` and
    /// returns `true`. Otherwise — either the current status differs from
    /// `expected`, or the transition is illegal — leaves the status unchanged
    /// and returns `false`.
    ///
    /// This provides a thread-safe way to advance status without race
    /// conditions, ensuring monotonic status progression under concurrent
    /// access. The expiry lifecycle's CAS-forward setter drives every chain book
    /// through this method; its only requested edges
    /// (`{Pending, Active, Halted} -> Settling` and
    /// `{Pending, Active, Halted, Settling} -> Expired`) are all legal, so the
    /// added validation never changes its behavior.
    ///
    /// # Arguments
    ///
    /// * `expected` - The expected current status
    /// * `new` - The new status to set if current equals expected
    ///
    /// # Returns
    ///
    /// `true` if the swap succeeded, `false` otherwise (lost race or illegal
    /// transition).
    #[inline]
    #[must_use]
    pub fn compare_and_set_status(
        &self,
        expected: InstrumentStatus,
        new: InstrumentStatus,
    ) -> bool {
        // Reject targets that are not a legal edge from `expected`, so a CAS can
        // never be used to force an illegal transition (e.g. Expired -> Active).
        if !expected.can_transition(new) {
            return false;
        }
        self.status
            .compare_exchange(
                expected as u8,
                new as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Halts the instrument, preventing new orders from being accepted.
    ///
    /// Existing resting orders are not cancelled. Use [`expire`](Self::expire)
    /// to both halt and cancel all orders. Halting is only legal from
    /// [`Active`](InstrumentStatus::Active) (or the idempotent `Halted -> Halted`
    /// no-op).
    ///
    /// # Errors
    ///
    /// Returns [`Error::IllegalStatusTransition`]
    /// if the instrument cannot legally transition to
    /// [`Halted`](InstrumentStatus::Halted) from its current status (e.g. it is
    /// already [`Settling`](InstrumentStatus::Settling) or
    /// [`Expired`](InstrumentStatus::Expired)).
    #[inline]
    pub fn halt(&self) -> Result<()> {
        self.set_status(InstrumentStatus::Halted)
    }

    /// Resumes the instrument, allowing new orders to be accepted.
    ///
    /// Resuming is legal from [`Halted`](InstrumentStatus::Halted) (the resume
    /// edge) or [`Pending`](InstrumentStatus::Pending) (activation), plus the
    /// idempotent `Active -> Active` no-op.
    ///
    /// # Errors
    ///
    /// Returns [`Error::IllegalStatusTransition`]
    /// if the instrument cannot legally transition to
    /// [`Active`](InstrumentStatus::Active) from its current status (e.g. it is
    /// [`Settling`](InstrumentStatus::Settling) or
    /// [`Expired`](InstrumentStatus::Expired)).
    #[inline]
    pub fn resume(&self) -> Result<()> {
        self.set_status(InstrumentStatus::Active)
    }

    /// Expires the instrument, cancelling all resting orders.
    ///
    /// Sets status to [`Expired`](InstrumentStatus::Expired) and cancels every
    /// resting order in a single pass via the underlying engine's
    /// [`cancel_all_orders`](orderbook_rs::OrderBook::cancel_all_orders). Each
    /// dropped order transitions to a terminal `Cancelled` state, the
    /// price-level-changed listener fires (book-change events), the order
    /// tracker counters advance, and the per-account pre-trade risk state is
    /// reset. The status is set first so the book stops accepting new orders
    /// before the sweep runs.
    ///
    /// Expiry is a terminal sweep: it is a legal transition from every
    /// non-terminal status and an idempotent no-op from
    /// [`Expired`](InstrumentStatus::Expired), so in practice it always
    /// succeeds. The orders are only swept when the transition is accepted.
    ///
    /// The status transition is atomic (a CAS loop in
    /// [`set_status`](Self::set_status)), but the sweep is *idempotent* rather
    /// than exactly-once: two concurrent `expire()` callers both pass the status
    /// step (one performs `live -> Expired`, the other a legal `Expired ->
    /// Expired` no-op) and both call `cancel_all_orders`. The first empties the
    /// book; the second is a no-op sweep, so each order is cancelled exactly once
    /// while either caller may report it in its returned id list.
    ///
    /// # Returns
    ///
    /// A vector of order IDs that were cancelled, in engine processing order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::IllegalStatusTransition`]
    /// if the current status cannot legally transition to
    /// [`Expired`](InstrumentStatus::Expired). No orders are swept when the
    /// transition is rejected.
    pub fn expire(&self) -> Result<Vec<OrderId>> {
        self.set_status(InstrumentStatus::Expired)?;
        Ok(self.book.cancel_all_orders().cancelled_order_ids().to_vec())
    }

    /// Checks that the instrument is accepting orders, returning an error if not.
    fn check_active(&self) -> Result<()> {
        let current = self.status();
        if current.is_accepting_orders() {
            Ok(())
        } else {
            Err(crate::Error::instrument_not_active(&self.symbol, current))
        }
    }

    /// Rejects an order priced outside the crate-side `[min_price, max_price]`
    /// band.
    ///
    /// Checks the upper bound first, then the lower bound — at most two `Option`
    /// compares. The happy path (no band, or price within the band) is
    /// allocation-free; each error construction is offloaded to a `#[cold]`
    /// helper so the check stays branch-predictable on the submission hot path.
    #[inline]
    fn check_price_band(&self, price: u128) -> Result<()> {
        if let Some(bound) = self.max_price
            && price > bound
        {
            return Err(self.max_price_exceeded(price, bound));
        }
        if let Some(bound) = self.min_price
            && price < bound
        {
            return Err(self.min_price_violated(price, bound));
        }
        Ok(())
    }

    /// Builds the `max_price` violation error. Cold + un-inlined so the common
    /// in-bounds path carries none of the formatting code.
    #[cold]
    #[inline(never)]
    fn max_price_exceeded(&self, price: u128, bound: u128) -> Error {
        Error::validation(format!(
            "price {price} exceeds max_price {bound} for {}",
            self.symbol
        ))
    }

    /// Builds the `min_price` violation error. Cold + un-inlined so the common
    /// in-bounds path carries none of the formatting code.
    #[cold]
    #[inline(never)]
    fn min_price_violated(&self, price: u128, bound: u128) -> Error {
        Error::validation(format!(
            "price {price} is below min_price {bound} for {}",
            self.symbol
        ))
    }

    /// Builds the iceberg `visible + hidden` overflow error. Cold + un-inlined
    /// so the common in-range path carries none of the formatting code.
    #[cold]
    #[inline(never)]
    fn iceberg_size_overflow(&self, visible: u64, hidden: u64) -> Error {
        Error::validation(format!(
            "iceberg visible_quantity ({visible}) + hidden_quantity ({hidden}) overflows u64 for {}",
            self.symbol
        ))
    }

    /// Adds a limit order to the book.
    ///
    /// # Arguments
    ///
    /// * `order_id` - Unique identifier for the order
    /// * `side` - Buy or Sell side
    /// * `price` - Limit price in smallest units (u128)
    /// * `quantity` - Order quantity in smallest units (u64)
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    pub fn add_limit_order(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
    ) -> Result<()> {
        self.check_active()?;
        self.check_price_band(price)?;
        self.book
            .add_limit_order(order_id, price, quantity, side, TimeInForce::Gtc, None)?;
        Ok(())
    }

    /// Adds a limit order with time-in-force specification.
    ///
    /// # Arguments
    ///
    /// * `order_id` - Unique identifier for the order
    /// * `side` - Buy or Sell side
    /// * `price` - Limit price in smallest units (u128)
    /// * `quantity` - Order quantity in smallest units (u64)
    /// * `tif` - Time-in-force (GTC, IOC, FOK, etc.)
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    pub fn add_limit_order_with_tif(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
    ) -> Result<()> {
        self.check_active()?;
        self.check_price_band(price)?;
        self.book
            .add_limit_order(order_id, price, quantity, side, tif, None)?;
        Ok(())
    }

    /// Adds a limit order with user identity for self-trade prevention.
    ///
    /// When STP is enabled on this book, the `user_id` is used to detect
    /// self-trades. Use [`Hash32::zero()`] to bypass STP checks.
    ///
    /// # Arguments
    ///
    /// * `order_id` - Unique identifier for the order
    /// * `side` - Buy or Sell side
    /// * `price` - Limit price in smallest units (u128)
    /// * `quantity` - Order quantity in smallest units (u64)
    /// * `user_id` - Owner identity for STP checks
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream book rejects the order
    ///   (e.g., `MissingUserId` when STP is enabled and `user_id` is zero).
    pub fn add_limit_order_with_user(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        user_id: Hash32,
    ) -> Result<()> {
        self.check_active()?;
        self.check_price_band(price)?;
        self.book.add_limit_order_with_user(
            order_id,
            price,
            quantity,
            side,
            TimeInForce::Gtc,
            user_id,
            None,
        )?;
        Ok(())
    }

    /// Adds a limit order with time-in-force and user identity for STP.
    ///
    /// Combines time-in-force specification with self-trade prevention.
    ///
    /// # Arguments
    ///
    /// * `order_id` - Unique identifier for the order
    /// * `side` - Buy or Sell side
    /// * `price` - Limit price in smallest units (u128)
    /// * `quantity` - Order quantity in smallest units (u64)
    /// * `tif` - Time-in-force (GTC, IOC, FOK, etc.)
    /// * `user_id` - Owner identity for STP checks
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream book rejects the order.
    pub fn add_limit_order_with_tif_and_user(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> Result<()> {
        self.check_active()?;
        self.check_price_band(price)?;
        self.book
            .add_limit_order_with_user(order_id, price, quantity, side, tif, user_id, None)?;
        Ok(())
    }

    // ── Full order methods (return TradeResult) ─────────────────────────

    /// Builds an empty [`TradeResult`] (no trades, zero fees) for a `_full`
    /// submission that rested on the book without matching.
    ///
    /// The engine's per-call `add_*_with_result` primitives return `None` for
    /// the trade component when an order rests without producing fills; the
    /// `_full` methods map that `None` onto this empty result so callers always
    /// receive a `TradeResult` carrying their own taker order id.
    #[must_use]
    fn empty_trade_result(&self, order_id: OrderId, quantity: u64) -> TradeResult {
        TradeResult::new(
            self.symbol.clone(),
            MatchResult::new(order_id, Quantity::new(quantity)),
        )
    }

    /// Adds a limit order and returns the full [`TradeResult`] including fees.
    ///
    /// Unlike [`add_limit_order`](Self::add_limit_order), this method returns
    /// the trade result with maker/taker fee fields populated according to
    /// the configured [`FeeSchedule`]. When the order rests without matching, an
    /// empty [`TradeResult`] (no trades, zero fees) carrying `order_id` is
    /// returned.
    ///
    /// # Attribution
    ///
    /// The returned [`TradeResult`] is built from *this* call's own match
    /// outcome via the engine's per-call `add_limit_order_with_result` primitive
    /// — never from shared capture state — so concurrent submits to the same
    /// book each receive exactly their own fills, with no cross-attribution and
    /// no lost fills.
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream book rejects the order.
    ///
    /// On an error-after-fills path (an unfillable IOC remainder, or a
    /// self-trade-prevention taker-cancel after earlier non-self fills) the
    /// typed `Err` is returned and the executed fills reach only the installed
    /// trade listener (and thus the NATS publisher / order-state tracker), not
    /// this return value.
    pub fn add_limit_order_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.check_price_band(price)?;
        let (_order, trade) = self.book.add_limit_order_with_result(
            order_id,
            price,
            quantity,
            side,
            TimeInForce::Gtc,
            None,
        )?;
        Ok(trade.unwrap_or_else(|| self.empty_trade_result(order_id, quantity)))
    }

    /// Adds a limit order with time-in-force and returns the full [`TradeResult`].
    ///
    /// Per-call attribution and the error-after-fills caveat are identical to
    /// [`add_limit_order_full`](Self::add_limit_order_full): the result comes
    /// from this call's own outcome, never a shared slot.
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream book rejects the order
    ///   (including error-after-fills paths, where executed fills reach only the trade listener).
    pub fn add_limit_order_with_tif_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.check_price_band(price)?;
        let (_order, trade) = self
            .book
            .add_limit_order_with_result(order_id, price, quantity, side, tif, None)?;
        Ok(trade.unwrap_or_else(|| self.empty_trade_result(order_id, quantity)))
    }

    /// Adds a limit order with user identity and returns the full [`TradeResult`].
    ///
    /// Per-call attribution and the error-after-fills caveat are identical to
    /// [`add_limit_order_full`](Self::add_limit_order_full): the result comes
    /// from this call's own outcome, never a shared slot.
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream book rejects the order
    ///   (including error-after-fills paths, where executed fills reach only the trade listener).
    pub fn add_limit_order_with_user_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        user_id: Hash32,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.check_price_band(price)?;
        let (_order, trade) = self.book.add_limit_order_with_user_and_result(
            order_id,
            price,
            quantity,
            side,
            TimeInForce::Gtc,
            user_id,
            None,
        )?;
        Ok(trade.unwrap_or_else(|| self.empty_trade_result(order_id, quantity)))
    }

    /// Adds a limit order with time-in-force, user identity, and returns
    /// the full [`TradeResult`].
    ///
    /// Per-call attribution and the error-after-fills caveat are identical to
    /// [`add_limit_order_full`](Self::add_limit_order_full): the result comes
    /// from this call's own outcome, never a shared slot.
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream book rejects the order
    ///   (including error-after-fills paths, where executed fills reach only the trade listener).
    pub fn add_limit_order_with_tif_and_user_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.check_price_band(price)?;
        let (_order, trade) = self.book.add_limit_order_with_user_and_result(
            order_id, price, quantity, side, tif, user_id, None,
        )?;
        Ok(trade.unwrap_or_else(|| self.empty_trade_result(order_id, quantity)))
    }

    // ── Order-kind methods (post-only / iceberg) ────────────────────────

    /// Adds a post-only order and returns the full [`TradeResult`].
    ///
    /// A post-only order **never trades on entry**: if it would cross the
    /// opposite side, the upstream engine's shape validation rejects it with
    /// [`OrderBookEngine`](crate::Error::OrderBookEngine) wrapping
    /// `OrderBookError::PriceCrossing { price, side, opposite_price }` *before*
    /// any matching. The only outcomes are therefore: the order rests (an empty
    /// [`TradeResult`] carrying `order_id` is returned), a `PriceCrossing`
    /// rejection, or another validation rejection (tick / lot / risk / STP).
    ///
    /// The leaf builds the [`OrderType::PostOnly`] value directly and submits it
    /// through the engine's generic `add_order_with_result` primitive, because
    /// `orderbook_rs` exposes no typed post-only `_with_result` helper. The
    /// order timestamp is drawn from this book's installed engine clock, so an
    /// injected deterministic clock keeps the stamp reproducible.
    ///
    /// # Arguments
    ///
    /// * `order_id` - Unique identifier for the order
    /// * `side` - Buy or Sell side
    /// * `price` - Limit price in smallest units (u128)
    /// * `quantity` - Order quantity in smallest units (u64)
    /// * `tif` - Time-in-force (GTC, IOC, FOK, etc.)
    /// * `user_id` - Owner identity for STP checks
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream engine rejects the
    ///   order, including `PriceCrossing` when a post-only order would cross.
    pub fn add_post_only_order_with_tif_and_user_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.check_price_band(price)?;
        let order = OrderType::PostOnly {
            id: order_id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id,
            timestamp: self.book.clock().now_millis(),
            time_in_force: tif,
            extra_fields: (),
        };
        let (_order, trade) = self.book.add_order_with_result(order)?;
        Ok(trade.unwrap_or_else(|| self.empty_trade_result(order_id, quantity)))
    }

    /// Adds an iceberg order and returns the full [`TradeResult`].
    ///
    /// Unlike a post-only order, an iceberg **can cross and trade on entry**: it
    /// is matched like a standard limit order, exposing only `visible_quantity`
    /// at a time while `hidden_quantity` is replenished behind it. When it rests
    /// without matching, an empty [`TradeResult`] carrying `order_id` is
    /// returned.
    ///
    /// Upstream lot-size validation applies to the visible **and** hidden
    /// quantities independently, while the min/max order-size checks apply to the
    /// combined total. The resting order's `quantity()` accessor reports the
    /// *visible* tranche, not the total. As with the post-only path, the leaf
    /// builds the [`OrderType::IcebergOrder`] value directly and submits it via
    /// the generic `add_order_with_result` primitive (no typed iceberg
    /// `_with_result` helper exists upstream); the timestamp comes from this
    /// book's engine clock.
    ///
    /// # Arguments
    ///
    /// * `order_id` - Unique identifier for the order
    /// * `side` - Buy or Sell side
    /// * `price` - Limit price in smallest units (u128)
    /// * `visible_quantity` - Visible tranche in smallest units (u64)
    /// * `hidden_quantity` - Hidden reserve in smallest units (u64)
    /// * `tif` - Time-in-force (GTC, IOC, FOK, etc.)
    /// * `user_id` - Owner identity for STP checks
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band, or if
    ///   `visible_quantity + hidden_quantity` overflows `u64`.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream engine rejects the
    ///   order (tick / lot / size / risk / STP).
    #[allow(clippy::too_many_arguments)]
    pub fn add_iceberg_order_with_tif_and_user_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        visible_quantity: u64,
        hidden_quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.check_price_band(price)?;
        // The combined size is what an unmatched-rest empty result reports and
        // what upstream's min/max size check uses; guard the addition so an
        // overflow becomes a typed error rather than a wrap.
        let total = visible_quantity
            .checked_add(hidden_quantity)
            .ok_or_else(|| self.iceberg_size_overflow(visible_quantity, hidden_quantity))?;
        let order = OrderType::IcebergOrder {
            id: order_id,
            price: Price::new(price),
            visible_quantity: Quantity::new(visible_quantity),
            hidden_quantity: Quantity::new(hidden_quantity),
            side,
            user_id,
            timestamp: self.book.clock().now_millis(),
            time_in_force: tif,
            extra_fields: (),
        };
        let (_order, trade) = self.book.add_order_with_result(order)?;
        Ok(trade.unwrap_or_else(|| self.empty_trade_result(order_id, total)))
    }

    /// Adds a post-only order, discarding the [`TradeResult`].
    ///
    /// Convenience wrapper over
    /// [`add_post_only_order_with_tif_and_user_full`](Self::add_post_only_order_with_tif_and_user_full):
    /// it delegates to the `_full` method (the single construction site) and
    /// drops the result. Note the `_with_result` engine path always consumes an
    /// `engine_seq` tick where a plain add may not; this is irrelevant for
    /// replay, since `engine_seq` is per-instance and not replay-comparable.
    ///
    /// # Errors
    ///
    /// Identical to
    /// [`add_post_only_order_with_tif_and_user_full`](Self::add_post_only_order_with_tif_and_user_full).
    pub fn add_post_only_order_with_tif_and_user(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> Result<()> {
        self.add_post_only_order_with_tif_and_user_full(
            order_id, side, price, quantity, tif, user_id,
        )
        .map(|_| ())
    }

    /// Adds an iceberg order, discarding the [`TradeResult`].
    ///
    /// Convenience wrapper over
    /// [`add_iceberg_order_with_tif_and_user_full`](Self::add_iceberg_order_with_tif_and_user_full):
    /// it delegates to the `_full` method (the single construction site) and
    /// drops the result. The same `engine_seq` note as the post-only convenience
    /// applies.
    ///
    /// # Errors
    ///
    /// Identical to
    /// [`add_iceberg_order_with_tif_and_user_full`](Self::add_iceberg_order_with_tif_and_user_full).
    #[allow(clippy::too_many_arguments)]
    pub fn add_iceberg_order_with_tif_and_user(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        visible_quantity: u64,
        hidden_quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> Result<()> {
        self.add_iceberg_order_with_tif_and_user_full(
            order_id,
            side,
            price,
            visible_quantity,
            hidden_quantity,
            tif,
            user_id,
        )
        .map(|_| ())
    }

    /// Cancels an order by its ID.
    ///
    /// # Arguments
    ///
    /// * `order_id` - The ID of the order to cancel
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the order was found and cancelled, `Ok(false)` if no order
    /// with that ID was resting on the book.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookEngine`](crate::Error::OrderBookEngine) if the
    /// underlying engine reports a real cancellation failure (distinct from a
    /// benign not-found).
    pub fn cancel_order(&self, order_id: OrderId) -> Result<bool> {
        // orderbook_rs::cancel_order returns Ok(Some(_)) when an order was
        // cancelled, Ok(None) when no such order was resting, and Err(_) on a
        // genuine engine failure. Map each distinctly: a not-found must report
        // `false` (not a false success), and a real error must surface (not be
        // swallowed as a benign not-found).
        match self.book.cancel_order(order_id) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically replaces a resting order's price, quantity, and side.
    ///
    /// Delegates to the engine's validate-first [`OrderUpdate::Replace`]: the
    /// replacement's shape, the modify-aware risk check, and the self-trade
    /// self-cross check all run **before** the original is cancelled, so on any
    /// rejection the original order stays resting untouched (no book mutation,
    /// no events, no trades). Only after both checks pass is the original
    /// cancelled and the replacement added.
    ///
    /// The replacement is a brand-new order: **queue priority is lost** (the
    /// replacement goes to the back of its price level). If the new price
    /// crosses the book, the replacement may rematch and fill immediately;
    /// those fills reach only the installed trade listener (and thus the NATS
    /// publisher / order-state tracker), **never this return value** — this call
    /// reports placement, not fills.
    ///
    /// # Arguments
    ///
    /// * `order_id` - The ID of the resting order to replace
    /// * `price` - New limit price in smallest units (u128)
    /// * `quantity` - New quantity in smallest units (u64)
    /// * `side` - New side (Buy or Sell); a flip moves the order across the book
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the order was found and replaced, `Ok(false)` if no order
    /// with that ID was resting on the book.
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`ValidationError`](crate::Error::ValidationError) if `price` falls outside
    ///   the configured `[min_price, max_price]` band.
    /// - [`OrderBookEngine`](crate::Error::OrderBookEngine) if the upstream engine rejects the
    ///   replacement (e.g. a tick/lot shape violation or a risk/STP rejection);
    ///   on any such rejection the original order survives.
    pub fn replace_order(
        &self,
        order_id: OrderId,
        price: u128,
        quantity: u64,
        side: Side,
    ) -> Result<bool> {
        self.check_active()?;
        self.check_price_band(price)?;
        // Map the engine's validate-first replace like `cancel_order`: Some =>
        // replaced, None => the order was not resting, Err => a real engine
        // rejection (with the original left untouched).
        match self.book.update_order(OrderUpdate::Replace {
            order_id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
        }) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Cancels all resting orders in this option book.
    ///
    /// # Description
    ///
    /// Cancels every resting order in the underlying OrderBook and returns the
    /// aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// A [`MassCancelResult`] containing the cancelled order count (orders) and
    /// identifiers.
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{OptionOrderBook, OrderId, Side};
    /// use optionstratlib::OptionStyle;
    ///
    /// let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    /// if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 1) {
    ///     panic!("add order failed: {}", err);
    /// }
    /// let result = match book.cancel_all() {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.cancelled_count(), 1);
    /// ```
    pub fn cancel_all(&self) -> Result<MassCancelResult> {
        Ok(self.book.cancel_all_orders())
    }

    /// Cancels all resting orders on a specific side.
    ///
    /// # Description
    ///
    /// Cancels every resting order on the provided side and returns the
    /// aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// * `side` - Side to cancel ([`Side::Buy`] or [`Side::Sell`]).
    ///
    /// # Returns
    ///
    /// A [`MassCancelResult`] containing the cancelled order count (orders) and
    /// identifiers.
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{OptionOrderBook, OrderId, Side};
    /// use optionstratlib::OptionStyle;
    ///
    /// let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    /// if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 1) {
    ///     panic!("add order failed: {}", err);
    /// }
    /// let result = match book.cancel_by_side(Side::Buy) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.cancelled_count(), 1);
    /// ```
    pub fn cancel_by_side(&self, side: Side) -> Result<MassCancelResult> {
        Ok(self.book.cancel_orders_by_side(side))
    }

    /// Cancels all resting orders for a specific user.
    ///
    /// # Description
    ///
    /// Cancels every resting order attributed to the provided user identifier
    /// and returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// * `user_id` - User identifier to cancel (32-byte hash).
    ///
    /// # Returns
    ///
    /// A [`MassCancelResult`] containing the cancelled order count (orders) and
    /// identifiers.
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{OptionOrderBook, OrderId, Side};
    /// use optionstratlib::OptionStyle;
    /// use pricelevel::Hash32;
    ///
    /// let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    /// let user = Hash32::from([1u8; 32]);
    /// if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 1, user) {
    ///     panic!("add order failed: {}", err);
    /// }
    /// let result = match book.cancel_by_user(user) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.cancelled_count(), 1);
    /// ```
    pub fn cancel_by_user(&self, user_id: Hash32) -> Result<MassCancelResult> {
        Ok(self.book.cancel_orders_by_user(user_id))
    }

    /// Evicts resting `GTD` / `DAY` orders whose deadline has passed as of
    /// `now_ms`.
    ///
    /// # Description
    ///
    /// Host-driven, deterministic expiry sweep. Pass-through to the engine's
    /// [`OrderBook::evict_expired_orders`](orderbook_rs::OrderBook::evict_expired_orders):
    /// every resting order whose time-in-force is expired at `now_ms` is
    /// removed through the same single-order cancel path as an ordinary cancel
    /// (so the price-level cache, depth statistics, order indices, risk state,
    /// and the order-state tracker all stay consistent), tagged with
    /// [`CancelReason::TimeInForceExpired`](orderbook_rs::CancelReason). The
    /// engine returns the evicted orders as `Arc<OrderType>`; this wrapper
    /// narrows them to their identifiers, matching the id-centric shape of
    /// [`expire`](Self::expire) and the mass-cancel pass-throughs.
    ///
    /// Unlike [`expire`](Self::expire), this does **not** change the
    /// instrument's lifecycle status and does not require the book to be
    /// active: it is a routine maintenance sweep the host runs on its own
    /// cadence, not a terminal transition.
    ///
    /// # Timestamp and determinism
    ///
    /// `now_ms` is a **caller-supplied Unix-milliseconds** timestamp — the
    /// engine reads no clock, so the sweep is a pure function of `now_ms` and
    /// the resting book, and replays identically. The boundary is inclusive:
    /// an order with deadline exactly `now_ms` is evicted (the same
    /// `tif_expired_at` definition admission uses, so a just-admitted order can
    /// never be swept at the same instant it was accepted).
    ///
    /// Evicted ids are returned in the engine's fixed, replay-stable order:
    /// bids first then asks, price levels ascending within a side, and
    /// ascending insertion sequence (oldest first) within a level.
    ///
    /// # Caveat
    ///
    /// Expiry is realized **only when the sweep runs**. An order whose deadline
    /// has passed but which has not yet been swept still rests and remains
    /// matchable until the next `evict_expired_orders` call; driving the sweep
    /// cadence is the host's responsibility.
    ///
    /// # Arguments
    ///
    /// * `now_ms` - Caller-supplied Unix-milliseconds cutoff. Orders expired at
    ///   or before this instant are evicted.
    ///
    /// # Returns
    ///
    /// The identifiers of the evicted orders, in the deterministic order
    /// described above. Empty when nothing was expired. A second sweep at the
    /// same `now_ms` returns an empty vector (idempotent).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{OptionOrderBook, OrderId, Side, TimeInForce, TimestampMs};
    /// use optionstratlib::OptionStyle;
    ///
    /// let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    /// let gtd = OrderId::new();
    /// // A resting GTD order that expires at t = 10_000_000_000_000 ms.
    /// if let Err(err) =
    ///     book.add_limit_order_with_tif(gtd, Side::Buy, 100, 1, TimeInForce::Gtd(10_000_000_000_000))
    /// {
    ///     panic!("add order failed: {}", err);
    /// }
    /// // Nothing expired one millisecond before the deadline.
    /// assert!(
    ///     book.evict_expired_orders(TimestampMs::new(9_999_999_999_999))
    ///         .is_empty()
    /// );
    /// // At the deadline the order is evicted.
    /// let evicted = book.evict_expired_orders(TimestampMs::new(10_000_000_000_000));
    /// assert_eq!(evicted, vec![gtd]);
    /// ```
    #[must_use]
    pub fn evict_expired_orders(&self, now_ms: TimestampMs) -> Vec<OrderId> {
        self.book
            .evict_expired_orders(now_ms)
            .into_iter()
            .map(|order| order.id())
            .collect()
    }

    // ── Order Lifecycle Queries ────────────────────────────────────────────

    /// Returns the current lifecycle status of an order.
    ///
    /// # Description
    ///
    /// Queries the order state tracker for the current status of the specified
    /// order. Returns `None` if state tracking is disabled, or if the order
    /// was never submitted to this book.
    ///
    /// # Arguments
    ///
    /// * `order_id` - The ID of the order to query.
    ///
    /// # Returns
    ///
    /// The current [`OrderStatus`] if the order is tracked, or `None`.
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{OptionOrderBook, OrderId, Side};
    /// use optionstratlib::OptionStyle;
    ///
    /// let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    /// let id = OrderId::new();
    /// book.add_limit_order(id, Side::Buy, 100, 10).expect("add order");
    /// let status = book.get_order_status(id);
    /// assert!(status.is_some());
    /// ```
    #[must_use]
    pub fn get_order_status(&self, order_id: OrderId) -> Option<OrderStatus> {
        self.book.order_status(order_id)
    }

    /// Returns the full transition history for an order.
    ///
    /// # Description
    ///
    /// Each entry is a `(timestamp_ns, OrderStatus)` pair in chronological
    /// order. Returns `None` if state tracking is disabled, or if the order
    /// was never submitted to this book.
    ///
    /// # Arguments
    ///
    /// * `order_id` - The ID of the order to query.
    ///
    /// # Returns
    ///
    /// A vector of `(timestamp_ns, OrderStatus)` pairs, or `None`.
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{OptionOrderBook, OrderId, Side};
    /// use optionstratlib::OptionStyle;
    ///
    /// let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    /// let id = OrderId::new();
    /// book.add_limit_order(id, Side::Buy, 100, 10).expect("add order");
    /// let history = book.get_order_history(id);
    /// assert!(history.is_some());
    /// ```
    #[must_use]
    pub fn get_order_history(&self, order_id: OrderId) -> Option<Vec<(u64, OrderStatus)>> {
        self.book.get_order_history(order_id)
    }

    /// Returns the number of orders currently in an active state.
    ///
    /// # Description
    ///
    /// Active orders are those in `Open` or `PartiallyFilled` status. Returns
    /// `0` if state tracking is disabled.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// The count of active (resting) orders.
    ///
    /// # Errors
    ///
    /// None.
    #[must_use]
    pub fn active_order_count(&self) -> usize {
        self.book.active_order_count()
    }

    /// Returns the number of orders currently in a terminal state.
    ///
    /// # Description
    ///
    /// Terminal orders are those in `Filled`, `Cancelled`, or `Rejected` status
    /// that are still retained by the tracker. Returns `0` if state tracking
    /// is disabled.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// The count of terminal orders retained in the tracker.
    ///
    /// # Errors
    ///
    /// None.
    #[must_use]
    pub fn terminal_order_count(&self) -> usize {
        self.book.terminal_order_count()
    }

    /// Removes terminal-state entries older than the specified duration.
    ///
    /// # Description
    ///
    /// Active orders (`Open`, `PartiallyFilled`) are never purged. This is
    /// useful for bounded memory management in long-running processes.
    /// Returns `0` if state tracking is disabled.
    ///
    /// # Arguments
    ///
    /// * `older_than` - Entries with a last-transition timestamp older than
    ///   `now - older_than` are removed.
    ///
    /// # Returns
    ///
    /// The number of entries purged.
    ///
    /// # Errors
    ///
    /// None.
    pub fn purge_terminal_states(&self, older_than: Duration) -> usize {
        self.book.purge_terminal_states(older_than)
    }

    /// Returns a summary of terminal order transitions.
    ///
    /// # Description
    ///
    /// The counts represent cumulative lifetime totals of orders that have
    /// transitioned to each terminal state. They are not adjusted when
    /// terminal states are purged or evicted from the tracker.
    ///
    /// Returns a zeroed summary if state tracking is disabled.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// A [`TerminalOrderSummary`] with filled, cancelled, and rejected counts.
    ///
    /// # Errors
    ///
    /// None.
    #[must_use]
    pub fn terminal_order_summary(&self) -> TerminalOrderSummary {
        self.terminal_counters
            .as_ref()
            .map(|c| c.snapshot())
            .unwrap_or_default()
    }

    /// Returns all currently active orders for a specific user.
    ///
    /// # Description
    ///
    /// Searches the order book for resting orders belonging to the specified
    /// user and returns their IDs with current status. Only active (resting)
    /// orders are returned; terminal orders cannot be queried by user because
    /// the tracker does not index by user ID.
    ///
    /// # Arguments
    ///
    /// * `user_id` - The user identifier to filter by.
    ///
    /// # Returns
    ///
    /// A vector of `(OrderId, OrderStatus)` pairs for active orders belonging
    /// to the user.
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{OptionOrderBook, OrderId, Side};
    /// use optionstratlib::OptionStyle;
    /// use pricelevel::Hash32;
    ///
    /// let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    /// let user = Hash32::from([1u8; 32]);
    /// book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user)
    ///     .expect("add order");
    /// let orders = book.orders_by_user(user);
    /// assert_eq!(orders.len(), 1);
    /// ```
    #[must_use]
    pub fn orders_by_user(&self, user_id: Hash32) -> Vec<(OrderId, OrderStatus)> {
        self.book
            .get_all_orders()
            .into_iter()
            .filter(|order| order.user_id() == user_id)
            .map(|order| {
                let id = order.id();
                let status = self.book.order_status(id).unwrap_or(OrderStatus::Open);
                (id, status)
            })
            .collect()
    }

    /// Returns the current best quote.
    ///
    /// This is a small bounded top-of-book read, not an O(1) cached lookup: it
    /// reads the best price on each side and the aggregate size resting at that
    /// single best level via
    /// [`total_depth_at_levels`](orderbook_rs::OrderBook::total_depth_at_levels)
    /// with `levels = 1`. It performs **no** full-book scan and **no** heap
    /// allocation. A one-sided book yields a one-sided [`Quote`] (the empty side
    /// has `None` price and zero size).
    ///
    /// This is the single boundary where the engine's raw `u128` prices and
    /// `u64` sizes are wrapped into the pricelevel newtypes ([`Price`],
    /// [`Quantity`], [`TimestampMs`]) that [`Quote`] exposes. Prices are 128-bit
    /// (`u128`): a value above `2^53` is not exactly representable as an
    /// IEEE-754 double, so any consumer deserializing the resulting [`Quote`]
    /// JSON must parse prices with a 128-bit-aware parser, not as `f64`.
    #[must_use]
    #[inline]
    pub fn best_quote(&self) -> Quote {
        // Wall-clock stamp (non-monotonic). Safe here: `Quote` is a transient
        // market-data read, is excluded from `Quote`'s `PartialEq`, and is never
        // serialized into a journal record or NATS payload. Do NOT let a `Quote`
        // (and hence this timestamp) feed a journaled/replayed path — thread an
        // injected clock instead if that ever changes.
        let timestamp_ms = TimestampMs::new(orderbook_rs::current_time_millis());

        // One ordered top-of-book read per populated side; `total_depth_at_levels`
        // with `levels = 1` sums only the best level and never allocates. The
        // raw `u128` / `u64` engine values are wrapped into the pricelevel
        // newtypes exactly once, here at the leaf boundary.
        let (bid_price, bid_size) = match self.book.best_bid() {
            Some(p) => (
                Some(Price::new(p)),
                Quantity::new(self.book.total_depth_at_levels(1, Side::Buy)),
            ),
            None => (None, Quantity::ZERO),
        };

        let (ask_price, ask_size) = match self.book.best_ask() {
            Some(p) => (
                Some(Price::new(p)),
                Quantity::new(self.book.total_depth_at_levels(1, Side::Sell)),
            ),
            None => (None, Quantity::ZERO),
        };

        Quote::new(bid_price, bid_size, ask_price, ask_size, timestamp_ms)
    }

    /// Returns `true` if the book currently has resting orders on **both**
    /// sides (a two-sided market).
    ///
    /// Cheaper than [`best_quote`](Self::best_quote) when only two-sidedness is
    /// needed: it reads just the best price on each side and never computes
    /// sizes or builds a [`Quote`]. Used by the strike layer to test whether a
    /// contract is fully quoted.
    #[must_use]
    #[inline]
    pub fn has_both_sides(&self) -> bool {
        self.book.best_bid().is_some() && self.book.best_ask().is_some()
    }

    /// Returns the best bid price.
    #[must_use]
    #[inline]
    pub fn best_bid(&self) -> Option<u128> {
        self.book.best_bid()
    }

    /// Returns the best ask price.
    #[must_use]
    #[inline]
    pub fn best_ask(&self) -> Option<u128> {
        self.book.best_ask()
    }

    /// Returns the mid price if both sides exist.
    #[must_use]
    #[inline]
    pub fn mid_price(&self) -> Option<f64> {
        self.book.mid_price()
    }

    /// Returns the spread if both sides exist.
    #[must_use]
    #[inline]
    pub fn spread(&self) -> Option<u128> {
        self.book.spread()
    }

    /// Returns the spread in basis points.
    #[must_use]
    pub fn spread_bps(&self) -> Option<f64> {
        self.book.spread_bps(None)
    }

    /// Returns a snapshot of the order book.
    ///
    /// # Arguments
    ///
    /// * `depth` - Maximum number of price levels to include on each side
    #[must_use]
    pub fn snapshot(&self, depth: usize) -> OrderBookSnapshot {
        self.book.create_snapshot(depth)
    }

    /// Returns the total bid depth (sum of all bid quantities).
    #[must_use]
    pub fn total_bid_depth(&self) -> u64 {
        self.book.total_depth_at_levels(usize::MAX, Side::Buy)
    }

    /// Returns the total ask depth (sum of all ask quantities).
    #[must_use]
    pub fn total_ask_depth(&self) -> u64 {
        self.book.total_depth_at_levels(usize::MAX, Side::Sell)
    }

    /// Returns the number of bid price levels.
    #[must_use]
    pub fn bid_level_count(&self) -> usize {
        self.book.get_bids().len()
    }

    /// Returns the number of ask price levels.
    #[must_use]
    pub fn ask_level_count(&self) -> usize {
        self.book.get_asks().len()
    }

    /// Returns the total number of orders in the book.
    #[must_use]
    pub fn order_count(&self) -> usize {
        self.book.get_all_orders().len()
    }

    /// Returns true if the order book is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.book.best_bid().is_none() && self.book.best_ask().is_none()
    }

    /// Clears all orders from the book.
    ///
    /// Routes through the underlying engine's
    /// [`cancel_all_orders`](orderbook_rs::OrderBook::cancel_all_orders) so
    /// every dropped order transitions to a terminal `Cancelled` state, the
    /// price-level-changed listener fires (book-change events), the order
    /// tracker counters advance, and the per-account pre-trade risk state is
    /// reset. A bare `restore_from_snapshot(empty)` would drop the orders
    /// silently — leaving `active_order_count`, `get_order_status`, the
    /// cancelled counter, downstream book-change publishers, and per-account
    /// risk counters stale — so that path is reserved for genuine snapshot
    /// restore only.
    pub fn clear(&self) {
        let _ = self.book.cancel_all_orders();
    }

    /// Returns the order book imbalance for top N levels.
    ///
    /// Calculated as `(bid_depth - ask_depth) / (bid_depth + ask_depth)`.
    /// Returns a value between -1.0 (all asks) and 1.0 (all bids).
    ///
    /// # Arguments
    ///
    /// * `levels` - Number of price levels to consider
    #[must_use]
    pub fn imbalance(&self, levels: usize) -> f64 {
        self.book.order_book_imbalance(levels)
    }

    /// Returns depth at a specific price level on the bid side.
    #[must_use]
    pub fn bid_depth_at_price(&self, price: u128) -> u64 {
        let (bid_volumes, _) = self.book.get_volume_by_price();
        bid_volumes.get(&price).copied().unwrap_or(0)
    }

    /// Returns depth at a specific price level on the ask side.
    #[must_use]
    pub fn ask_depth_at_price(&self, price: u128) -> u64 {
        let (_, ask_volumes) = self.book.get_volume_by_price();
        ask_volumes.get(&price).copied().unwrap_or(0)
    }

    /// Calculates VWAP for a given quantity.
    ///
    /// # Arguments
    ///
    /// * `quantity` - Target quantity to fill
    /// * `side` - Side to calculate VWAP for
    #[must_use]
    pub fn vwap(&self, quantity: u64, side: Side) -> Option<f64> {
        self.book.vwap(quantity, side)
    }

    /// Returns the micro price (weighted by volume at best bid/ask).
    #[must_use]
    pub fn micro_price(&self) -> Option<f64> {
        self.book.micro_price()
    }

    /// Calculates market impact for a hypothetical order.
    #[must_use]
    pub fn market_impact(&self, quantity: u64, side: Side) -> orderbook_rs::MarketImpact {
        self.book.market_impact(quantity, side)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_option_order_book_creation() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        assert_eq!(book.symbol(), "BTC-20240329-50000-C");
        assert_eq!(book.option_style(), OptionStyle::Call);
        assert!(book.is_empty());
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_limit_orders() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.order_count(), 2);
        assert_eq!(book.bid_level_count(), 1);
        assert_eq!(book.ask_level_count(), 1);
    }

    #[test]
    fn test_orderbook_engine_error_preserves_source_chain() {
        use orderbook_rs::prelude::OrderBookError;
        use std::error::Error as _;

        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        // Admit a resting order, then submit a second order reusing the same
        // OrderId. orderbook_rs rejects the duplicate id with a typed
        // OrderBookError::DuplicateOrderId; the wrapper must propagate it as
        // Error::OrderBookEngine with the upstream source chain intact (rather
        // than flattening it to a string and dropping the typed cause).
        let order_id = OrderId::new();
        book.add_limit_order(order_id, Side::Buy, 100, 10)
            .expect("first add should succeed");

        let err = book
            .add_limit_order(order_id, Side::Buy, 100, 10)
            .expect_err("duplicate order id must be rejected");

        // The wrapper maps the upstream failure to the typed engine variant.
        assert!(
            matches!(err, crate::Error::OrderBookEngine(_)),
            "expected OrderBookEngine variant, got: {err:?}"
        );

        // The typed upstream error is reachable via std::error::Error::source()
        // and downcasts back to the concrete orderbook_rs error.
        let source = err.source().expect("source chain must be preserved");
        let downcast = source
            .downcast_ref::<OrderBookError>()
            .expect("source must downcast to orderbook_rs OrderBookError");
        assert!(
            matches!(downcast, OrderBookError::DuplicateOrderId { .. }),
            "expected DuplicateOrderId from the engine, got: {downcast:?}"
        );
    }

    #[test]
    fn test_best_quote() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
            panic!("add order failed: {}", err);
        }

        let quote = book.best_quote();

        assert_eq!(quote.bid_price(), Some(Price::new(100)));
        assert_eq!(quote.bid_size(), Quantity::new(10));
        assert_eq!(quote.ask_price(), Some(Price::new(101)));
        assert_eq!(quote.ask_size(), Quantity::new(5));
        assert!(quote.is_two_sided());
        assert!(book.has_both_sides());
    }

    #[test]
    fn test_best_quote_sums_best_level_only() {
        // best_quote's size must aggregate all orders resting at the single
        // best level (and not deeper levels), using total_depth_at_levels(1).
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        // Two orders at the best bid (100), one deeper bid (99).
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add bid");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 7)
            .expect("add bid");
        book.add_limit_order(OrderId::new(), Side::Buy, 99, 100)
            .expect("add deep bid");
        // Two orders at the best ask (101), one deeper ask (102).
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .expect("add ask");
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 3)
            .expect("add ask");
        book.add_limit_order(OrderId::new(), Side::Sell, 102, 100)
            .expect("add deep ask");

        let quote = book.best_quote();
        assert_eq!(quote.bid_price(), Some(Price::new(100)));
        assert_eq!(quote.bid_size(), Quantity::new(17)); // 10 + 7, deeper 99 excluded
        assert_eq!(quote.ask_price(), Some(Price::new(101)));
        assert_eq!(quote.ask_size(), Quantity::new(8)); // 5 + 3, deeper 102 excluded
        assert!(quote.is_two_sided());
        assert!(book.has_both_sides());
    }

    #[test]
    fn test_best_quote_one_sided() {
        // Bid-only book: ask side must be None/zero, not two-sided.
        let bid_only = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        bid_only
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add bid");
        let q = bid_only.best_quote();
        assert_eq!(q.bid_price(), Some(Price::new(100)));
        assert_eq!(q.bid_size(), Quantity::new(10));
        assert_eq!(q.ask_price(), None);
        assert_eq!(q.ask_size(), Quantity::ZERO);
        assert!(!q.is_two_sided());
        assert!(!bid_only.has_both_sides());

        // Ask-only book: bid side must be None/zero, not two-sided.
        let ask_only = OptionOrderBook::new("BTC-20240329-50000-P", OptionStyle::Put);
        ask_only
            .add_limit_order(OrderId::new(), Side::Sell, 105, 5)
            .expect("add ask");
        let q = ask_only.best_quote();
        assert_eq!(q.bid_price(), None);
        assert_eq!(q.bid_size(), Quantity::ZERO);
        assert_eq!(q.ask_price(), Some(Price::new(105)));
        assert_eq!(q.ask_size(), Quantity::new(5));
        assert!(!q.is_two_sided());
        assert!(!ask_only.has_both_sides());

        // Empty book: neither side, not two-sided.
        let empty = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let q = empty.best_quote();
        assert!(q.is_empty());
        assert!(!q.is_two_sided());
        assert!(!empty.has_both_sides());
    }

    #[test]
    fn test_trade_capture_disarmed_by_default() {
        // Default: capture is disarmed, so plain order methods do not populate
        // last_trade_result even when a match occurs.
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(!book.is_trade_capture_armed());

        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add ask");
        // Crossing buy matches the resting ask.
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add crossing buy");
        assert!(
            book.last_trade_result().is_none(),
            "disarmed capture must not record a trade"
        );

        // Arming makes the plain path populate the capture.
        book.arm_trade_capture(true);
        assert!(book.is_trade_capture_armed());
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 4)
            .expect("add ask");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 4)
            .expect("add crossing buy");
        assert!(
            book.last_trade_result().is_some(),
            "armed capture must record a trade"
        );
    }

    #[test]
    fn test_full_order_returns_result_independent_of_capture_arm_state() {
        // The _full methods attribute fills per-call from the engine's
        // add_*_with_result primitive, so they return a populated TradeResult
        // regardless of the continuous-capture arm state — and without ever
        // touching the last_trade_result slot.
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(!book.is_trade_capture_armed());

        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add ask");
        let taker = OrderId::new();
        let result = book
            .add_limit_order_full(taker, Side::Buy, 100, 10)
            .expect("full buy");
        // The crossing buy fully consumed the resting ask, attributed to its
        // own taker order id.
        assert_eq!(result.symbol, "BTC-20240329-50000-C");
        assert_eq!(result.match_result.order_id(), taker);
        assert!(result.match_result.is_complete());
        assert_eq!(book.order_count(), 0);
        // Continuous capture stayed disarmed, and the _full path never wrote the
        // last_trade_result slot.
        assert!(!book.is_trade_capture_armed());
        assert!(book.last_trade_result().is_none());
    }

    #[test]
    fn test_full_concurrent_attribution_no_cross_or_lost_fills() {
        // Regression for the per-book capture-slot race: many threads submit
        // crossing buys to the SAME book via `add_limit_order_full`, and each
        // thread must receive the TradeResult for its OWN order id — never
        // another thread's (cross-attribution) and never an empty one (lost
        // fills).
        //
        // Determinism: the book is pre-seeded with exactly one resting sell per
        // buy, all at the same price and quantity, so every crossing buy fully
        // consumes exactly one sell regardless of thread scheduling. Which sell a
        // given buy matches is irrelevant to the assertions — each buy's own
        // taker id and its complete fill of QTY are invariant under any
        // interleaving.
        use std::sync::Barrier;
        use std::thread;

        const THREADS: usize = 8;
        const QTY: u64 = 10;
        const PRICE: u128 = 100;

        let book = Arc::new(OptionOrderBook::new(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
        ));

        // One resting sell per aggressive buy.
        for _ in 0..THREADS {
            book.add_limit_order(OrderId::new(), Side::Sell, PRICE, QTY)
                .expect("seed resting sell");
        }

        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let book = Arc::clone(&book);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let my_id = OrderId::new();
                // Release all submissions as close to simultaneously as possible
                // to maximize contention on the shared book.
                barrier.wait();
                let result = book
                    .add_limit_order_full(my_id, Side::Buy, PRICE, QTY)
                    .expect("full buy");
                (my_id, result)
            }));
        }

        for handle in handles {
            let (my_id, result) = handle.join().expect("thread join");
            // No cross-attribution: the returned result is attributed to THIS
            // thread's own taker order id.
            assert_eq!(
                result.match_result.order_id(),
                my_id,
                "each thread must receive the TradeResult for its own order id"
            );
            // No lost fills: this buy fully crossed one resting sell.
            assert!(
                result.match_result.is_complete(),
                "each concurrent buy must report its own complete fill"
            );
            assert_eq!(
                result.match_result.remaining_quantity(),
                Quantity::new(0),
                "a fully filled buy must leave no remainder"
            );
        }

        // Every resting sell was consumed by exactly one buy.
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_mid_price_and_spread() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.mid_price(), Some(100.5));
        assert_eq!(book.spread(), Some(1));
    }

    #[test]
    fn test_cancel_order() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        let order_id = OrderId::new();
        if let Err(err) = book.add_limit_order(order_id, Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        assert_eq!(book.order_count(), 1);

        let cancelled = match book.cancel_order(order_id) {
            Ok(c) => c,
            Err(err) => panic!("cancel order failed: {}", err),
        };
        assert!(cancelled);
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_cancel_order_not_found_returns_false() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        // Cancelling an order that was never added must report `false`, not a
        // false `true`. (Regression: the wrapper used to map every Ok(_) to
        // Ok(true), so a no-op cancel falsely reported success.)
        match book.cancel_order(OrderId::new()) {
            Ok(found) => assert!(!found, "cancel of a non-existent order must be false"),
            Err(err) => panic!("cancel of a non-existent order must not error: {}", err),
        }
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_cancel_order_already_cancelled_returns_false() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        let order_id = OrderId::new();
        book.add_limit_order(order_id, Side::Buy, 100, 10)
            .expect("add order should succeed");
        assert!(book.cancel_order(order_id).expect("first cancel"));
        // A second cancel of the same id is a no-op: not found, so `false`.
        assert!(
            !book
                .cancel_order(order_id)
                .expect("second cancel must not error"),
            "second cancel of the same order must be false"
        );
    }

    #[test]
    fn test_cancel_all_orders() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
            panic!("add order failed: {}", err);
        }

        let result = match book.cancel_all() {
            Ok(result) => result,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.cancelled_count(), 2);
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_cancel_by_side() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
            panic!("add order failed: {}", err);
        }

        let result = match book.cancel_by_side(Side::Buy) {
            Ok(result) => result,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.cancelled_count(), 1);
        assert_eq!(book.order_count(), 1);
        assert!(book.best_bid().is_none());
        assert!(book.best_ask().is_some());
    }

    #[test]
    fn test_cancel_by_user() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Sell, 101, 5, user_b)
        {
            panic!("add order failed: {}", err);
        }

        let result = match book.cancel_by_user(user_a) {
            Ok(result) => result,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.cancelled_count(), 1);
        assert_eq!(book.order_count(), 1);
        assert!(book.best_ask().is_some());
    }

    // Far-future GTD deadlines (Unix ms) so admission — which reads the real
    // wall clock — accepts them, while the sweep, which is driven purely by the
    // caller-supplied `now_ms`, controls whether they are treated as expired.
    const GTD_EXPIRED: u64 = 10_000_000_000_000;
    const GTD_LATER: u64 = 20_000_000_000_000;

    #[test]
    fn test_evict_expired_orders_evicts_gtd_and_leaves_unexpired_and_gtc() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let expired = OrderId::new();
        let later = OrderId::new();
        let gtc = OrderId::new();

        if let Err(err) = book.add_limit_order_with_tif(
            expired,
            Side::Buy,
            100,
            10,
            TimeInForce::Gtd(GTD_EXPIRED),
        ) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) =
            book.add_limit_order_with_tif(later, Side::Buy, 99, 5, TimeInForce::Gtd(GTD_LATER))
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order_with_tif(gtc, Side::Sell, 105, 7, TimeInForce::Gtc) {
            panic!("add order failed: {}", err);
        }

        // One millisecond before the earliest deadline nothing is expired.
        assert!(
            book.evict_expired_orders(TimestampMs::new(GTD_EXPIRED - 1))
                .is_empty()
        );
        assert_eq!(book.order_count(), 3);

        // At the deadline only the matching GTD order is swept (inclusive
        // boundary); the later GTD and the GTC order are untouched.
        let evicted = book.evict_expired_orders(TimestampMs::new(GTD_EXPIRED));
        assert_eq!(evicted, vec![expired]);
        assert_eq!(book.order_count(), 2);
        assert_eq!(book.get_order_status(later), Some(OrderStatus::Open));
        assert_eq!(book.get_order_status(gtc), Some(OrderStatus::Open));
    }

    #[test]
    fn test_evict_expired_orders_is_idempotent() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let expired = OrderId::new();
        if let Err(err) = book.add_limit_order_with_tif(
            expired,
            Side::Buy,
            100,
            10,
            TimeInForce::Gtd(GTD_EXPIRED),
        ) {
            panic!("add order failed: {}", err);
        }

        let first = book.evict_expired_orders(TimestampMs::new(GTD_EXPIRED));
        assert_eq!(first, vec![expired]);

        // A second sweep at the same instant evicts nothing: the order is gone.
        let second = book.evict_expired_orders(TimestampMs::new(GTD_EXPIRED));
        assert!(second.is_empty());
    }

    #[test]
    fn test_evict_expired_orders_empty_book() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(
            book.evict_expired_orders(TimestampMs::new(GTD_EXPIRED))
                .is_empty()
        );
    }

    #[test]
    fn test_evict_expired_orders_deterministic_order() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let bid_low = OrderId::new();
        let bid_high = OrderId::new();
        let ask = OrderId::new();

        // Add out of the documented order to prove the sweep, not insertion,
        // dictates the result order: bids ascending price, then asks.
        if let Err(err) =
            book.add_limit_order_with_tif(ask, Side::Sell, 105, 1, TimeInForce::Gtd(GTD_EXPIRED))
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order_with_tif(
            bid_high,
            Side::Buy,
            100,
            1,
            TimeInForce::Gtd(GTD_EXPIRED),
        ) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) =
            book.add_limit_order_with_tif(bid_low, Side::Buy, 99, 1, TimeInForce::Gtd(GTD_EXPIRED))
        {
            panic!("add order failed: {}", err);
        }

        let evicted = book.evict_expired_orders(TimestampMs::new(GTD_EXPIRED));
        // Bids ascending price (99 then 100), then asks.
        assert_eq!(evicted, vec![bid_low, bid_high, ask]);
    }

    #[test]
    fn test_total_depth() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 99, 20) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.total_bid_depth(), 30);
        assert_eq!(book.total_ask_depth(), 5);
    }

    #[test]
    fn test_symbol_hash() {
        let book1 = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let book2 = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let book3 = OptionOrderBook::new("BTC-20240329-50000-P", OptionStyle::Put);

        assert_eq!(book1.symbol_hash(), book2.symbol_hash());
        assert_ne!(book1.symbol_hash(), book3.symbol_hash());
    }

    #[test]
    fn test_imbalance() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 60) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 40) {
            panic!("add order failed: {}", err);
        }

        // Imbalance = (60 - 40) / (60 + 40) = 0.2
        let imbalance = book.imbalance(5);
        assert!((imbalance - 0.2).abs() < 0.01);
    }

    #[test]
    fn test_inner_access() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let _inner = book.inner();
        assert!(book.is_empty());
    }

    #[test]
    fn test_inner_arc_access() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let _inner_arc = book.inner_arc();
        assert!(book.is_empty());
    }

    #[test]
    fn test_add_limit_order_with_tif() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) =
            book.add_limit_order_with_tif(OrderId::new(), Side::Buy, 100, 10, TimeInForce::Gtc)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_best_bid_ask() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        assert!(book.best_bid().is_none());
        assert!(book.best_ask().is_none());

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 105, 5) {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.best_bid(), Some(100));
        assert_eq!(book.best_ask(), Some(105));
    }

    #[test]
    fn test_spread_bps() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 102, 5) {
            panic!("add order failed: {}", err);
        }

        let spread_bps = book.spread_bps();
        assert!(spread_bps.is_some());
    }

    #[test]
    fn test_snapshot() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 105, 5) {
            panic!("add order failed: {}", err);
        }

        let snapshot = book.snapshot(5);
        assert_eq!(snapshot.bids.len(), 1);
        assert_eq!(snapshot.asks.len(), 1);
    }

    #[test]
    fn test_clear() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 105, 5) {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.order_count(), 2);
        book.clear();
        assert!(book.is_empty());
    }

    #[test]
    fn test_clear_transitions_orders_to_cancelled_terminal() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id1 = OrderId::new();
        let id2 = OrderId::new();
        book.add_limit_order(id1, Side::Buy, 100, 10)
            .expect("add bid");
        book.add_limit_order(id2, Side::Sell, 105, 5)
            .expect("add ask");
        assert_eq!(book.active_order_count(), 2);

        book.clear();

        // Dropped orders must reach the terminal state, the active count must
        // collapse to zero, and the cancelled counter must advance by the
        // number of orders cleared.
        assert_eq!(book.active_order_count(), 0);
        assert!(matches!(
            book.get_order_status(id1),
            Some(OrderStatus::Cancelled { .. })
        ));
        assert!(matches!(
            book.get_order_status(id2),
            Some(OrderStatus::Cancelled { .. })
        ));
        assert_eq!(book.terminal_order_summary().cancelled, 2);
    }

    #[test]
    fn test_depth_at_price() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 105, 5) {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.bid_depth_at_price(100), 10);
        assert_eq!(book.bid_depth_at_price(99), 0);
        assert_eq!(book.ask_depth_at_price(105), 5);
        assert_eq!(book.ask_depth_at_price(106), 0);
    }

    #[test]
    fn test_vwap() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 99, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 105, 10) {
            panic!("add order failed: {}", err);
        }

        let vwap_sell = book.vwap(5, Side::Sell);
        assert!(vwap_sell.is_some());

        let vwap_buy = book.vwap(5, Side::Buy);
        assert!(vwap_buy.is_some());
    }

    #[test]
    fn test_micro_price() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 102, 10) {
            panic!("add order failed: {}", err);
        }

        let micro = book.micro_price();
        assert!(micro.is_some());
    }

    #[test]
    fn test_market_impact() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 105, 10) {
            panic!("add order failed: {}", err);
        }

        let impact = book.market_impact(5, Side::Buy);
        // avg_price is f64, just verify it's a valid number
        assert!(impact.avg_price >= 0.0 || impact.avg_price < 0.0);
    }

    #[test]
    fn test_new_with_validation_tick_size() {
        let config = ValidationConfig::new().with_tick_size(100);
        let book = OptionOrderBook::new_with_validation(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            &config,
        );

        // Valid price (multiple of 100)
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 200, 10)
                .is_ok()
        );

        // Invalid price (not a multiple of 100)
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_err()
        );
    }

    #[test]
    fn test_new_with_validation_lot_size() {
        let config = ValidationConfig::new().with_lot_size(10);
        let book = OptionOrderBook::new_with_validation(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            &config,
        );

        // Valid quantity (multiple of 10)
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 20)
                .is_ok()
        );

        // Invalid quantity (not a multiple of 10)
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 15)
                .is_err()
        );
    }

    #[test]
    fn test_new_with_validation_min_max_order_size() {
        let config = ValidationConfig::new()
            .with_min_order_size(5)
            .with_max_order_size(100);
        let book = OptionOrderBook::new_with_validation(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            &config,
        );

        // Valid quantity (within range)
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 50)
                .is_ok()
        );

        // Too small
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 2)
                .is_err()
        );

        // Too large
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 200)
                .is_err()
        );
    }

    #[test]
    fn test_validation_config_readback() {
        let config = ValidationConfig::new()
            .with_tick_size(100)
            .with_lot_size(10)
            .with_min_order_size(1)
            .with_max_order_size(1000);
        let book = OptionOrderBook::new_with_validation(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            &config,
        );

        let readback = book.validation_config();
        assert_eq!(readback, Some(config));
    }

    #[test]
    fn test_no_validation_by_default() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(book.validation_config().is_none());

        // Any price/quantity should work
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 1, 1)
                .is_ok()
        );
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 150, 7)
                .is_ok()
        );
    }

    #[test]
    fn test_new_with_empty_validation() {
        let config = ValidationConfig::new();
        let book = OptionOrderBook::new_with_validation(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            &config,
        );

        // Empty config = no validation = anything goes
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 1, 1)
                .is_ok()
        );
    }

    // ── Instrument status tests ──────────────────────────────────────────

    #[test]
    fn test_default_status_is_active() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert_eq!(book.status(), InstrumentStatus::Active);
    }

    #[test]
    fn test_default_status_is_active_with_validation() {
        let config = ValidationConfig::new().with_tick_size(100);
        let book = OptionOrderBook::new_with_validation(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            &config,
        );
        assert_eq!(book.status(), InstrumentStatus::Active);
    }

    #[test]
    fn test_set_status_walks_legal_path_and_get() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        // Active (default) -> Halted -> Active (resume) -> Settling -> Expired.
        book.set_status(InstrumentStatus::Halted)
            .expect("Active -> Halted is legal");
        assert_eq!(book.status(), InstrumentStatus::Halted);

        book.set_status(InstrumentStatus::Active)
            .expect("Halted -> Active is legal");
        assert_eq!(book.status(), InstrumentStatus::Active);

        book.set_status(InstrumentStatus::Settling)
            .expect("Active -> Settling is legal");
        assert_eq!(book.status(), InstrumentStatus::Settling);

        book.set_status(InstrumentStatus::Expired)
            .expect("Settling -> Expired is legal");
        assert_eq!(book.status(), InstrumentStatus::Expired);
    }

    #[test]
    fn test_set_status_active_to_settling_succeeds() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(book.set_status(InstrumentStatus::Settling).is_ok());
        assert_eq!(book.status(), InstrumentStatus::Settling);
    }

    #[test]
    fn test_set_status_settling_to_expired_succeeds() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.set_status(InstrumentStatus::Settling)
            .expect("Active -> Settling is legal");
        assert!(book.set_status(InstrumentStatus::Expired).is_ok());
        assert_eq!(book.status(), InstrumentStatus::Expired);
    }

    #[test]
    fn test_set_status_self_transition_is_legal_noop() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Active -> Active is an idempotent no-op.
        assert!(book.set_status(InstrumentStatus::Active).is_ok());
        assert_eq!(book.status(), InstrumentStatus::Active);

        // Settling -> Settling stays Settling (repeated lifecycle ticks).
        book.set_status(InstrumentStatus::Settling)
            .expect("Active -> Settling is legal");
        assert!(book.set_status(InstrumentStatus::Settling).is_ok());
        assert_eq!(book.status(), InstrumentStatus::Settling);
    }

    #[test]
    fn test_set_status_expired_to_active_rejected() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.expire().expect("Active -> Expired is legal");

        let err = book.set_status(InstrumentStatus::Active);
        assert!(matches!(
            err,
            Err(Error::IllegalStatusTransition {
                from: InstrumentStatus::Expired,
                to: InstrumentStatus::Active,
            })
        ));
        // Status is unchanged on rejection.
        assert_eq!(book.status(), InstrumentStatus::Expired);
    }

    #[test]
    fn test_set_status_expired_to_halted_rejected() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.expire().expect("Active -> Expired is legal");

        let err = book.set_status(InstrumentStatus::Halted);
        assert!(matches!(
            err,
            Err(Error::IllegalStatusTransition {
                from: InstrumentStatus::Expired,
                to: InstrumentStatus::Halted,
            })
        ));
        assert_eq!(book.status(), InstrumentStatus::Expired);
    }

    #[test]
    fn test_set_status_settling_to_halted_rejected() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.set_status(InstrumentStatus::Settling)
            .expect("Active -> Settling is legal");

        let err = book.set_status(InstrumentStatus::Halted);
        assert!(matches!(
            err,
            Err(Error::IllegalStatusTransition {
                from: InstrumentStatus::Settling,
                to: InstrumentStatus::Halted,
            })
        ));
        assert_eq!(book.status(), InstrumentStatus::Settling);
    }

    #[test]
    fn test_halt_sets_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert_eq!(book.status(), InstrumentStatus::Active);

        book.halt().expect("Active -> Halted is legal");
        assert_eq!(book.status(), InstrumentStatus::Halted);
    }

    #[test]
    fn test_halt_settling_rejected() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.set_status(InstrumentStatus::Settling)
            .expect("Active -> Settling is legal");

        let err = book.halt();
        assert!(matches!(
            err,
            Err(Error::IllegalStatusTransition {
                from: InstrumentStatus::Settling,
                to: InstrumentStatus::Halted,
            })
        ));
        assert_eq!(book.status(), InstrumentStatus::Settling);
    }

    #[test]
    fn test_resume_sets_active() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("Active -> Halted is legal");
        assert_eq!(book.status(), InstrumentStatus::Halted);

        book.resume().expect("Halted -> Active is legal (resume)");
        assert_eq!(book.status(), InstrumentStatus::Active);
    }

    #[test]
    fn test_resume_halted_to_active_succeeds() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("Active -> Halted is legal");
        assert!(book.resume().is_ok());
        assert_eq!(book.status(), InstrumentStatus::Active);
    }

    #[test]
    fn test_resume_expired_rejected() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.expire().expect("Active -> Expired is legal");

        let err = book.resume();
        assert!(matches!(
            err,
            Err(Error::IllegalStatusTransition {
                from: InstrumentStatus::Expired,
                to: InstrumentStatus::Active,
            })
        ));
        assert_eq!(book.status(), InstrumentStatus::Expired);
    }

    #[test]
    fn test_compare_and_set_status_allows_legal_forward() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(book.compare_and_set_status(InstrumentStatus::Active, InstrumentStatus::Settling,));
        assert_eq!(book.status(), InstrumentStatus::Settling);
    }

    #[test]
    fn test_compare_and_set_status_rejects_illegal_target() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.expire().expect("Active -> Expired is legal");

        // Expired -> Active is illegal: the CAS reports failure (no swap).
        assert!(!book.compare_and_set_status(InstrumentStatus::Expired, InstrumentStatus::Active,));
        assert_eq!(book.status(), InstrumentStatus::Expired);
    }

    #[test]
    fn test_set_status_expired_is_terminal_under_concurrent_resume() {
        use std::sync::{Arc, Barrier, Mutex};
        use std::thread;

        // Race one expirer (`Halted -> Expired`) against one resumer
        // (`Halted -> Active`) on a fresh book each round, both released from the
        // same barrier. The invariant under test is that once a book reaches the
        // terminal `Expired` status it can NEVER end `Active` — `resume()` must
        // either win the CAS before expiry (returning `Ok`) or lose it and return
        // `IllegalStatusTransition`. The former check-then-act setter could clobber
        // `Expired` with `Active` here; the CAS loop closes that race.
        //
        // Two long-lived worker threads are reused across every round (only two
        // spawns total) and synchronized per round by a pair of 3-party barriers,
        // so the test stays cheap on constrained CI rather than spawning a thread
        // pair per round. `round_ready` publishes the round's book to both workers
        // (the barrier provides the happens-before for the slot write); `round_done`
        // ensures both workers finished before the main thread asserts.
        const ROUNDS: usize = 2_000;
        let slot: Arc<Mutex<Option<Arc<OptionOrderBook>>>> = Arc::new(Mutex::new(None));
        let round_ready = Arc::new(Barrier::new(3));
        let round_done = Arc::new(Barrier::new(3));

        let expirer = {
            let slot = Arc::clone(&slot);
            let round_ready = Arc::clone(&round_ready);
            let round_done = Arc::clone(&round_done);
            thread::spawn(move || {
                for _ in 0..ROUNDS {
                    round_ready.wait();
                    let book = slot.lock().unwrap().clone().expect("book published");
                    book.set_status(InstrumentStatus::Expired)
                        .expect("transition to Expired is legal from every live state");
                    round_done.wait();
                }
            })
        };

        let resumer = {
            let slot = Arc::clone(&slot);
            let round_ready = Arc::clone(&round_ready);
            let round_done = Arc::clone(&round_done);
            thread::spawn(move || {
                for _ in 0..ROUNDS {
                    round_ready.wait();
                    let book = slot.lock().unwrap().clone().expect("book published");
                    // Concurrent cross-check: IF resume happened to lose the race,
                    // it must have failed with the typed illegal-transition error
                    // (`Expired -> Active`), never silently. A resume that wins
                    // returns `Ok`, so this branch is schedule-dependent and we do
                    // not assert it fires — the typed rejection is covered
                    // deterministically by `test_set_status_expired_to_active_rejected`
                    // and `test_resume_expired_rejected`.
                    if let Err(err) = book.resume() {
                        assert!(
                            matches!(
                                err,
                                Error::IllegalStatusTransition {
                                    from: InstrumentStatus::Expired,
                                    to: InstrumentStatus::Active,
                                }
                            ),
                            "unexpected resume error: {err:?}",
                        );
                    }
                    round_done.wait();
                }
            })
        };

        for _ in 0..ROUNDS {
            let book = Arc::new(OptionOrderBook::new(
                "BTC-20240329-50000-C",
                OptionStyle::Call,
            ));
            // Seed `Halted` so resume (`-> Active`) and expire (`-> Expired`) both
            // perform a real compare-exchange from the same starting state and
            // genuinely contend (an `Active -> Active` resume is a no-op and would
            // not exercise the race).
            book.halt().expect("Active -> Halted is legal");
            *slot.lock().unwrap() = Some(Arc::clone(&book));

            round_ready.wait(); // release both workers onto this round's book
            round_done.wait(); // both have finished; safe to read the final status

            // The book always ends terminal-`Expired`, never reactivated.
            assert_eq!(
                book.status(),
                InstrumentStatus::Expired,
                "book must remain Expired after a concurrent resume",
            );
        }

        expirer.join().expect("expirer thread panicked");
        resumer.join().expect("resumer thread panicked");
    }

    #[test]
    fn test_expire_sets_expired_and_clears() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        let id1 = OrderId::new();
        let id2 = OrderId::new();
        if let Err(err) = book.add_limit_order(id1, Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(id2, Side::Sell, 105, 5) {
            panic!("add order failed: {}", err);
        }
        assert_eq!(book.order_count(), 2);

        let cancelled = book.expire().expect("Active -> Expired is legal");
        assert_eq!(book.status(), InstrumentStatus::Expired);
        assert!(book.is_empty());
        assert_eq!(cancelled.len(), 2);
        assert!(cancelled.contains(&id1));
        assert!(cancelled.contains(&id2));
    }

    #[test]
    fn test_expire_on_empty_book() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let cancelled = book.expire().expect("Active -> Expired is legal");
        assert_eq!(book.status(), InstrumentStatus::Expired);
        assert!(cancelled.is_empty());
    }

    #[test]
    fn test_expire_transitions_orders_to_cancelled_and_rejects_new_adds() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id1 = OrderId::new();
        let id2 = OrderId::new();
        book.add_limit_order(id1, Side::Buy, 100, 10)
            .expect("add bid");
        book.add_limit_order(id2, Side::Sell, 105, 5)
            .expect("add ask");
        assert_eq!(book.active_order_count(), 2);

        let cancelled = book.expire().expect("Active -> Expired is legal");

        // All resting orders cancelled and transitioned to a terminal state.
        assert_eq!(cancelled.len(), 2);
        assert_eq!(book.active_order_count(), 0);
        assert!(matches!(
            book.get_order_status(id1),
            Some(OrderStatus::Cancelled { .. })
        ));
        assert!(matches!(
            book.get_order_status(id2),
            Some(OrderStatus::Cancelled { .. })
        ));
        assert_eq!(book.terminal_order_summary().cancelled, 2);

        // Instrument is now Expired and must reject new flow.
        assert_eq!(book.status(), InstrumentStatus::Expired);
        let rejected = book.add_limit_order(OrderId::new(), Side::Buy, 100, 1);
        let err = match rejected {
            Ok(()) => panic!("expected InstrumentNotActive error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("instrument not active"));
        assert!(err.to_string().contains("Expired"));
    }

    #[test]
    fn test_order_rejected_when_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("Active -> Halted is legal");

        let result = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10);
        assert!(result.is_err());
        let err = match result {
            Ok(_) => panic!("expected error but got Ok"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("instrument not active"));
        assert!(err.to_string().contains("Halted"));
    }

    #[test]
    fn test_order_rejected_when_pending() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Pending is the initial lifecycle state; books default to Active and no
        // legal edge targets Pending, so seed it with the raw (unvalidated)
        // setter to exercise the order-rejection path.
        book.store_status(InstrumentStatus::Pending);

        let result = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10);
        assert!(result.is_err());
        let err = match result {
            Ok(_) => panic!("expected error but got Ok"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("Pending"));
    }

    #[test]
    fn test_order_rejected_when_settling() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.set_status(InstrumentStatus::Settling)
            .expect("Active -> Settling is legal");

        let result = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10);
        assert!(result.is_err());
        let err = match result {
            Ok(_) => panic!("expected error but got Ok"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("Settling"));
    }

    #[test]
    fn test_order_rejected_when_expired() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.set_status(InstrumentStatus::Expired)
            .expect("Active -> Expired is legal");

        let result = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10);
        assert!(result.is_err());
        let err = match result {
            Ok(_) => panic!("expected error but got Ok"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("Expired"));
    }

    #[test]
    fn test_order_rejected_with_tif_when_not_active() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("Active -> Halted is legal");

        let result =
            book.add_limit_order_with_tif(OrderId::new(), Side::Buy, 100, 10, TimeInForce::Gtc);
        assert!(result.is_err());
        let err = match result {
            Ok(_) => panic!("expected error but got Ok"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("Halted"));
    }

    #[test]
    fn test_orders_accepted_after_resume() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        book.halt().expect("Active -> Halted is legal");
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .is_err()
        );

        book.resume().expect("Halted -> Active is legal (resume)");
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .is_ok()
        );
    }

    #[test]
    fn test_halt_preserves_existing_orders() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        assert_eq!(book.order_count(), 1);

        book.halt().expect("Active -> Halted is legal");
        // Existing orders remain
        assert_eq!(book.order_count(), 1);
        assert_eq!(book.best_bid(), Some(100));
    }

    #[test]
    fn test_cancel_works_when_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        let oid = OrderId::new();
        if let Err(err) = book.add_limit_order(oid, Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        book.halt().expect("Active -> Halted is legal");

        // Cancellation should still work on halted instruments
        let cancelled = match book.cancel_order(oid) {
            Ok(c) => c,
            Err(err) => panic!("cancel order failed: {}", err),
        };
        assert!(cancelled);
        assert!(book.is_empty());
    }

    #[test]
    fn test_default_instrument_id_is_zero() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert_eq!(book.instrument_id(), 0);
    }

    #[test]
    fn test_new_with_id() {
        let book = OptionOrderBook::new_with_id("BTC-20240329-50000-C", OptionStyle::Call, 42);
        assert_eq!(book.instrument_id(), 42);
        assert_eq!(book.symbol(), "BTC-20240329-50000-C");
    }

    #[test]
    fn test_new_with_validation_has_zero_id() {
        let config = super::super::validation::ValidationConfig::new().with_tick_size(10);
        let book = OptionOrderBook::new_with_validation(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            &config,
        );
        assert_eq!(book.instrument_id(), 0);
    }

    #[test]
    fn test_new_with_config_id_and_validation() {
        let config = super::super::validation::ValidationConfig::new().with_tick_size(10);
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                instrument_id: 99,
                validation: Some(config),
                ..BookConfig::default()
            },
        );
        assert_eq!(book.instrument_id(), 99);
        let vc = book.validation_config();
        assert!(vc.is_some());
        let vc = match vc {
            Some(v) => v,
            None => panic!("expected validation config"),
        };
        assert_eq!(vc.tick_size(), Some(10));
    }

    #[test]
    fn test_stp_mode_default_is_none() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert_eq!(book.stp_mode(), STPMode::None);
    }

    #[test]
    fn test_new_with_stp_cancel_taker() {
        let book = OptionOrderBook::new_with_stp(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            STPMode::CancelTaker,
        );
        assert_eq!(book.stp_mode(), STPMode::CancelTaker);
        assert_eq!(book.instrument_id(), 0);
    }

    #[test]
    fn test_new_with_config_id_and_stp() {
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                instrument_id: 42,
                stp_mode: STPMode::CancelMaker,
                ..BookConfig::default()
            },
        );
        assert_eq!(book.stp_mode(), STPMode::CancelMaker);
        assert_eq!(book.instrument_id(), 42);
    }

    #[test]
    fn test_new_with_config_validation_and_stp() {
        let config = super::super::validation::ValidationConfig::new().with_tick_size(10);
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                validation: Some(config),
                stp_mode: STPMode::CancelBoth,
                ..BookConfig::default()
            },
        );
        assert_eq!(book.stp_mode(), STPMode::CancelBoth);
        assert_eq!(
            book.validation_config().map(|c| c.tick_size()),
            Some(Some(10))
        );
    }

    #[test]
    fn test_new_with_config_id_validation_and_stp() {
        let config = super::super::validation::ValidationConfig::new().with_tick_size(10);
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                instrument_id: 7,
                validation: Some(config),
                stp_mode: STPMode::CancelTaker,
                ..BookConfig::default()
            },
        );
        assert_eq!(book.stp_mode(), STPMode::CancelTaker);
        assert_eq!(book.instrument_id(), 7);
        assert_eq!(
            book.validation_config().map(|c| c.tick_size()),
            Some(Some(10))
        );
    }

    // A GTD deadline (Unix ms) that a fresh `StubClock` near 0 sees as still in
    // the future (admitted), while the wall-clock `MonotonicClock` — in the year
    // 2026, ~1.7e12 ms — sees as long expired (rejected on admission).
    const GTD_ADMIT_DEADLINE: u64 = 10_000;

    #[test]
    fn test_book_config_clock_applies_to_both_ctor_branches() {
        use orderbook_rs::StubClock;

        // For each STP branch of `new_with_config` (None takes the plain `new`
        // ctor, non-None takes `with_stp_mode`), an injected `StubClock` must
        // admit a GTD order whose deadline is only "future" relative to the
        // stub, and the control without a clock must reject it against the wall
        // clock. A non-zero user id is supplied so the STP branch does not
        // reject on `MissingUserId` before the TIF admission check runs.
        let user = Hash32::from([9u8; 32]);
        for stp in [STPMode::None, STPMode::CancelTaker] {
            let with_clock = OptionOrderBook::new_with_config(
                "BTC-20240329-50000-C",
                OptionStyle::Call,
                BookConfig {
                    stp_mode: stp,
                    clock: Some(Arc::new(StubClock::new()) as Arc<dyn Clock>),
                    ..BookConfig::default()
                },
            );
            let admitted = with_clock.add_limit_order_with_tif_and_user(
                OrderId::new(),
                Side::Buy,
                100,
                10,
                TimeInForce::Gtd(GTD_ADMIT_DEADLINE),
                user,
            );
            assert!(
                admitted.is_ok(),
                "GTD order should be admitted under the injected StubClock (stp={stp:?}): {admitted:?}"
            );
            assert_eq!(with_clock.order_count(), 1);

            let without_clock = OptionOrderBook::new_with_config(
                "BTC-20240329-50000-C",
                OptionStyle::Call,
                BookConfig {
                    stp_mode: stp,
                    ..BookConfig::default()
                },
            );
            let rejected = without_clock.add_limit_order_with_tif_and_user(
                OrderId::new(),
                Side::Buy,
                100,
                10,
                TimeInForce::Gtd(GTD_ADMIT_DEADLINE),
                user,
            );
            assert!(
                rejected.is_err(),
                "GTD order should be rejected under the default wall clock (stp={stp:?})"
            );
            assert_eq!(without_clock.order_count(), 0);
        }
    }

    #[test]
    fn test_add_limit_order_with_user() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let user = Hash32::from([1u8; 32]);
        let result = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user);
        assert!(result.is_ok());
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_add_limit_order_with_tif_and_user() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let user = Hash32::from([2u8; 32]);
        let result = book.add_limit_order_with_tif_and_user(
            OrderId::new(),
            Side::Sell,
            200,
            5,
            TimeInForce::Gtc,
            user,
        );
        assert!(result.is_ok());
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_stp_cancel_taker_prevents_self_trade() {
        let book = OptionOrderBook::new_with_stp(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            STPMode::CancelTaker,
        );
        let user = Hash32::from([1u8; 32]);

        // Place a resting sell order
        if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Sell, 100, 10, user)
        {
            panic!("add order failed: {}", err);
        }
        assert_eq!(book.order_count(), 1);

        // Same user places a crossing buy — STP triggers, returns error
        let result = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user);
        assert!(result.is_err());
        // Maker (sell) should still be there
        assert_eq!(book.order_count(), 1);
        assert!(book.best_ask().is_some());
    }

    #[test]
    fn test_stp_cancel_maker_removes_resting_order() {
        let book = OptionOrderBook::new_with_stp(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            STPMode::CancelMaker,
        );
        let user = Hash32::from([1u8; 32]);

        // Place a resting sell order
        if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Sell, 100, 10, user)
        {
            panic!("add order failed: {}", err);
        }
        assert_eq!(book.order_count(), 1);

        // Same user places a crossing buy — maker cancelled, taker rests
        if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user) {
            panic!("add order failed: {}", err);
        }
        // Taker (buy) should now be resting, maker (sell) was cancelled
        assert_eq!(book.order_count(), 1);
        assert!(book.best_bid().is_some());
    }

    #[test]
    fn test_stp_cancel_both_removes_all() {
        let book = OptionOrderBook::new_with_stp(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            STPMode::CancelBoth,
        );
        let user = Hash32::from([1u8; 32]);

        // Place a resting sell order
        if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Sell, 100, 10, user)
        {
            panic!("add order failed: {}", err);
        }
        assert_eq!(book.order_count(), 1);

        // Same user places a crossing buy — STP triggers, returns error
        let result = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user);
        assert!(result.is_err());
    }

    #[test]
    fn test_stp_different_users_trade_normally() {
        let book = OptionOrderBook::new_with_stp(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            STPMode::CancelTaker,
        );
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        // User A sells
        if let Err(err) =
            book.add_limit_order_with_user(OrderId::new(), Side::Sell, 100, 10, user_a)
        {
            panic!("add order failed: {}", err);
        }

        // User B buys — should trade normally
        if let Err(err) = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_b)
        {
            panic!("add order failed: {}", err);
        }
        // Both matched and removed
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_limit_order_with_user_rejected_when_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("Active -> Halted is legal");
        let user = Hash32::from([1u8; 32]);
        let result = book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user);
        assert!(result.is_err());
    }

    // ── Fee schedule tests ──────────────────────────────────────────────

    #[test]
    fn test_fee_schedule_default_is_none() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(book.fee_schedule().is_none());
    }

    #[test]
    fn test_fee_schedule_via_config() {
        let schedule = FeeSchedule::new(-2, 5);
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                fee_schedule: Some(schedule),
                ..BookConfig::default()
            },
        );
        let fs = book.fee_schedule();
        assert!(fs.is_some());
        let s = match fs {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(s.maker_fee_bps, -2);
        assert_eq!(s.taker_fee_bps, 5);
    }

    #[test]
    fn test_fee_schedule_with_stp_and_validation() {
        let config = super::super::validation::ValidationConfig::new().with_tick_size(10);
        let schedule = FeeSchedule::new(-5, 10);
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                instrument_id: 42,
                validation: Some(config),
                stp_mode: STPMode::CancelTaker,
                fee_schedule: Some(schedule),
                enable_state_tracking: true,
                clock: None,
                #[cfg(feature = "nats")]
                nats_listeners: None,
            },
        );
        assert_eq!(book.instrument_id(), 42);
        assert_eq!(book.stp_mode(), STPMode::CancelTaker);
        assert!(book.validation_config().is_some());
        let fs = match book.fee_schedule() {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(fs.maker_fee_bps, -5);
        assert_eq!(fs.taker_fee_bps, 10);
    }

    #[test]
    fn test_add_limit_order_full_no_match() {
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                fee_schedule: Some(FeeSchedule::new(-2, 5)),
                ..BookConfig::default()
            },
        );
        // Single order, no match — returns empty TradeResult
        let result = match book.add_limit_order_full(OrderId::new(), Side::Buy, 100, 10) {
            Ok(r) => r,
            Err(err) => panic!("add order failed: {}", err),
        };
        assert_eq!(result.total_maker_fees, 0);
        assert_eq!(result.total_taker_fees, 0);
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_add_limit_order_full_with_match_and_fees() {
        let schedule = FeeSchedule::new(-2, 5);
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                fee_schedule: Some(schedule),
                ..BookConfig::default()
            },
        );
        // Place a resting sell order
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 100, 10) {
            panic!("add order failed: {}", err);
        }

        // Aggressive buy matches the sell — trade occurs, fees calculated
        let result = match book.add_limit_order_full(OrderId::new(), Side::Buy, 100, 10) {
            Ok(r) => r,
            Err(err) => panic!("add order failed: {}", err),
        };

        // Taker fee: notional * taker_bps / 10_000 = (100 * 10) * 5 / 10_000 = 0
        // For small notionals, fees may round to zero. Just verify the fields exist
        // and the trade was executed.
        assert_eq!(book.order_count(), 0);
        // The result should be a real TradeResult from the matching engine
        assert_eq!(result.symbol, "BTC-20240329-50000-C");
    }

    #[test]
    fn test_add_limit_order_full_zero_fees_by_default() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Place a resting sell
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 100, 10) {
            panic!("add order failed: {}", err);
        }
        // Aggressive buy with _full — no fee schedule, so zero fees
        let result = match book.add_limit_order_full(OrderId::new(), Side::Buy, 100, 10) {
            Ok(r) => r,
            Err(err) => panic!("add order failed: {}", err),
        };
        assert_eq!(result.total_maker_fees, 0);
        assert_eq!(result.total_taker_fees, 0);
    }

    #[test]
    fn test_add_limit_order_with_tif_full() {
        let schedule = FeeSchedule::new(0, 10);
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                fee_schedule: Some(schedule),
                ..BookConfig::default()
            },
        );
        let result = match book.add_limit_order_with_tif_full(
            OrderId::new(),
            Side::Buy,
            100,
            10,
            TimeInForce::Gtc,
        ) {
            Ok(r) => r,
            Err(err) => panic!("add order failed: {}", err),
        };
        // No match, so zero fees
        assert_eq!(result.total_taker_fees, 0);
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_add_limit_order_with_user_full() {
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                fee_schedule: Some(FeeSchedule::new(0, 5)),
                ..BookConfig::default()
            },
        );
        let user = Hash32::from([1u8; 32]);
        let result =
            match book.add_limit_order_with_user_full(OrderId::new(), Side::Buy, 100, 10, user) {
                Ok(r) => r,
                Err(err) => panic!("add order failed: {}", err),
            };
        assert_eq!(result.total_taker_fees, 0);
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_add_limit_order_with_tif_and_user_full() {
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                fee_schedule: Some(FeeSchedule::new(0, 5)),
                ..BookConfig::default()
            },
        );
        let user = Hash32::from([1u8; 32]);
        let result = match book.add_limit_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Sell,
            200,
            5,
            TimeInForce::Gtc,
            user,
        ) {
            Ok(r) => r,
            Err(err) => panic!("add order failed: {}", err),
        };
        assert_eq!(result.total_taker_fees, 0);
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_last_trade_result_none_initially() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(book.last_trade_result().is_none());
    }

    #[test]
    fn test_last_trade_result_populated_after_match() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Capture is disarmed by default; arm it so the plain order path records
        // the trade into last_trade_result.
        book.arm_trade_capture(true);
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 100, 10) {
            panic!("add order failed: {}", err);
        }
        // Trigger a match
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        // last_trade_result should now be populated
        let tr = book.last_trade_result();
        assert!(tr.is_some());
    }

    #[test]
    fn test_full_method_rejected_when_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("Active -> Halted is legal");
        let result = book.add_limit_order_full(OrderId::new(), Side::Buy, 100, 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_backward_compat_no_fee_schedule_zero_fees() {
        // Verifies that existing code path without fee schedule still works
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert!(book.fee_schedule().is_none());
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 100, 10) {
            panic!("add order failed: {}", err);
        }
        let result = match book.add_limit_order_full(OrderId::new(), Side::Buy, 100, 10) {
            Ok(r) => r,
            Err(err) => panic!("add order failed: {}", err),
        };
        assert_eq!(result.total_maker_fees, 0);
        assert_eq!(result.total_taker_fees, 0);
    }

    // ── Order lifecycle tests ──────────────────────────────────────────────

    #[test]
    fn test_get_order_status_returns_open_for_resting_order() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10)
            .expect("add order");
        let status = book.get_order_status(id);
        assert!(status.is_some());
        assert!(matches!(status, Some(OrderStatus::Open)));
    }

    #[test]
    fn test_get_order_status_returns_filled_after_full_match() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let maker_id = OrderId::new();
        let taker_id = OrderId::new();
        book.add_limit_order(maker_id, Side::Sell, 100, 10)
            .expect("add maker");
        book.add_limit_order(taker_id, Side::Buy, 100, 10)
            .expect("add taker");
        // Maker should be filled
        let maker_status = book.get_order_status(maker_id);
        assert!(matches!(maker_status, Some(OrderStatus::Filled { .. })));
    }

    #[test]
    fn test_get_order_history_returns_transitions() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10)
            .expect("add order");
        let history = book.get_order_history(id);
        assert!(history.is_some());
        let h = history.expect("history");
        assert!(!h.is_empty());
        // First transition should be to Open
        assert!(matches!(h[0].1, OrderStatus::Open));
    }

    #[test]
    fn test_active_order_count_tracks_resting_orders() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert_eq!(book.active_order_count(), 0);
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add order");
        assert_eq!(book.active_order_count(), 1);
        book.add_limit_order(OrderId::new(), Side::Sell, 110, 5)
            .expect("add order");
        assert_eq!(book.active_order_count(), 2);
    }

    #[test]
    fn test_terminal_order_count_tracks_filled_orders() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert_eq!(book.terminal_order_count(), 0);
        // Add and match orders
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        // Both should be filled (terminal)
        assert_eq!(book.terminal_order_count(), 2);
    }

    #[test]
    fn test_terminal_order_summary_counts_filled() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        let summary = book.terminal_order_summary();
        assert_eq!(summary.filled, 2);
        assert_eq!(summary.cancelled, 0);
        assert_eq!(summary.rejected, 0);
    }

    #[test]
    fn test_terminal_order_summary_counts_cancelled() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10)
            .expect("add order");
        book.cancel_order(id).expect("cancel");
        let summary = book.terminal_order_summary();
        assert_eq!(summary.filled, 0);
        assert_eq!(summary.cancelled, 1);
        assert_eq!(summary.rejected, 0);
    }

    #[test]
    fn test_orders_by_user_returns_user_orders() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);
        book.add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
            .expect("add a1");
        book.add_limit_order_with_user(OrderId::new(), Side::Buy, 99, 5, user_a)
            .expect("add a2");
        book.add_limit_order_with_user(OrderId::new(), Side::Sell, 110, 10, user_b)
            .expect("add b1");
        let a_orders = book.orders_by_user(user_a);
        assert_eq!(a_orders.len(), 2);
        let b_orders = book.orders_by_user(user_b);
        assert_eq!(b_orders.len(), 1);
    }

    #[test]
    fn test_purge_terminal_states_removes_old_entries() {
        use std::thread;
        use std::time::Duration;

        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Add and match to create terminal states
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        assert_eq!(book.terminal_order_count(), 2);

        // Sleep briefly and purge with a small duration (should purge all)
        thread::sleep(Duration::from_millis(10));
        let purged = book.purge_terminal_states(Duration::from_millis(1));
        assert_eq!(purged, 2);
        assert_eq!(book.terminal_order_count(), 0);
    }

    #[test]
    fn test_state_tracking_disabled_returns_none() {
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                enable_state_tracking: false,
                ..BookConfig::default()
            },
        );
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10)
            .expect("add order");
        // Status should be None when tracking disabled
        assert!(book.get_order_status(id).is_none());
        assert_eq!(book.active_order_count(), 0);
        assert_eq!(book.terminal_order_count(), 0);
    }

    /// A matched trade must drive the prepared NATS trade publisher listener
    /// (carrying the configured subject), while the internal trade-capture
    /// listener and the book-change publisher listener stay wired too.
    ///
    /// This is a fully in-process check: the prepared listeners are capturable
    /// stand-ins for the real `NatsTradePublisher` / `NatsBookChangePublisher`
    /// callbacks (which capture their subject the same way), so no live NATS
    /// server is required.
    #[cfg(feature = "nats")]
    #[test]
    fn test_new_with_config_nats_listener_publishes_to_subject_on_match() {
        use orderbook_rs::PriceLevelChangedEvent;
        use std::sync::atomic::{AtomicU64, Ordering};

        let symbol = "BTC-20240329-50000-C";

        // The trade subject the production free function computes for this
        // symbol with prefix "optionchain" (subject derivation is covered by
        // `OptionChainSubjectBuilder`'s own tests; kept literal here so the leaf
        // test stays free of the eventing `nats` module).
        let expected_subject = "optionchain.trades.BTC.20240329.50000.C".to_string();

        // Capturable stand-in for the NATS trade publisher's listener: it
        // records the (captured) subject and counts the trades it receives,
        // exactly as the real publisher's listener closure does.
        let trade_count = Arc::new(AtomicU64::new(0));
        let captured_subject = Arc::new(Mutex::new(None::<String>));
        let trade_count_c = Arc::clone(&trade_count);
        let captured_subject_c = Arc::clone(&captured_subject);
        let subject_for_closure = expected_subject.clone();
        let trade_listener: TradeListener = Arc::new(move |_tr: &TradeResult| {
            trade_count_c.fetch_add(1, Ordering::Relaxed);
            *captured_subject_c.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(subject_for_closure.clone());
        });

        // Capturable stand-in for the book-change publisher's listener.
        let book_change_count = Arc::new(AtomicU64::new(0));
        let book_change_count_c = Arc::clone(&book_change_count);
        let book_listener: PriceLevelChangedListener =
            Arc::new(move |_ev: PriceLevelChangedEvent| {
                book_change_count_c.fetch_add(1, Ordering::Relaxed);
            });

        let book = OptionOrderBook::new_with_config(
            symbol,
            OptionStyle::Call,
            BookConfig {
                nats_listeners: Some(PreparedNatsListeners {
                    trade_listener,
                    book_listener,
                }),
                ..BookConfig::default()
            },
        );

        // Arm trade capture so the multiplexed internal capture listener (which
        // is disarmed by default) records the trade alongside the NATS listener.
        book.arm_trade_capture(true);

        // Rest a sell, then cross it with a marketable buy to force a trade.
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 5)
            .expect("rest sell");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 5)
            .expect("cross buy");

        // The NATS trade listener fired on the match, carrying the subject.
        assert!(
            trade_count.load(Ordering::Relaxed) >= 1,
            "nats trade listener must fire on a matched trade"
        );
        let got = captured_subject
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        assert_eq!(got, Some(expected_subject));

        // The internal trade-capture listener is preserved by the multiplex.
        assert!(
            book.last_trade_result().is_some(),
            "trade-capture listener must remain wired alongside NATS"
        );

        // The book-change publisher listener was installed and fired.
        assert!(
            book_change_count.load(Ordering::Relaxed) >= 1,
            "book-change listener must fire on price-level changes"
        );
    }

    // ── max_price leaf enforcement ──────────────────────────────────────

    /// Builds a book whose only validation rule is a `max_price` bound.
    fn max_price_book(bound: u128) -> OptionOrderBook {
        OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                validation: Some(ValidationConfig::new().with_max_price(bound)),
                ..BookConfig::default()
            },
        )
    }

    #[test]
    fn test_max_price_add_above_bound_rejected_all_eight_paths() {
        const BOUND: u128 = 1_000;
        const ABOVE: u128 = 1_001;
        let user = Hash32::from([3u8; 32]);
        let book = max_price_book(BOUND);

        // Every add entry point must apply the crate-side bound. Normalize each
        // return type to `Result<()>` so the eight paths can be checked uniformly.
        let results: Vec<Result<()>> = vec![
            book.add_limit_order(OrderId::new(), Side::Buy, ABOVE, 10),
            book.add_limit_order_with_tif(OrderId::new(), Side::Buy, ABOVE, 10, TimeInForce::Gtc),
            book.add_limit_order_with_user(OrderId::new(), Side::Buy, ABOVE, 10, user),
            book.add_limit_order_with_tif_and_user(
                OrderId::new(),
                Side::Buy,
                ABOVE,
                10,
                TimeInForce::Gtc,
                user,
            ),
            book.add_limit_order_full(OrderId::new(), Side::Buy, ABOVE, 10)
                .map(|_| ()),
            book.add_limit_order_with_tif_full(
                OrderId::new(),
                Side::Buy,
                ABOVE,
                10,
                TimeInForce::Gtc,
            )
            .map(|_| ()),
            book.add_limit_order_with_user_full(OrderId::new(), Side::Buy, ABOVE, 10, user)
                .map(|_| ()),
            book.add_limit_order_with_tif_and_user_full(
                OrderId::new(),
                Side::Buy,
                ABOVE,
                10,
                TimeInForce::Gtc,
                user,
            )
            .map(|_| ()),
        ];

        assert_eq!(results.len(), 8);
        for (i, res) in results.into_iter().enumerate() {
            assert!(
                matches!(res, Err(Error::ValidationError { .. })),
                "add path {i} must reject an above-bound price with ValidationError"
            );
        }
        // No above-bound order ever reached the engine.
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_max_price_add_at_bound_accepted() {
        const BOUND: u128 = 1_000;
        let book = max_price_book(BOUND);
        // The bound is inclusive: a price exactly at the bound is accepted.
        let res = book.add_limit_order(OrderId::new(), Side::Buy, BOUND, 10);
        assert!(res.is_ok(), "price at the bound must be accepted: {res:?}");
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_max_price_unset_never_rejects() {
        // No max_price configured: even an extreme price is admitted (subject to
        // the engine's own checks, of which there are none here).
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let res = book.add_limit_order(OrderId::new(), Side::Buy, u128::MAX, 10);
        assert!(
            res.is_ok(),
            "no bound means no crate-side price rejection: {res:?}"
        );
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_validation_config_readback_merges_max_price() {
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                validation: Some(
                    ValidationConfig::new()
                        .with_tick_size(1)
                        .with_max_price(5_000),
                ),
                ..BookConfig::default()
            },
        );
        let config = book.validation_config().expect("config present");
        assert_eq!(config.max_price(), Some(5_000));
        assert_eq!(config.tick_size(), Some(1));
    }

    #[test]
    fn test_validation_config_readback_only_max_price_is_some() {
        // A book configured with ONLY a max_price still reports a non-empty
        // config, because the readback merges the leaf-held bound before the
        // emptiness check.
        let book = max_price_book(2_000);
        let config = book
            .validation_config()
            .expect("config present with only max_price");
        assert_eq!(config.max_price(), Some(2_000));
        assert_eq!(config.tick_size(), None);
    }

    // ── min_price / price-band leaf enforcement ─────────────────────────

    /// Builds a book whose only validation rules are an inclusive price band.
    fn band_book(min: u128, max: u128) -> OptionOrderBook {
        OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                validation: Some(
                    ValidationConfig::new()
                        .with_min_price(min)
                        .with_max_price(max),
                ),
                ..BookConfig::default()
            },
        )
    }

    #[test]
    fn test_min_price_add_below_bound_rejected_all_eight_paths() {
        const MIN: u128 = 500;
        const MAX: u128 = 5_000;
        const BELOW: u128 = 499;
        let user = Hash32::from([4u8; 32]);
        let book = band_book(MIN, MAX);

        // Every add entry point must apply the lower band bound. Normalize each
        // return type to `Result<()>` so the eight paths check uniformly.
        let results: Vec<Result<()>> = vec![
            book.add_limit_order(OrderId::new(), Side::Buy, BELOW, 10),
            book.add_limit_order_with_tif(OrderId::new(), Side::Buy, BELOW, 10, TimeInForce::Gtc),
            book.add_limit_order_with_user(OrderId::new(), Side::Buy, BELOW, 10, user),
            book.add_limit_order_with_tif_and_user(
                OrderId::new(),
                Side::Buy,
                BELOW,
                10,
                TimeInForce::Gtc,
                user,
            ),
            book.add_limit_order_full(OrderId::new(), Side::Buy, BELOW, 10)
                .map(|_| ()),
            book.add_limit_order_with_tif_full(
                OrderId::new(),
                Side::Buy,
                BELOW,
                10,
                TimeInForce::Gtc,
            )
            .map(|_| ()),
            book.add_limit_order_with_user_full(OrderId::new(), Side::Buy, BELOW, 10, user)
                .map(|_| ()),
            book.add_limit_order_with_tif_and_user_full(
                OrderId::new(),
                Side::Buy,
                BELOW,
                10,
                TimeInForce::Gtc,
                user,
            )
            .map(|_| ()),
        ];

        assert_eq!(results.len(), 8);
        for (i, res) in results.into_iter().enumerate() {
            assert!(
                matches!(res, Err(Error::ValidationError { .. })),
                "add path {i} must reject a below-band price with ValidationError"
            );
        }
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_price_band_at_both_bounds_inclusive_accepted() {
        const MIN: u128 = 500;
        const MAX: u128 = 5_000;
        let book = band_book(MIN, MAX);
        // Both bounds are inclusive.
        book.add_limit_order(OrderId::new(), Side::Buy, MIN, 10)
            .expect("price at the lower bound must be accepted");
        book.add_limit_order(OrderId::new(), Side::Sell, MAX, 10)
            .expect("price at the upper bound must be accepted");
        assert_eq!(book.order_count(), 2);
    }

    #[test]
    fn test_min_price_unset_never_rejects_low_price() {
        // No min_price configured (only a max): a very low price is admitted.
        let book = max_price_book(5_000);
        let res = book.add_limit_order(OrderId::new(), Side::Buy, 1, 10);
        assert!(
            res.is_ok(),
            "no lower bound means no low-price rejection: {res:?}"
        );
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_price_band_within_band_accepted() {
        let book = band_book(500, 5_000);
        let res = book.add_limit_order(OrderId::new(), Side::Buy, 1_000, 10);
        assert!(
            res.is_ok(),
            "a price inside the band must be accepted: {res:?}"
        );
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_min_price_enforced_on_replace_order() {
        let book = band_book(500, 5_000);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 1_000, 10)
            .expect("add within band");
        // Reprice below the lower bound: rejected crate-side, original untouched.
        let res = book.replace_order(id, 499, 10, Side::Buy);
        assert!(
            matches!(res, Err(Error::ValidationError { .. })),
            "replace below the band must be rejected crate-side: {res:?}"
        );
        assert_eq!(book.best_bid(), Some(1_000));
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_validation_config_readback_merges_min_price() {
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                validation: Some(ValidationConfig::new().with_min_price(250)),
                ..BookConfig::default()
            },
        );
        let config = book.validation_config().expect("config present");
        assert_eq!(config.min_price(), Some(250));
    }

    #[test]
    fn test_validation_config_readback_merges_full_band() {
        let book = band_book(500, 5_000);
        let config = book.validation_config().expect("config present");
        assert_eq!(config.min_price(), Some(500));
        assert_eq!(config.max_price(), Some(5_000));
    }

    // ── trade-capture take / clear ──────────────────────────────────────

    #[test]
    fn test_take_trade_result_returns_captured_and_empties_slot() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.arm_trade_capture(true);
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("rest sell");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 5)
            .expect("cross buy");

        let first = book.take_trade_result();
        assert!(
            first.is_some(),
            "the crossing match must have been captured"
        );
        // The take consumed the slot: a second read is empty until the next match.
        assert!(book.take_trade_result().is_none());
    }

    #[test]
    fn test_take_trade_result_none_when_never_armed() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // A match occurs, but capture was never armed, so nothing is recorded.
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("rest sell");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 5)
            .expect("cross buy");
        assert!(book.take_trade_result().is_none());
    }

    #[test]
    fn test_clear_trade_capture_empties_slot_without_disarming() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.arm_trade_capture(true);
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("rest sell");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 5)
            .expect("cross buy");
        assert!(book.last_trade_result().is_some());

        book.clear_trade_capture();
        assert!(book.last_trade_result().is_none(), "clear empties the slot");
        // Clearing does NOT disarm: capture stays on for future matches.
        assert!(book.is_trade_capture_armed());
    }

    #[test]
    fn test_take_trade_result_recovers_from_poisoned_lock() {
        let book = Arc::new(OptionOrderBook::new(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
        ));
        // Seed the slot directly with an empty trade result.
        {
            let mut guard = book
                .last_trade_result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(book.empty_trade_result(OrderId::new(), 5));
        }

        // Poison the capture lock by panicking while holding the guard.
        let clone = Arc::clone(&book);
        let handle = std::thread::spawn(move || {
            let _guard = clone
                .last_trade_result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            panic!("poison the trade-capture lock");
        });
        assert!(handle.join().is_err());

        // The take still recovers the seeded value and empties the slot.
        assert!(book.take_trade_result().is_some());
        assert!(book.take_trade_result().is_none());
    }

    // ── replace_order ───────────────────────────────────────────────────

    #[test]
    fn test_replace_order_moves_price_and_quantity_returns_true() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10).expect("add");

        let res = book.replace_order(id, 105, 20, Side::Buy);
        assert!(
            matches!(res, Ok(true)),
            "replace should report a hit: {res:?}"
        );
        assert_eq!(book.best_bid(), Some(105));
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_replace_order_unknown_id_returns_false() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let res = book.replace_order(OrderId::new(), 100, 10, Side::Buy);
        assert!(
            matches!(res, Ok(false)),
            "unknown id must be a miss: {res:?}"
        );
    }

    #[test]
    fn test_replace_order_rejected_when_halted_original_untouched() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10).expect("add");
        book.halt().expect("halt");

        let res = book.replace_order(id, 105, 20, Side::Buy);
        assert!(
            matches!(res, Err(Error::InstrumentNotActive { .. })),
            "replace on a halted book must be rejected: {res:?}"
        );
        // The original is untouched (inspectable while halted).
        assert_eq!(book.best_bid(), Some(100));
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_replace_order_validate_first_rejection_leaves_original_resting() {
        // Tick size 10: the original rests at a valid multiple, then the replace
        // targets a non-multiple price. The engine validates the new shape BEFORE
        // cancelling, so the original stays resting on the rejection.
        let book = OptionOrderBook::new_with_config(
            "BTC-20240329-50000-C",
            OptionStyle::Call,
            BookConfig {
                validation: Some(ValidationConfig::new().with_tick_size(10)),
                ..BookConfig::default()
            },
        );
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10)
            .expect("add at valid tick");

        let res = book.replace_order(id, 105, 10, Side::Buy);
        assert!(
            matches!(res, Err(Error::OrderBookEngine(_))),
            "a tick-size violation must be rejected by the engine: {res:?}"
        );
        // Validate-first: the original survives the rejection untouched.
        assert_eq!(book.best_bid(), Some(100));
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_replace_order_to_crossing_price_rematches_and_fills_reach_listener() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Resting sell to cross into, and a resting buy well below it.
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("rest sell");
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 90, 5)
            .expect("rest buy");

        book.arm_trade_capture(true);
        // Reprice the buy up to the sell: the replacement rematches and fills.
        let res = book.replace_order(id, 100, 5, Side::Buy);
        assert!(
            matches!(res, Ok(true)),
            "crossing replace reports a hit: {res:?}"
        );

        // The fills reached the trade listener (capture slot), NOT this return.
        let captured = book.take_trade_result();
        assert!(
            captured.is_some(),
            "a crossing replace's fills must reach the trade listener"
        );
    }

    #[test]
    fn test_replace_order_side_flip_moves_order_across_book() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 100, 10)
            .expect("add buy");
        assert_eq!(book.best_bid(), Some(100));
        assert!(book.best_ask().is_none());

        // Flip the resting buy into a sell on the other side of the book.
        let res = book.replace_order(id, 120, 10, Side::Sell);
        assert!(matches!(res, Ok(true)), "side flip reports a hit: {res:?}");
        assert!(book.best_bid().is_none());
        assert_eq!(book.best_ask(), Some(120));
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_max_price_enforced_on_replace_order() {
        let book = max_price_book(1_000);
        let id = OrderId::new();
        book.add_limit_order(id, Side::Buy, 500, 10)
            .expect("add within bound");

        let res = book.replace_order(id, 1_001, 10, Side::Buy);
        assert!(
            matches!(res, Err(Error::ValidationError { .. })),
            "replace above the bound must be rejected crate-side: {res:?}"
        );
        // The bound is checked before the engine, so the original is untouched.
        assert_eq!(book.best_bid(), Some(500));
        assert_eq!(book.order_count(), 1);
    }

    // ── post-only / iceberg order kinds ─────────────────────────────────

    /// Fixed non-zero owner for the order-kind tests.
    fn kind_user() -> Hash32 {
        Hash32::from([7u8; 32])
    }

    #[test]
    fn test_add_post_only_order_rests_when_not_crossing() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Empty book: a post-only buy has nothing to cross and rests.
        let res = book.add_post_only_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Buy,
            100,
            10,
            TimeInForce::Gtc,
            kind_user(),
        );
        assert!(
            res.is_ok(),
            "post-only should rest on an empty book: {res:?}"
        );
        assert_eq!(book.best_bid(), Some(100));
        assert!(book.best_ask().is_none());
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn test_add_post_only_order_would_cross_rejected_original_book_untouched() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Seed a resting sell; a post-only buy at the same price would cross.
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("rest sell");

        let res = book.add_post_only_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Buy,
            100,
            5,
            TimeInForce::Gtc,
            kind_user(),
        );
        let err = match res {
            Ok(_) => panic!("a crossing post-only must be rejected"),
            Err(e) => e,
        };
        assert!(
            err.to_string().to_lowercase().contains("cross"),
            "the rejection should mention crossing: {err}"
        );
        // Validate-first shape check: the book is untouched — no bid rested.
        assert_eq!(book.order_count(), 1);
        assert_eq!(book.best_ask(), Some(100));
        assert!(book.best_bid().is_none());
    }

    #[test]
    fn test_add_post_only_order_full_returns_empty_trade_when_rested() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let id = OrderId::new();
        let trade = book
            .add_post_only_order_with_tif_and_user_full(
                id,
                Side::Buy,
                100,
                10,
                TimeInForce::Gtc,
                kind_user(),
            )
            .expect("post-only rests");
        // A rested post-only produced no fills; the empty result carries the
        // taker's own order id.
        assert!(trade.match_result.trades().is_empty());
        assert_eq!(trade.match_result.order_id(), id);
    }

    #[test]
    fn test_add_iceberg_order_full_rests_with_visible_and_hidden() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let trade = book
            .add_iceberg_order_with_tif_and_user_full(
                OrderId::new(),
                Side::Buy,
                100,
                3,
                7,
                TimeInForce::Gtc,
                kind_user(),
            )
            .expect("iceberg rests");
        assert!(
            trade.match_result.trades().is_empty(),
            "resting iceberg has no fills"
        );
        assert_eq!(book.order_count(), 1);
        assert_eq!(book.best_bid(), Some(100));

        // The level exposes the visible tranche and hides the reserve.
        let snapshot = book.snapshot(8);
        let level = snapshot.bids.first().expect("one resting bid level");
        assert_eq!(level.visible_quantity().as_u64(), 3, "visible tranche");
        assert_eq!(level.hidden_quantity().as_u64(), 7, "hidden reserve");
    }

    #[test]
    fn test_add_iceberg_order_full_crossing_returns_fills() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // Resting sell to cross into.
        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("rest sell");

        // Iceberg buy at the ask crosses and trades on entry (visible 3 + hidden 5).
        let trade = book
            .add_iceberg_order_with_tif_and_user_full(
                OrderId::new(),
                Side::Buy,
                100,
                3,
                5,
                TimeInForce::Gtc,
                kind_user(),
            )
            .expect("iceberg crosses");
        assert!(
            !trade.match_result.trades().is_empty(),
            "a crossing iceberg must trade on entry"
        );
    }

    #[test]
    fn test_add_iceberg_order_visible_plus_hidden_overflow_rejected() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let res = book.add_iceberg_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Buy,
            100,
            u64::MAX,
            1,
            TimeInForce::Gtc,
            kind_user(),
        );
        assert!(
            matches!(res, Err(Error::ValidationError { .. })),
            "visible + hidden overflow must be a typed ValidationError: {res:?}"
        );
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_post_only_order_out_of_band_rejected_before_engine() {
        let book = max_price_book(1_000);
        // An above-band post-only is rejected by the crate-side band check before
        // the order is ever built or handed to the engine.
        let res = book.add_post_only_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Buy,
            1_001,
            10,
            TimeInForce::Gtc,
            kind_user(),
        );
        assert!(
            matches!(res, Err(Error::ValidationError { .. })),
            "out-of-band post-only must be a ValidationError, not an engine error: {res:?}"
        );
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_iceberg_order_out_of_band_rejected_before_engine() {
        let book = max_price_book(1_000);
        let res = book.add_iceberg_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Buy,
            1_001,
            3,
            7,
            TimeInForce::Gtc,
            kind_user(),
        );
        assert!(
            matches!(res, Err(Error::ValidationError { .. })),
            "out-of-band iceberg must be a ValidationError, not an engine error: {res:?}"
        );
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_post_only_order_rejected_when_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("halt");
        let res = book.add_post_only_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Buy,
            100,
            10,
            TimeInForce::Gtc,
            kind_user(),
        );
        assert!(
            matches!(res, Err(Error::InstrumentNotActive { .. })),
            "post-only on a halted book must be rejected: {res:?}"
        );
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_iceberg_order_rejected_when_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt().expect("halt");
        let res = book.add_iceberg_order_with_tif_and_user_full(
            OrderId::new(),
            Side::Buy,
            100,
            3,
            7,
            TimeInForce::Gtc,
            kind_user(),
        );
        assert!(
            matches!(res, Err(Error::InstrumentNotActive { .. })),
            "iceberg on a halted book must be rejected: {res:?}"
        );
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_post_only_convenience_delegates() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        // The non-`_full` convenience delegates to the `_full` method and drops
        // the result; a resting post-only still succeeds and rests.
        let res = book.add_post_only_order_with_tif_and_user(
            OrderId::new(),
            Side::Buy,
            100,
            10,
            TimeInForce::Gtc,
            kind_user(),
        );
        assert!(res.is_ok(), "post-only convenience should rest: {res:?}");
        assert_eq!(book.best_bid(), Some(100));
        assert_eq!(book.order_count(), 1);
    }
}
