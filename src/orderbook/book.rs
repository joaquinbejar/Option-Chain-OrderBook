//! Option order book wrapper.
//!
//! This module provides the [`OptionOrderBook`] structure that wraps the
//! OrderBook-rs `OrderBook<T>` implementation with option-specific functionality.

use super::instrument_status::InstrumentStatus;
use super::quote::Quote;
use super::validation::ValidationConfig;
use crate::Result;
use optionstratlib::OptionStyle;
use orderbook_rs::{
    DefaultOrderBook, FeeSchedule, MassCancelResult, OrderBookSnapshot, OrderId, OrderStateTracker,
    OrderStatus, STPMode, Side, TimeInForce, TradeResult,
};
use pricelevel::{Hash32, MatchResult, Quantity};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Internal configuration for constructing an [`OptionOrderBook`].
///
/// Consolidates all optional configuration (instrument ID, validation,
/// STP mode, fee schedule) into a single struct, avoiding constructor
/// explosion as new features are added.
#[derive(Debug, Clone)]
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
}

impl Default for BookConfig {
    fn default() -> Self {
        Self {
            instrument_id: 0,
            validation: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
            enable_state_tracking: true,
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
    /// Last known quote for change detection.
    last_quote: Arc<Quote>,
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
    /// Captured trade result from the last order submission.
    ///
    /// Populated by the internal trade listener when a matching occurs.
    /// Used by the `_full` order methods to return [`TradeResult`].
    last_trade_result: Arc<Mutex<Option<TradeResult>>>,
    /// Cumulative terminal order counters maintained by the state listener.
    ///
    /// `None` when state tracking is disabled via `BookConfig`.
    terminal_counters: Option<Arc<TerminalCounters>>,
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
        if let Some(schedule) = config.fee_schedule {
            book.set_fee_schedule(Some(schedule));
        }

        // Install trade-capture listener so `_full` methods can return TradeResult
        let capture: Arc<Mutex<Option<TradeResult>>> = Arc::new(Mutex::new(None));
        let capture_clone = Arc::clone(&capture);
        book.set_trade_listener(Arc::new(move |tr: &TradeResult| {
            let mut guard = capture_clone
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(tr.clone());
        }));

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
            last_quote: Arc::new(Quote::empty(0)),
            option_style,
            id: OrderId::new(),
            status: AtomicU8::new(InstrumentStatus::Active as u8),
            instrument_id: AtomicU32::new(config.instrument_id),
            last_trade_result: capture,
            terminal_counters,
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
    /// or `None` if no match occurred.
    ///
    /// This is populated by the internal trade listener whenever a matching
    /// event occurs. Prefer the `_full` order methods for reliable access.
    ///
    /// **Note:** concurrent calls to order methods on the same book may
    /// overwrite this value before it is read.
    #[must_use]
    pub fn last_trade_result(&self) -> Option<TradeResult> {
        self.last_trade_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
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

    /// Sets the lifecycle status of this instrument.
    ///
    /// # Arguments
    ///
    /// * `status` - The new status to set
    #[inline]
    pub fn set_status(&self, status: InstrumentStatus) {
        self.status.store(status as u8, Ordering::Release);
    }

    /// Atomically compares and sets the lifecycle status.
    ///
    /// If the current status equals `expected`, sets it to `new` and returns `true`.
    /// Otherwise, leaves the status unchanged and returns `false`.
    ///
    /// This provides a thread-safe way to advance status without race conditions,
    /// ensuring monotonic status progression under concurrent access.
    ///
    /// # Arguments
    ///
    /// * `expected` - The expected current status
    /// * `new` - The new status to set if current equals expected
    ///
    /// # Returns
    ///
    /// `true` if the swap succeeded, `false` otherwise.
    #[inline]
    #[must_use]
    pub fn compare_and_set_status(
        &self,
        expected: InstrumentStatus,
        new: InstrumentStatus,
    ) -> bool {
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
    /// to both halt and cancel all orders.
    #[inline]
    pub fn halt(&self) {
        self.set_status(InstrumentStatus::Halted);
    }

    /// Resumes the instrument, allowing new orders to be accepted.
    #[inline]
    pub fn resume(&self) {
        self.set_status(InstrumentStatus::Active);
    }

    /// Expires the instrument, cancelling all resting orders.
    ///
    /// Sets status to [`Expired`](InstrumentStatus::Expired), collects all
    /// resting order IDs, and clears the book.
    ///
    /// # Returns
    ///
    /// A vector of order IDs that were cancelled.
    pub fn expire(&self) -> Vec<OrderId> {
        self.set_status(InstrumentStatus::Expired);
        let orders = self.book.get_all_orders();
        let ids: Vec<OrderId> = orders.iter().map(|o| o.id()).collect();
        self.clear();
        ids
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
    /// Returns [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    /// [`Active`](InstrumentStatus::Active).
    pub fn add_limit_order(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
    ) -> Result<()> {
        self.check_active()?;
        self.book
            .add_limit_order(order_id, price, quantity, side, TimeInForce::Gtc, None)
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
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
    /// Returns [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    /// [`Active`](InstrumentStatus::Active).
    pub fn add_limit_order_with_tif(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
    ) -> Result<()> {
        self.check_active()?;
        self.book
            .add_limit_order(order_id, price, quantity, side, tif, None)
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
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
    /// - [`OrderBookError`](crate::Error::OrderBookError) if the upstream book rejects the order
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
        self.book
            .add_limit_order_with_user(
                order_id,
                price,
                quantity,
                side,
                TimeInForce::Gtc,
                user_id,
                None,
            )
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
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
    /// - [`OrderBookError`](crate::Error::OrderBookError) if the upstream book rejects the order.
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
        self.book
            .add_limit_order_with_user(order_id, price, quantity, side, tif, user_id, None)
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
        Ok(())
    }

    // ── Full order methods (return TradeResult) ─────────────────────────

    /// Clears the captured trade result before submitting an order.
    fn clear_trade_capture(&self) {
        let mut guard = self
            .last_trade_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = None;
    }

    /// Extracts the captured [`TradeResult`], or creates an empty one
    /// (no trades, zero fees) when the order rested without matching.
    #[must_use]
    fn extract_trade_result(&self, order_id: OrderId, quantity: u64) -> TradeResult {
        self.last_trade_result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .unwrap_or_else(|| {
                TradeResult::new(
                    self.symbol.clone(),
                    MatchResult::new(order_id, Quantity::new(quantity)),
                )
            })
    }

    /// Adds a limit order and returns the full [`TradeResult`] including fees.
    ///
    /// Unlike [`add_limit_order`](Self::add_limit_order), this method returns
    /// the trade result with maker/taker fee fields populated according to
    /// the configured [`FeeSchedule`].
    ///
    /// **Concurrency note:** concurrent `_full` calls on the same book are
    /// not guaranteed to return the correct result for each caller. Use the
    /// regular `add_limit_order` methods for concurrent workloads.
    ///
    /// # Errors
    ///
    /// Returns [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    /// [`Active`](InstrumentStatus::Active).
    pub fn add_limit_order_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.clear_trade_capture();
        self.book
            .add_limit_order(order_id, price, quantity, side, TimeInForce::Gtc, None)
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
        Ok(self.extract_trade_result(order_id, quantity))
    }

    /// Adds a limit order with time-in-force and returns the full [`TradeResult`].
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`OrderBookError`](crate::Error::OrderBookError) if the upstream book rejects the order.
    pub fn add_limit_order_with_tif_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.clear_trade_capture();
        self.book
            .add_limit_order(order_id, price, quantity, side, tif, None)
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
        Ok(self.extract_trade_result(order_id, quantity))
    }

    /// Adds a limit order with user identity and returns the full [`TradeResult`].
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`OrderBookError`](crate::Error::OrderBookError) if the upstream book rejects the order.
    pub fn add_limit_order_with_user_full(
        &self,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        user_id: Hash32,
    ) -> Result<TradeResult> {
        self.check_active()?;
        self.clear_trade_capture();
        self.book
            .add_limit_order_with_user(
                order_id,
                price,
                quantity,
                side,
                TimeInForce::Gtc,
                user_id,
                None,
            )
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
        Ok(self.extract_trade_result(order_id, quantity))
    }

    /// Adds a limit order with time-in-force, user identity, and returns
    /// the full [`TradeResult`].
    ///
    /// # Errors
    ///
    /// - [`InstrumentNotActive`](crate::Error::InstrumentNotActive) if the instrument is not
    ///   [`Active`](InstrumentStatus::Active).
    /// - [`OrderBookError`](crate::Error::OrderBookError) if the upstream book rejects the order.
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
        self.clear_trade_capture();
        self.book
            .add_limit_order_with_user(order_id, price, quantity, side, tif, user_id, None)
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
        Ok(self.extract_trade_result(order_id, quantity))
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
    /// Returns [`Error::orderbook`](crate::Error::orderbook) if the underlying
    /// engine reports a real cancellation failure (distinct from a benign
    /// not-found).
    pub fn cancel_order(&self, order_id: OrderId) -> Result<bool> {
        // orderbook_rs::cancel_order returns Ok(Some(_)) when an order was
        // cancelled, Ok(None) when no such order was resting, and Err(_) on a
        // genuine engine failure. Map each distinctly: a not-found must report
        // `false` (not a false success), and a real error must surface (not be
        // swallowed as a benign not-found).
        match self.book.cancel_order(order_id) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(crate::Error::orderbook(e.to_string())),
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
    /// use option_chain_orderbook::orderbook::OptionOrderBook;
    /// use optionstratlib::OptionStyle;
    /// use orderbook_rs::{OrderId, Side};
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
    /// use option_chain_orderbook::orderbook::OptionOrderBook;
    /// use optionstratlib::OptionStyle;
    /// use orderbook_rs::{OrderId, Side};
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
    /// use option_chain_orderbook::orderbook::OptionOrderBook;
    /// use optionstratlib::OptionStyle;
    /// use orderbook_rs::{OrderId, Side};
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
    /// use option_chain_orderbook::orderbook::OptionOrderBook;
    /// use optionstratlib::OptionStyle;
    /// use orderbook_rs::{OrderId, Side};
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
    /// use option_chain_orderbook::orderbook::OptionOrderBook;
    /// use optionstratlib::OptionStyle;
    /// use orderbook_rs::{OrderId, Side};
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
    /// use option_chain_orderbook::orderbook::OptionOrderBook;
    /// use optionstratlib::OptionStyle;
    /// use orderbook_rs::{OrderId, Side};
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
    #[must_use]
    pub fn best_quote(&self) -> Quote {
        let timestamp_ms = orderbook_rs::current_time_millis();

        let (bid_price, bid_size) = self
            .book
            .best_bid()
            .map(|p| (Some(p), self.bid_depth_at_price(p)))
            .unwrap_or((None, 0));

        let (ask_price, ask_size) = self
            .book
            .best_ask()
            .map(|p| (Some(p), self.ask_depth_at_price(p)))
            .unwrap_or((None, 0));

        Quote::new(bid_price, bid_size, ask_price, ask_size, timestamp_ms)
    }

    /// Returns the best bid price.
    #[must_use]
    pub fn best_bid(&self) -> Option<u128> {
        self.book.best_bid()
    }

    /// Returns the best ask price.
    #[must_use]
    pub fn best_ask(&self) -> Option<u128> {
        self.book.best_ask()
    }

    /// Returns the mid price if both sides exist.
    #[must_use]
    pub fn mid_price(&self) -> Option<f64> {
        self.book.mid_price()
    }

    /// Returns the spread if both sides exist.
    #[must_use]
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
    pub fn clear(&self) {
        let empty_snapshot = OrderBookSnapshot {
            symbol: self.symbol.clone(),
            timestamp: orderbook_rs::current_time_millis(),
            bids: vec![],
            asks: vec![],
        };
        let _ = self.book.restore_from_snapshot(empty_snapshot);
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

    /// Updates the last known quote and returns true if it changed.
    pub fn update_last_quote(&mut self) -> bool {
        let current = self.best_quote();
        let changed = current != *self.last_quote;
        self.last_quote = Arc::new(current);
        changed
    }

    /// Returns a reference to the last known quote.
    #[must_use]
    pub fn last_quote(&self) -> &Quote {
        &self.last_quote
    }

    /// Returns an Arc reference to the last known quote.
    #[must_use]
    pub fn last_quote_arc(&self) -> Arc<Quote> {
        Arc::clone(&self.last_quote)
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

    // ── NATS Integration ─────────────────────────────────────────────────

    /// Connects NATS trade and book change publishers to this order book.
    ///
    /// This method creates NATS publishers for trade events and price level
    /// changes, using hierarchical subjects based on the option symbol.
    ///
    /// # Subject Format
    ///
    /// - Trades: `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
    /// - Book changes: `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
    ///
    /// # Arguments
    ///
    /// * `config` - NATS configuration with JetStream context and subject prefix
    ///
    /// # Errors
    ///
    /// Returns an error if the symbol cannot be parsed into its components.
    ///
    /// # Feature Gate
    ///
    /// This method is only available when the `nats` feature is enabled.
    #[cfg(feature = "nats")]
    pub fn connect_nats(
        &self,
        config: &super::nats::OptionChainNatsConfig,
    ) -> crate::Result<NatsPublisherHandles> {
        use super::nats::OptionChainSubjectBuilder;
        use orderbook_rs::prelude::{NatsBookChangePublisher, NatsTradePublisher};

        let subject = OptionChainSubjectBuilder::from_symbol(&self.symbol)?;
        let trade_subject = subject.trade_subject(config.subject_prefix());
        let book_subject = subject.book_subject(config.subject_prefix());

        // Create trade publisher
        let trade_publisher = NatsTradePublisher::new(
            config.jetstream().clone(),
            trade_subject,
            config.runtime().clone(),
        );
        let (trade_handle, trade_listener) = trade_publisher.into_listener();

        // Create book change publisher
        let book_change_publisher = NatsBookChangePublisher::new(
            config.jetstream().clone(),
            self.symbol.clone(),
            book_subject,
            config.runtime().clone(),
        );
        let (book_handle, book_listener) = book_change_publisher.into_listener();

        // Note: The underlying DefaultOrderBook is wrapped in Arc, so we cannot
        // modify listeners after construction. This is a limitation - NATS
        // integration should ideally be configured at construction time.
        // For now, we return the handles and listeners for the caller to use.

        Ok(NatsPublisherHandles {
            trade_handle,
            trade_listener,
            book_handle,
            book_listener,
        })
    }
}

/// Handles and listeners for NATS publishers.
///
/// Returned by [`OptionOrderBook::connect_nats`] when the `nats` feature is enabled.
/// The handles can be used to read metrics (publish counts, error counts), and
/// the listeners should be attached to the order book during construction.
#[cfg(feature = "nats")]
pub struct NatsPublisherHandles {
    /// Handle to the trade publisher for reading metrics.
    pub trade_handle: std::sync::Arc<orderbook_rs::prelude::NatsTradePublisher>,
    /// Trade listener to attach to the order book.
    pub trade_listener: orderbook_rs::prelude::TradeListener,
    /// Handle to the book change publisher for reading metrics.
    pub book_handle: std::sync::Arc<orderbook_rs::prelude::NatsBookChangePublisher>,
    /// Book change listener to attach to the order book.
    pub book_listener: orderbook_rs::orderbook::book_change_event::PriceLevelChangedListener,
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
    fn test_best_quote() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
            panic!("add order failed: {}", err);
        }

        let quote = book.best_quote();

        assert_eq!(quote.bid_price(), Some(100));
        assert_eq!(quote.bid_size(), 10);
        assert_eq!(quote.ask_price(), Some(101));
        assert_eq!(quote.ask_size(), 5);
        assert!(quote.is_two_sided());
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
    fn test_update_last_quote() {
        let mut book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }

        let changed = book.update_last_quote();
        assert!(changed);

        let changed_again = book.update_last_quote();
        assert!(!changed_again);

        let _last = book.last_quote();
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
    fn test_set_status_and_get() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

        for &status in &[
            InstrumentStatus::Pending,
            InstrumentStatus::Active,
            InstrumentStatus::Halted,
            InstrumentStatus::Settling,
            InstrumentStatus::Expired,
        ] {
            book.set_status(status);
            assert_eq!(book.status(), status);
        }
    }

    #[test]
    fn test_halt_sets_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        assert_eq!(book.status(), InstrumentStatus::Active);

        book.halt();
        assert_eq!(book.status(), InstrumentStatus::Halted);
    }

    #[test]
    fn test_resume_sets_active() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt();
        assert_eq!(book.status(), InstrumentStatus::Halted);

        book.resume();
        assert_eq!(book.status(), InstrumentStatus::Active);
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

        let cancelled = book.expire();
        assert_eq!(book.status(), InstrumentStatus::Expired);
        assert!(book.is_empty());
        assert_eq!(cancelled.len(), 2);
        assert!(cancelled.contains(&id1));
        assert!(cancelled.contains(&id2));
    }

    #[test]
    fn test_expire_on_empty_book() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        let cancelled = book.expire();
        assert_eq!(book.status(), InstrumentStatus::Expired);
        assert!(cancelled.is_empty());
    }

    #[test]
    fn test_order_rejected_when_halted() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.halt();

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
        book.set_status(InstrumentStatus::Pending);

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
        book.set_status(InstrumentStatus::Settling);

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
        book.set_status(InstrumentStatus::Expired);

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
        book.halt();

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

        book.halt();
        assert!(
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .is_err()
        );

        book.resume();
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

        book.halt();
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
        book.halt();

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
        book.halt();
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
        book.halt();
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
}
