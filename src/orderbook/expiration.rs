//! Expiration order book module.
//!
//! This module provides the [`ExpirationOrderBook`] and [`ExpirationOrderBookManager`]
//! for managing all expirations for a single underlying asset.

use super::chain::{ChainEvictExpiredResult, ChainMassCancelResult, OptionChainOrderBook};
use super::contract_specs::ContractSpecs;
use super::expiration_key::ExpirationKey;
use super::instrument_registry::InstrumentRegistry;
use super::shared::Shared;
use super::strike::StrikeOrderBook;
use super::symbol_index::SymbolIndex;
use super::validation::ValidationConfig;
use crate::error::{Error, Result};
use crate::utils::checked_accumulate;
use crossbeam_skiplist::SkipMap;
use optionstratlib::ExpirationDate;
use orderbook_rs::{Clock, FeeSchedule, OrderId, OrderStatus, STPMode, Side};
use pricelevel::{Hash32, TimestampMs};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use super::book::TerminalOrderSummary;
#[cfg(feature = "nats")]
use super::book::{ContractNatsListenerFactory, SharedNatsFactory};

/// Order book for a single expiration date.
///
/// Contains the complete option chain for a specific expiration.
///
/// ## Architecture
///
/// ```text
/// ExpirationOrderBook (per expiry date)
///   └── OptionChainOrderBook
///         └── StrikeOrderBookManager
///               └── StrikeOrderBook (per strike)
/// ```
pub struct ExpirationOrderBook {
    /// The underlying asset symbol.
    underlying: String,
    /// The expiration date.
    expiration: ExpirationDate,
    /// The option chain for this expiration.
    chain: Arc<OptionChainOrderBook>,
    /// Unique identifier for this expiration order book.
    id: OrderId,
    /// Instrument registry propagated to the chain.
    registry: Option<Arc<InstrumentRegistry>>,
    /// Symbol index for O(1) lookup by symbol string.
    /// Stored for future use in hierarchy traversal.
    #[allow(dead_code)]
    symbol_index: Option<Arc<SymbolIndex>>,
}

impl ExpirationOrderBook {
    /// Creates a new expiration order book.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol (e.g., "BTC")
    /// * `expiration` - The expiration date
    #[must_use]
    pub fn new(underlying: impl Into<String>, expiration: ExpirationDate) -> Self {
        let underlying = underlying.into();

        Self {
            chain: Arc::new(OptionChainOrderBook::new(&underlying, expiration)),
            underlying,
            expiration,
            id: OrderId::new(),
            registry: None,
            symbol_index: None,
        }
    }

    /// Creates a new expiration order book with an instrument registry.
    ///
    /// The registry is propagated to the internal [`OptionChainOrderBook`]
    /// and its strike manager.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `expiration` - The expiration date
    /// * `registry` - The instrument registry for ID allocation
    #[must_use]
    pub(crate) fn new_with_registry(
        underlying: impl Into<String>,
        expiration: ExpirationDate,
        registry: Arc<InstrumentRegistry>,
    ) -> Self {
        let underlying = underlying.into();

        Self {
            chain: Arc::new(OptionChainOrderBook::new_with_registry(
                &underlying,
                expiration,
                Arc::clone(&registry),
            )),
            underlying,
            expiration,
            id: OrderId::new(),
            registry: Some(registry),
            symbol_index: None,
        }
    }

    /// Creates a new expiration order book with both instrument registry and symbol index.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `expiration` - The expiration date
    /// * `registry` - The instrument registry for ID allocation
    /// * `symbol_index` - The symbol index for O(1) lookups
    #[must_use]
    pub(crate) fn new_with_registry_and_index(
        underlying: impl Into<String>,
        expiration: ExpirationDate,
        registry: Arc<InstrumentRegistry>,
        symbol_index: Arc<SymbolIndex>,
    ) -> Self {
        let underlying = underlying.into();

        Self {
            chain: Arc::new(OptionChainOrderBook::new_with_registry_and_index(
                &underlying,
                expiration,
                Arc::clone(&registry),
                Arc::clone(&symbol_index),
            )),
            underlying,
            expiration,
            id: OrderId::new(),
            registry: Some(registry),
            symbol_index: Some(symbol_index),
        }
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the expiration date.
    #[must_use = "returns the expiration date without modifying the book"]
    pub const fn expiration(&self) -> &ExpirationDate {
        &self.expiration
    }

    /// Returns the unique identifier for this expiration order book.
    #[must_use]
    pub const fn id(&self) -> OrderId {
        self.id
    }

    /// Returns a reference to the option chain.
    #[must_use]
    pub fn chain(&self) -> &OptionChainOrderBook {
        &self.chain
    }

    /// Returns a reference to the instrument registry, if any.
    #[must_use]
    pub fn registry(&self) -> Option<&Arc<InstrumentRegistry>> {
        self.registry.as_ref()
    }

    /// Returns an Arc reference to the option chain.
    #[must_use]
    pub fn chain_arc(&self) -> Arc<OptionChainOrderBook> {
        Arc::clone(&self.chain)
    }

    /// Returns the contract specifications, if any.
    ///
    /// Delegates to the underlying [`OptionChainOrderBook::specs`].
    #[must_use]
    pub fn specs(&self) -> Option<ContractSpecs> {
        self.chain.specs()
    }

    /// Sets the validation config for all future strikes created within this expiration.
    ///
    /// Delegates to the underlying [`OptionChainOrderBook::set_validation`].
    /// Existing strikes are not affected.
    pub fn set_validation(&self, config: ValidationConfig) {
        self.chain.set_validation(config);
    }

    /// Returns the current validation config, if any.
    #[must_use]
    pub fn validation_config(&self) -> Option<ValidationConfig> {
        self.chain.validation_config()
    }

    /// Sets the STP mode for all future option books created within this expiration.
    ///
    /// Delegates to the underlying [`OptionChainOrderBook::set_stp_mode`].
    /// Existing books are not affected.
    #[inline]
    pub fn set_stp_mode(&self, mode: STPMode) {
        self.chain.set_stp_mode(mode);
    }

    /// Returns the current STP mode.
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.chain.stp_mode()
    }

    /// Sets the fee schedule for all future option books created within this expiration.
    ///
    /// Delegates to the underlying [`OptionChainOrderBook::set_fee_schedule`].
    /// Existing books are not affected.
    #[inline]
    pub fn set_fee_schedule(&self, schedule: FeeSchedule) {
        self.chain.set_fee_schedule(schedule);
    }

    /// Clears the fee schedule so future option books have no fees configured.
    ///
    /// Delegates to the underlying [`OptionChainOrderBook::clear_fee_schedule`].
    /// Existing books are not affected.
    #[inline]
    pub fn clear_fee_schedule(&self) {
        self.chain.clear_fee_schedule();
    }

    /// Returns the current fee schedule, or `None` if no fees are configured.
    #[must_use]
    #[inline]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.chain.fee_schedule()
    }

    /// Sets the engine clock for all future option books created within this expiration.
    ///
    /// Delegates to the underlying [`OptionChainOrderBook::set_clock`].
    /// Existing books are not affected.
    #[inline]
    pub fn set_clock(&self, clock: Arc<dyn Clock>) {
        self.chain.set_clock(clock);
    }

    /// Clears the injected engine clock, so future option books fall back to
    /// the upstream default `MonotonicClock`.
    ///
    /// Delegates to the underlying [`OptionChainOrderBook::clear_clock`].
    /// Existing books are not affected.
    #[inline]
    pub fn clear_clock(&self) {
        self.chain.clear_clock();
    }

    /// Returns the current engine clock, or `None` when future books use the
    /// upstream default `MonotonicClock`.
    #[must_use]
    #[inline]
    pub fn clock(&self) -> Option<Arc<dyn Clock>> {
        self.chain.clock()
    }

    /// Sets the root trade-ID namespace for all future option books created
    /// within this expiration.
    ///
    /// Delegates to [`OptionChainOrderBook::set_trade_id_namespace`](super::chain::OptionChainOrderBook::set_trade_id_namespace).
    /// Existing books are not affected.
    #[inline]
    pub fn set_trade_id_namespace(&self, namespace: Uuid) {
        self.chain.set_trade_id_namespace(namespace);
    }

    /// Clears the root trade-ID namespace, so future option books use the
    /// upstream default random namespace.
    ///
    /// Delegates to [`OptionChainOrderBook::clear_trade_id_namespace`](super::chain::OptionChainOrderBook::clear_trade_id_namespace).
    /// Existing books are not affected.
    #[inline]
    pub fn clear_trade_id_namespace(&self) {
        self.chain.clear_trade_id_namespace();
    }

    /// Returns the current root trade-ID namespace, or `None` when future books
    /// use the upstream default random namespace.
    #[must_use]
    #[inline]
    pub fn trade_id_namespace(&self) -> Option<Uuid> {
        self.chain.trade_id_namespace()
    }

    /// Propagates the per-contract NATS listener factory down to this
    /// expiration's chain (and onward to its strike manager).
    ///
    /// Delegates to [`OptionChainOrderBook::set_nats_factory`]. Existing books
    /// are not affected.
    #[cfg(feature = "nats")]
    #[inline]
    pub(crate) fn set_nats_factory(&self, factory: Option<ContractNatsListenerFactory>) {
        self.chain.set_nats_factory(factory);
    }

    /// Cancels all resting orders across the expiration's option chain.
    ///
    /// # Description
    ///
    /// Cancels every resting order across the chain for this expiration and
    /// returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// An [`ExpirationMassCancelResult`] containing per-chain results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ExpirationOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    ///
    /// let book = ExpirationOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)));
    /// let result = match book.cancel_all() {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_all(&self) -> Result<ExpirationMassCancelResult> {
        let result = self.chain.cancel_all()?;

        Ok(ExpirationMassCancelResult {
            per_child: vec![(self.expiration.to_string(), result)],
        })
    }

    /// Cancels all resting orders on a specific side across the expiration's chain.
    ///
    /// # Description
    ///
    /// Cancels every resting order on the provided side across the chain for
    /// this expiration and returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// * `side` - Side to cancel ([`Side::Buy`] or [`Side::Sell`]).
    ///
    /// # Returns
    ///
    /// An [`ExpirationMassCancelResult`] containing per-chain results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{ExpirationOrderBook, Side};
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    ///
    /// let book = ExpirationOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)));
    /// let result = match book.cancel_by_side(Side::Buy) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_side(&self, side: Side) -> Result<ExpirationMassCancelResult> {
        let result = self.chain.cancel_by_side(side)?;

        Ok(ExpirationMassCancelResult {
            per_child: vec![(self.expiration.to_string(), result)],
        })
    }

    /// Cancels all resting orders for a specific user across the expiration's chain.
    ///
    /// # Description
    ///
    /// Cancels every resting order attributed to the provided user identifier
    /// across the chain for this expiration and returns the aggregated
    /// cancellation details.
    ///
    /// # Arguments
    ///
    /// * `user_id` - User identifier to cancel (32-byte hash).
    ///
    /// # Returns
    ///
    /// An [`ExpirationMassCancelResult`] containing per-chain results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ExpirationOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    /// use pricelevel::Hash32;
    ///
    /// let book = ExpirationOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)));
    /// let user = Hash32::from([1u8; 32]);
    /// let result = match book.cancel_by_user(user) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_user(&self, user_id: Hash32) -> Result<ExpirationMassCancelResult> {
        let result = self.chain.cancel_by_user(user_id)?;

        Ok(ExpirationMassCancelResult {
            per_child: vec![(self.expiration.to_string(), result)],
        })
    }

    /// Evicts expired `GTD` / `DAY` orders across this expiration's chain.
    ///
    /// # Description
    ///
    /// Runs the host-driven expiry sweep across the expiration's chain and
    /// returns the aggregated result. `now_ms` is a caller-supplied
    /// Unix-milliseconds cutoff; the sweep reads no clock, so it is a pure
    /// function of `now_ms` and the resting books and replays identically. The
    /// chain is walked in ascending strike order and each leaf book in the
    /// engine's deterministic eviction order — the same traversal
    /// [`cancel_all`](Self::cancel_all) uses.
    ///
    /// Expiry is realized only when the sweep runs: an order past its deadline
    /// that has not yet been swept still rests and remains matchable.
    ///
    /// # Arguments
    ///
    /// * `now_ms` - Caller-supplied Unix-milliseconds cutoff.
    ///
    /// # Returns
    ///
    /// An [`ExpirationEvictExpiredResult`] containing per-chain results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ExpirationOrderBook;
    /// use option_chain_orderbook::TimestampMs;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    ///
    /// let book = ExpirationOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)));
    /// let result = book.evict_expired_orders(TimestampMs::new(10_000_000_000_000));
    /// assert_eq!(result.total_evicted(), 0);
    /// ```
    pub fn evict_expired_orders(&self, now_ms: TimestampMs) -> ExpirationEvictExpiredResult {
        let result = self.chain.evict_expired_orders(now_ms);

        ExpirationEvictExpiredResult {
            per_child: vec![(self.expiration.to_string(), result)],
        }
    }

    // ── Order Lifecycle Queries ────────────────────────────────────────────

    /// Finds an order anywhere in this expiration's chain.
    ///
    /// # Description
    ///
    /// Delegates to the underlying chain. Returns the option symbol and
    /// current status if found.
    ///
    /// # Arguments
    ///
    /// * `order_id` - The ID of the order to find.
    ///
    /// # Returns
    ///
    /// `Some((symbol, status))` if found, `None` otherwise.
    ///
    /// # Errors
    ///
    /// None.
    #[must_use]
    pub fn find_order(&self, order_id: OrderId) -> Option<(String, OrderStatus)> {
        self.chain.find_order(order_id)
    }

    /// Returns the total number of active orders in the chain.
    ///
    /// # Description
    ///
    /// Delegates to the underlying chain.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Total active order count.
    ///
    /// # Errors
    ///
    /// None.
    #[must_use]
    pub fn total_active_orders(&self) -> usize {
        self.chain.total_active_orders()
    }

    /// Removes terminal-state entries older than the specified duration.
    ///
    /// # Description
    ///
    /// Delegates to the underlying chain and returns the total purged.
    ///
    /// # Arguments
    ///
    /// * `older_than` - Entries older than this duration are removed.
    ///
    /// # Returns
    ///
    /// The number of entries purged.
    ///
    /// # Errors
    ///
    /// None.
    pub fn purge_terminal_states(&self, older_than: Duration) -> usize {
        self.chain.purge_terminal_states(older_than)
    }

    /// Returns all currently active orders for a specific user.
    ///
    /// # Description
    ///
    /// Delegates to the underlying chain. Returns tuples of
    /// (symbol, order_id, status).
    ///
    /// # Arguments
    ///
    /// * `user_id` - The user identifier to filter by.
    ///
    /// # Returns
    ///
    /// A vector of `(symbol, OrderId, OrderStatus)` tuples.
    ///
    /// # Errors
    ///
    /// None.
    #[must_use]
    pub fn orders_by_user(&self, user_id: Hash32) -> Vec<(String, OrderId, OrderStatus)> {
        self.chain.orders_by_user(user_id)
    }

    /// Returns a summary of terminal order transitions.
    ///
    /// # Description
    ///
    /// Delegates to the underlying chain.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// A [`TerminalOrderSummary`] with aggregated filled, cancelled, and
    /// rejected counts.
    ///
    /// # Errors
    ///
    /// None.
    #[must_use]
    pub fn terminal_order_summary(&self) -> TerminalOrderSummary {
        self.chain.terminal_order_summary()
    }

    /// Gets or creates a strike order book, returning an Arc reference.
    pub fn get_or_create_strike(&self, strike: u64) -> Arc<StrikeOrderBook> {
        self.chain.get_or_create_strike(strike)
    }

    /// Gets a strike order book.
    ///
    /// # Errors
    ///
    /// Returns `Error::StrikeNotFound` if the strike does not exist.
    pub fn get_strike(&self, strike: u64) -> Result<Arc<StrikeOrderBook>> {
        self.chain.get_strike(strike)
    }

    /// Returns the number of strikes.
    #[must_use]
    pub fn strike_count(&self) -> usize {
        self.chain.strike_count()
    }

    /// Returns true if there are no strikes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }

    /// Returns all strike prices (sorted).
    pub fn strike_prices(&self) -> Vec<u64> {
        self.chain.strike_prices()
    }

    /// Returns the total order count.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.chain.total_order_count()
    }

    /// Returns the strike count and total order count in a single ordered pass.
    ///
    /// Delegates to [`OptionChainOrderBook::strike_and_order_counts`], walking
    /// this expiration's chain once for the single-pass `stats()` aggregation.
    #[must_use]
    pub(crate) fn strike_and_order_counts(&self) -> (usize, usize) {
        self.chain.strike_and_order_counts()
    }

    /// Returns the ATM strike closest to the given spot price.
    ///
    /// Delegates to
    /// [`StrikeOrderBookManager::atm_strike`](super::strike::StrikeOrderBookManager::atm_strike);
    /// see it for the selection rule (nearest strike, lower strike on a tie).
    ///
    /// # Errors
    ///
    /// Returns `Error::NoDataAvailable` if there are no strikes.
    pub fn atm_strike(&self, spot: u64) -> Result<u64> {
        self.chain.atm_strike(spot)
    }
}

/// Manages expiration order books for a single underlying.
///
/// Provides centralized access to all expirations for an underlying asset.
/// Uses `SkipMap` for thread-safe concurrent access.
pub struct ExpirationOrderBookManager {
    /// Expiration order books indexed by a deterministic [`ExpirationKey`].
    ///
    /// Keyed on [`ExpirationKey`] rather than [`ExpirationDate`] directly
    /// because the latter's `Ord`/`Eq` is wall-clock-relative and collides
    /// distinct expirations (see [`ExpirationKey`] docs). Each stored
    /// [`ExpirationOrderBook`] retains its original [`ExpirationDate`], so the
    /// public API still hands back `ExpirationDate` values.
    expirations: SkipMap<ExpirationKey, Arc<ExpirationOrderBook>>,
    /// The underlying asset symbol.
    underlying: String,
    /// Validation config applied to newly created expiration books.
    validation_config: Shared<Option<ValidationConfig>>,
    /// Contract specs propagated to newly created expiration books.
    contract_specs: Shared<Option<ContractSpecs>>,
    /// Instrument registry propagated to newly created expiration books.
    registry: Option<Arc<InstrumentRegistry>>,
    /// Symbol index for O(1) lookup by symbol string.
    symbol_index: Option<Arc<SymbolIndex>>,
    /// STP mode propagated to newly created expiration books.
    stp_mode: Shared<STPMode>,
    /// Fee schedule propagated to newly created expiration books.
    fee_schedule: Shared<Option<FeeSchedule>>,
    /// Engine clock propagated to newly created expiration books. `None` keeps
    /// the upstream default `MonotonicClock`; a shared `Arc<dyn Clock>` makes
    /// time-in-force admission deterministic for replay.
    clock: Shared<Option<Arc<dyn Clock>>>,
    /// Root trade-ID namespace propagated to newly created expiration books.
    /// `None` keeps the upstream default random namespace; a set root makes
    /// trade IDs reproducible across runs sharing the root.
    trade_id_root_namespace: Shared<Option<Uuid>>,
    /// Per-contract NATS listener factory propagated to newly created
    /// expiration books (and onward to their strikes). `None` (the default)
    /// reproduces the non-NATS path exactly.
    #[cfg(feature = "nats")]
    nats_factory: SharedNatsFactory,
}

impl ExpirationOrderBookManager {
    /// Creates a new expiration order book manager.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    #[must_use]
    pub fn new(underlying: impl Into<String>) -> Self {
        Self {
            expirations: SkipMap::new(),
            underlying: underlying.into(),
            validation_config: Shared::new(None),
            contract_specs: Shared::new(None),
            registry: None,
            symbol_index: None,
            stp_mode: Shared::new(STPMode::None),
            fee_schedule: Shared::new(None),
            clock: Shared::new(None),
            trade_id_root_namespace: Shared::new(None),
            #[cfg(feature = "nats")]
            nats_factory: SharedNatsFactory::new(),
        }
    }

    /// Creates a new expiration order book manager with an instrument registry.
    ///
    /// The registry is propagated to newly created expiration books and
    /// their chains.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `registry` - The instrument registry for ID allocation
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn new_with_registry(
        underlying: impl Into<String>,
        registry: Arc<InstrumentRegistry>,
    ) -> Self {
        Self {
            expirations: SkipMap::new(),
            underlying: underlying.into(),
            validation_config: Shared::new(None),
            contract_specs: Shared::new(None),
            registry: Some(registry),
            symbol_index: None,
            stp_mode: Shared::new(STPMode::None),
            fee_schedule: Shared::new(None),
            clock: Shared::new(None),
            trade_id_root_namespace: Shared::new(None),
            #[cfg(feature = "nats")]
            nats_factory: SharedNatsFactory::new(),
        }
    }

    /// Creates a new expiration order book manager with both instrument registry and symbol index.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `registry` - The instrument registry for ID allocation
    /// * `symbol_index` - The symbol index for O(1) lookups
    #[must_use]
    pub(crate) fn new_with_registry_and_index(
        underlying: impl Into<String>,
        registry: Arc<InstrumentRegistry>,
        symbol_index: Arc<SymbolIndex>,
    ) -> Self {
        Self {
            expirations: SkipMap::new(),
            underlying: underlying.into(),
            validation_config: Shared::new(None),
            contract_specs: Shared::new(None),
            registry: Some(registry),
            symbol_index: Some(symbol_index),
            stp_mode: Shared::new(STPMode::None),
            fee_schedule: Shared::new(None),
            clock: Shared::new(None),
            trade_id_root_namespace: Shared::new(None),
            #[cfg(feature = "nats")]
            nats_factory: SharedNatsFactory::new(),
        }
    }

    /// Sets the contract specs for all future expirations created by this manager.
    ///
    /// Existing expiration books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will have these specs propagated.
    /// The specs' `[min_price, max_price]` price band is enforced crate-side at
    /// the leaf (see [`OptionChainOrderBook::set_specs`](super::chain::OptionChainOrderBook::set_specs)).
    pub fn set_specs(&self, specs: ContractSpecs) {
        self.contract_specs.set(Some(specs));
    }

    /// Returns the current contract specs, if any.
    #[must_use]
    pub fn specs(&self) -> Option<ContractSpecs> {
        self.contract_specs.get()
    }

    /// Sets the validation config for all future expirations created by this manager.
    ///
    /// Existing expiration books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will have this config applied.
    pub fn set_validation(&self, config: ValidationConfig) {
        self.validation_config.set(Some(config));
    }

    /// Returns the current validation config, if any.
    #[must_use]
    pub fn validation_config(&self) -> Option<ValidationConfig> {
        self.validation_config.get()
    }

    /// Sets the STP mode for all future expiration books created by this manager.
    ///
    /// Existing books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will have this mode propagated.
    #[inline]
    pub fn set_stp_mode(&self, mode: STPMode) {
        self.stp_mode.set(mode);
    }

    /// Returns the current STP mode.
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.stp_mode.get()
    }

    /// Sets the fee schedule for all future expiration books created by this manager.
    ///
    /// Existing books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will have this schedule propagated.
    #[inline]
    pub fn set_fee_schedule(&self, schedule: FeeSchedule) {
        self.fee_schedule.set(Some(schedule));
    }

    /// Clears the fee schedule so future expiration books have no fees configured.
    ///
    /// Existing books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will be affected.
    #[inline]
    pub fn clear_fee_schedule(&self) {
        self.fee_schedule.set(None);
    }

    /// Returns the current fee schedule, or `None` if no fees are configured.
    #[must_use]
    #[inline]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.fee_schedule.get()
    }

    /// Sets the engine clock for all future expiration books created by this manager.
    ///
    /// Existing books are not affected. Only newly created books via
    /// [`get_or_create`](Self::get_or_create) will have this clock propagated
    /// down to their chains and strikes. The `Arc<dyn Clock>` is shared, not
    /// deep-cloned, so every future option book stamps orders from the same
    /// clock — inject a `StubClock` to make time-in-force admission
    /// deterministic for replay.
    #[inline]
    pub fn set_clock(&self, clock: Arc<dyn Clock>) {
        self.clock.set(Some(clock));
    }

    /// Clears the engine clock so future expiration books use the upstream
    /// default `MonotonicClock` (wall-clock time).
    ///
    /// Existing books are not affected. Only newly created books via
    /// [`get_or_create`](Self::get_or_create) will be affected.
    #[inline]
    pub fn clear_clock(&self) {
        self.clock.set(None);
    }

    /// Returns the current engine clock, or `None` when future books use the
    /// upstream default `MonotonicClock`.
    #[must_use]
    #[inline]
    pub fn clock(&self) -> Option<Arc<dyn Clock>> {
        self.clock.get()
    }

    /// Sets the root trade-ID namespace for all future expiration books created
    /// by this manager.
    ///
    /// Existing books are not affected. Only newly created books via
    /// [`get_or_create`](Self::get_or_create) will have this root propagated down
    /// to their chains and strikes, where each leaf derives `UUIDv5(root, symbol)`.
    #[inline]
    pub fn set_trade_id_namespace(&self, namespace: Uuid) {
        self.trade_id_root_namespace.set(Some(namespace));
    }

    /// Clears the root trade-ID namespace so future expiration books use the
    /// upstream default random namespace.
    ///
    /// Existing books are not affected. Only newly created books via
    /// [`get_or_create`](Self::get_or_create) will be affected.
    #[inline]
    pub fn clear_trade_id_namespace(&self) {
        self.trade_id_root_namespace.set(None);
    }

    /// Returns the current root trade-ID namespace, or `None` when future books
    /// use the upstream default random namespace.
    #[must_use]
    #[inline]
    pub fn trade_id_namespace(&self) -> Option<Uuid> {
        self.trade_id_root_namespace.get()
    }

    /// Stores the per-contract NATS listener factory propagated from the top of
    /// the hierarchy and forwarded to every future expiration book.
    ///
    /// Existing expiration books are not affected; only books created by a
    /// later [`get_or_create`](Self::get_or_create) carry the factory down to
    /// their strikes. Mirrors [`set_fee_schedule`](Self::set_fee_schedule)
    /// propagation.
    #[cfg(feature = "nats")]
    pub(crate) fn set_nats_factory(&self, factory: Option<ContractNatsListenerFactory>) {
        self.nats_factory.set(factory);
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the number of expirations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.expirations.len()
    }

    /// Returns true if there are no expirations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.expirations.is_empty()
    }

    /// Gets or creates an expiration order book.
    ///
    /// If a validation config has been set via [`set_validation`](Self::set_validation),
    /// newly created expiration books will have that config propagated to their chain.
    ///
    /// # Concurrency
    ///
    /// This is atomic and idempotent. The fresh expiration book is fully
    /// configured before it is published, and publishing uses
    /// [`SkipMap::get_or_insert`], which inserts only when the key is absent and
    /// never evicts an existing entry. The first inserter wins and is never
    /// orphaned; concurrent losers receive the winning handle and drop their
    /// fresh book. Building and configuring the book has no global side effects
    /// (instrument IDs and symbol-index entries are only allocated lazily at
    /// strike creation), so a dropped loser leaks nothing.
    pub fn get_or_create(&self, expiration: ExpirationDate) -> Arc<ExpirationOrderBook> {
        self.get_or_create_inserted(expiration).0
    }

    /// Gets or creates an expiration order book, reporting whether this call
    /// performed the insertion.
    ///
    /// Returns `(book, inserted)` where `inserted` is `true` for exactly one
    /// caller across all concurrent callers for a given expiration — the one
    /// whose freshly built book actually won the atomic
    /// [`SkipMap::get_or_insert`] publish. Every other caller (including the
    /// fast path that observes a pre-existing entry, and concurrent race losers
    /// whose fresh book is dropped) receives `inserted == false`. The returned
    /// `book` is always the single winning handle.
    ///
    /// The `inserted` bit is the canonical signal for "this expiration was
    /// genuinely created now" — callers must NOT re-probe with
    /// [`get`](Self::get) to decide newness, because a separate probe reopens a
    /// check-then-act race that double-counts a date under concurrency.
    ///
    /// # Concurrency
    ///
    /// Identical guarantees to [`get_or_create`](Self::get_or_create): atomic,
    /// idempotent, lock-free. The winner is detected via
    /// [`Arc::ptr_eq`] against the locally built handle, so the `inserted` flag
    /// is derived purely from the atomic insert result — no follow-up map probe
    /// after `get_or_insert` (the up-front `get` fast-path still applies).
    #[must_use]
    pub fn get_or_create_inserted(
        &self,
        expiration: ExpirationDate,
    ) -> (Arc<ExpirationOrderBook>, bool) {
        let key = ExpirationKey::from(&expiration);
        if let Some(entry) = self.expirations.get(&key) {
            return (Arc::clone(entry.value()), false);
        }
        // Build and fully configure the fresh book BEFORE publishing it, so the
        // winning book is configured-before-visible. Configuration only mutates
        // the fresh object (no global side effects), so a race loser's book is
        // dropped harmlessly.
        let fresh = match (&self.registry, &self.symbol_index) {
            (Some(reg), Some(idx)) => Arc::new(ExpirationOrderBook::new_with_registry_and_index(
                &self.underlying,
                expiration,
                Arc::clone(reg),
                Arc::clone(idx),
            )),
            (Some(reg), None) => Arc::new(ExpirationOrderBook::new_with_registry(
                &self.underlying,
                expiration,
                Arc::clone(reg),
            )),
            _ => Arc::new(ExpirationOrderBook::new(&self.underlying, expiration)),
        };
        if let Some(ref config) = self.validation_config.get() {
            fresh.set_validation(config.clone());
        }
        if let Some(ref specs) = self.contract_specs.get() {
            fresh.chain().set_specs(specs.clone());
        }
        let stp = self.stp_mode.get();
        if stp != STPMode::None {
            fresh.set_stp_mode(stp);
        }
        if let Some(schedule) = self.fee_schedule.get() {
            fresh.set_fee_schedule(schedule);
        }
        if let Some(clock) = self.clock.get() {
            fresh.set_clock(clock);
        }
        if let Some(ns) = self.trade_id_root_namespace.get() {
            fresh.set_trade_id_namespace(ns);
        }
        // Propagate the per-contract NATS factory down to the fresh chain/strike
        // manager BEFORE publishing, so the configured-before-visible invariant
        // holds: any later strike created under this expiration installs its
        // publishers. Done only when a factory is configured to keep the
        // no-factory path identical.
        #[cfg(feature = "nats")]
        if let Some(factory) = self.nats_factory.get() {
            fresh.set_nats_factory(Some(factory));
        }
        // Keep a handle to the locally built book so we can identify whether it
        // won the race after the atomic publish.
        let candidate = Arc::clone(&fresh);
        // Atomic, idempotent publish: first inserter wins and is never evicted.
        let entry = self.expirations.get_or_insert(key, fresh);
        // `inserted` is true iff the published value is the book we just built,
        // i.e. this call is the unique `get_or_insert` winner for this key.
        let inserted = Arc::ptr_eq(entry.value(), &candidate);
        if inserted {
            // Cold path: emitted once per truly-new expiration (the unique
            // `get_or_insert` winner), for every creating caller — direct
            // `get_or_create_expiration` and the expiry scheduler alike. Never on
            // the order-submission path.
            tracing::info!(
                underlying = %self.underlying,
                expiration = %expiration,
                "expiration created",
            );
        }
        (Arc::clone(entry.value()), inserted)
    }

    /// Gets an expiration order book.
    ///
    /// # Errors
    ///
    /// Returns `Error::ExpirationNotFound` if the expiration does not exist.
    pub fn get(&self, expiration: &ExpirationDate) -> Result<Arc<ExpirationOrderBook>> {
        self.expirations
            .get(&ExpirationKey::from(expiration))
            .map(|e| Arc::clone(e.value()))
            .ok_or_else(|| Error::expiration_not_found(expiration.to_string()))
    }

    /// Returns true if an expiration exists.
    #[must_use]
    pub fn contains(&self, expiration: &ExpirationDate) -> bool {
        self.expirations
            .contains_key(&ExpirationKey::from(expiration))
    }

    /// Returns an iterator over all expirations, ordered by expiration.
    ///
    /// Yields `(ExpirationDate, Arc<ExpirationOrderBook>)` tuples in ascending
    /// order. Ordering is driven by an internal deterministic expiration key
    /// (clock-independent and collision-free, unlike `ExpirationDate`'s own
    /// `Ord`), so the traversal order is stable and replay-safe. The
    /// `ExpirationDate` is the original value stored in each book; the internal
    /// key is never exposed.
    pub fn iter(&self) -> impl Iterator<Item = (ExpirationDate, Arc<ExpirationOrderBook>)> + '_ {
        self.expirations
            .iter()
            .map(|e| (*e.value().expiration(), Arc::clone(e.value())))
    }

    /// Removes an expiration order book.
    pub fn remove(&self, expiration: &ExpirationDate) -> bool {
        self.expirations
            .remove(&ExpirationKey::from(expiration))
            .is_some()
    }

    /// Returns the total order count across all expirations.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.expirations
            .iter()
            .map(|e| e.value().total_order_count())
            .sum()
    }

    /// Returns the total strike count across all expirations.
    #[must_use]
    pub fn total_strike_count(&self) -> usize {
        self.expirations
            .iter()
            .map(|e| e.value().strike_count())
            .sum()
    }

    /// Returns statistics about this expiration manager.
    #[must_use]
    pub fn stats(&self) -> ExpirationManagerStats {
        ExpirationManagerStats {
            underlying: self.underlying.clone(),
            expiration_count: self.len(),
            total_strikes: self.total_strike_count(),
            total_orders: self.total_order_count(),
        }
    }

    /// Returns a single-pass [`SubtreeStats`] tally for this manager's
    /// expirations.
    ///
    /// Walks the ordered expiration [`SkipMap`] exactly once and, for each
    /// expiration, walks its strikes exactly once, accumulating the expiration,
    /// strike, and order counts together. This replaces the three independent
    /// subtree walks (`len()` + `total_strike_count()` + `total_order_count()`)
    /// with one, yielding a coherent snapshot for the underlying-level
    /// `stats()` even under concurrent mutation.
    #[must_use]
    pub(crate) fn subtree_stats(&self) -> SubtreeStats {
        let mut acc = SubtreeStats::default();
        for entry in self.expirations.iter() {
            let (strikes, orders) = entry.value().strike_and_order_counts();
            acc.add_expiration(strikes, orders);
        }
        acc
    }
}

/// Single-pass tally of an underlying subtree's expiration, strike, and order
/// counts.
///
/// Internal accumulator consumed by the `stats()` methods on
/// `UnderlyingOrderBook` and `UnderlyingOrderBookManager`. It is filled in ONE
/// leaf-up traversal of the ordered `SkipMap`s — each expiration is visited
/// once and its strikes walked once — so the three counts form a coherent
/// snapshot taken at a single point in the walk, unlike the previous three
/// independent `expiration_count()` + `total_strike_count()` +
/// `total_order_count()` passes which could each observe the tree at a
/// different moment under concurrent mutation. Counters accumulate with checked
/// addition (capping at `usize::MAX` on the structurally unreachable overflow)
/// rather than wrapping, keeping the tally monotonic without a panic.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SubtreeStats {
    /// Number of expirations visited.
    pub(crate) expirations: usize,
    /// Total number of strikes across the visited expirations.
    pub(crate) strikes: usize,
    /// Total number of resting orders across the visited strikes.
    pub(crate) orders: usize,
}

impl SubtreeStats {
    /// Folds one expiration's `(strikes, orders)` tally into the accumulator,
    /// incrementing the expiration count by one. Uses checked addition.
    #[inline]
    pub(crate) fn add_expiration(&mut self, strikes: usize, orders: usize) {
        self.expirations = checked_accumulate(self.expirations, 1);
        self.strikes = checked_accumulate(self.strikes, strikes);
        self.orders = checked_accumulate(self.orders, orders);
    }

    /// Folds another subtree tally (e.g. one underlying's) into this one. Uses
    /// checked addition.
    #[inline]
    pub(crate) fn merge(&mut self, other: Self) {
        self.expirations = checked_accumulate(self.expirations, other.expirations);
        self.strikes = checked_accumulate(self.strikes, other.strikes);
        self.orders = checked_accumulate(self.orders, other.orders);
    }
}

/// Statistics about an expiration manager.
#[derive(Debug, Clone)]
pub struct ExpirationManagerStats {
    /// The underlying asset symbol.
    pub underlying: String,
    /// Number of expirations.
    pub expiration_count: usize,
    /// Total number of strikes across all expirations.
    pub total_strikes: usize,
    /// Total number of orders across all expirations.
    pub total_orders: usize,
}

impl std::fmt::Display for ExpirationManagerStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} expirations, {} strikes, {} orders",
            self.underlying, self.expiration_count, self.total_strikes, self.total_orders
        )
    }
}

/// Expiration-level mass cancel summary.
///
/// # Description
///
/// Aggregates per-chain mass cancel results for a single expiration.
///
/// # Arguments
///
/// None.
///
/// # Returns
///
/// Use [`books_affected`](Self::books_affected) and [`total_cancelled`](Self::total_cancelled)
/// for aggregated counts.
///
/// # Errors
///
/// None.
///
/// # Examples
///
/// ```rust,no_run
/// use option_chain_orderbook::orderbook::ExpirationMassCancelResult;
///
/// let result = ExpirationMassCancelResult { per_child: Vec::new() };
/// assert_eq!(result.total_cancelled(), 0);
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct ExpirationMassCancelResult {
    /// Per-chain cancellation results keyed by expiration.
    pub per_child: Vec<(String, ChainMassCancelResult)>,
}

impl ExpirationMassCancelResult {
    /// Returns the number of leaf option books with cancelled orders.
    ///
    /// # Description
    ///
    /// Drills into each per-chain result and sums its affected leaf
    /// [`OptionOrderBook`](super::book::OptionOrderBook)s (call/put contract
    /// books). The unit is a leaf contract book — identical to the unit reported
    /// at every other level — so results aggregate cleanly up the tree and match
    /// the same count read at the chain / underlying / global levels. This is NOT
    /// a count of affected chains.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Number of leaf option books affected (call/put contract books).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ExpirationMassCancelResult;
    ///
    /// let result = ExpirationMassCancelResult { per_child: Vec::new() };
    /// assert_eq!(result.books_affected(), 0);
    /// ```
    #[must_use]
    pub fn books_affected(&self) -> usize {
        self.per_child
            .iter()
            .map(|(_, result)| result.books_affected())
            .sum()
    }

    /// Returns the total number of cancelled orders across the expiration.
    ///
    /// # Description
    ///
    /// Sums cancelled orders across the chain for this expiration.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Total cancelled orders (orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ExpirationMassCancelResult;
    ///
    /// let result = ExpirationMassCancelResult { per_child: Vec::new() };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    #[must_use]
    pub fn total_cancelled(&self) -> usize {
        self.per_child
            .iter()
            .map(|(_, result)| result.total_cancelled())
            .sum()
    }
}

/// Aggregated result of an expiry sweep across an expiration's chain.
///
/// The eviction analogue of [`ExpirationMassCancelResult`]: `per_child` carries
/// the single per-chain [`ChainEvictExpiredResult`] keyed by expiration. The
/// aggregate accessors report the leaf-contract-book unit, identical to the
/// mass-cancel counterpart.
///
/// # Examples
///
/// ```rust,no_run
/// use option_chain_orderbook::orderbook::ExpirationEvictExpiredResult;
///
/// let result = ExpirationEvictExpiredResult { per_child: Vec::new() };
/// assert_eq!(result.total_evicted(), 0);
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct ExpirationEvictExpiredResult {
    /// Per-chain eviction results keyed by expiration.
    pub per_child: Vec<(String, ChainEvictExpiredResult)>,
}

impl ExpirationEvictExpiredResult {
    /// Returns the number of leaf option books with evicted orders.
    ///
    /// # Description
    ///
    /// Drills into each per-chain result and sums its affected leaf
    /// [`OptionOrderBook`](super::book::OptionOrderBook)s (call/put contract
    /// books). The unit is a leaf contract book — identical to the unit reported
    /// at every other level — so results aggregate cleanly up the tree. This is
    /// NOT a count of affected chains.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Number of leaf option books affected (call/put contract books).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ExpirationEvictExpiredResult;
    ///
    /// let result = ExpirationEvictExpiredResult { per_child: Vec::new() };
    /// assert_eq!(result.books_affected(), 0);
    /// ```
    #[must_use]
    pub fn books_affected(&self) -> usize {
        self.per_child
            .iter()
            .map(|(_, result)| result.books_affected())
            .sum()
    }

    /// Returns the total number of evicted orders across the expiration.
    ///
    /// # Description
    ///
    /// Sums evicted orders across the expiration's chain.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Total evicted orders (orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ExpirationEvictExpiredResult;
    ///
    /// let result = ExpirationEvictExpiredResult { per_child: Vec::new() };
    /// assert_eq!(result.total_evicted(), 0);
    /// ```
    #[must_use]
    pub fn total_evicted(&self) -> usize {
        self.per_child
            .iter()
            .map(|(_, result)| result.total_evicted())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optionstratlib::prelude::pos_or_panic;
    use orderbook_rs::{OrderId, Side};
    use pricelevel::Hash32;

    fn test_expiration() -> ExpirationDate {
        ExpirationDate::Days(pos_or_panic!(30.0))
    }

    #[test]
    fn test_expiration_cancel_all() {
        let exp = ExpirationOrderBook::new("BTC", test_expiration());

        let s1 = exp.get_or_create_strike(50000);
        if let Err(err) = s1
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = s1.put().add_limit_order(OrderId::new(), Side::Sell, 60, 5) {
            panic!("add order failed: {}", err);
        }
        drop(s1);

        let s2 = exp.get_or_create_strike(52000);
        if let Err(err) = s2.call().add_limit_order(OrderId::new(), Side::Buy, 80, 10) {
            panic!("add order failed: {}", err);
        }
        drop(s2);

        assert_eq!(exp.total_order_count(), 3);

        let result = match exp.cancel_all() {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 3);
        assert_eq!(exp.total_order_count(), 0);
    }

    #[test]
    fn test_expiration_cancel_by_side() {
        let exp = ExpirationOrderBook::new("BTC", test_expiration());

        let s1 = exp.get_or_create_strike(50000);
        if let Err(err) = s1
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = s1
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 110, 5)
        {
            panic!("add order failed: {}", err);
        }
        drop(s1);

        assert_eq!(exp.total_order_count(), 2);

        let result = match exp.cancel_by_side(Side::Sell) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 1);
        assert_eq!(exp.total_order_count(), 1);
    }

    #[test]
    fn test_expiration_cancel_by_user() {
        let exp = ExpirationOrderBook::new("BTC", test_expiration());
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        let s1 = exp.get_or_create_strike(50000);
        if let Err(err) =
            s1.call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
        {
            panic!("add order failed: {}", err);
        }
        drop(s1);

        let s2 = exp.get_or_create_strike(52000);
        if let Err(err) =
            s2.put()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 60, 5, user_b)
        {
            panic!("add order failed: {}", err);
        }
        drop(s2);

        assert_eq!(exp.total_order_count(), 2);

        let result = match exp.cancel_by_user(user_a) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 1);
        assert_eq!(exp.total_order_count(), 1);
    }

    #[test]
    fn test_expiration_order_book_creation() {
        let exp = ExpirationOrderBook::new("BTC", test_expiration());

        assert_eq!(exp.underlying(), "BTC");
        assert!(exp.is_empty());
    }

    #[test]
    fn test_expiration_order_book_strikes() {
        let exp = ExpirationOrderBook::new("BTC", test_expiration());

        drop(exp.get_or_create_strike(50000));
        drop(exp.get_or_create_strike(55000));
        drop(exp.get_or_create_strike(45000));

        assert_eq!(exp.strike_count(), 3);
        assert_eq!(exp.strike_prices(), vec![45000, 50000, 55000]);
    }

    #[test]
    fn test_expiration_order_book_orders() {
        let exp = ExpirationOrderBook::new("BTC", test_expiration());

        let strike = exp.get_or_create_strike(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(exp.total_order_count(), 1);
    }

    #[test]
    fn test_expiration_manager_creation() {
        let manager = ExpirationOrderBookManager::new("BTC");

        assert!(manager.is_empty());
        assert_eq!(manager.underlying(), "BTC");
    }

    #[test]
    fn test_expiration_manager_get_or_create() {
        let manager = ExpirationOrderBookManager::new("BTC");

        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(30.0))));
        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(60.0))));
        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(90.0))));

        assert_eq!(manager.len(), 3);
    }

    #[test]
    fn test_expiration_manager_get_or_create_returns_same_handle() {
        let manager = ExpirationOrderBookManager::new("BTC");
        let exp = test_expiration();

        let first = manager.get_or_create(exp);
        let second = manager.get_or_create(exp);

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_expiration_manager_get_or_create_concurrent_returns_same_handle() {
        use std::sync::Barrier;
        use std::thread;

        // Race N threads to create the SAME expiration via a barrier for
        // lockstep. With `get_or_insert` the first inserter wins and is never
        // evicted, so every thread must observe one identical handle and the
        // map must hold exactly one entry (no orphan, no split-brain).
        const N: usize = 16;
        let manager = Arc::new(ExpirationOrderBookManager::new("BTC"));
        let barrier = Arc::new(Barrier::new(N));
        let exp = test_expiration();

        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                manager.get_or_create(exp)
            }));
        }

        let books: Vec<Arc<ExpirationOrderBook>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        let first = books.first().expect("no books returned");
        for book in &books {
            assert!(Arc::ptr_eq(first, book));
        }
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_expiration_manager_same_day_of_month_distinct_months_both_retrievable() {
        use chrono::{TimeZone, Utc};

        // Regression for issue #50: two `DateTime` expirations sharing a
        // day-of-month across different months must not collide into one
        // `SkipMap` slot.
        let jan = match Utc.with_ymd_and_hms(2026, 1, 15, 8, 0, 0) {
            chrono::offset::LocalResult::Single(dt) => ExpirationDate::DateTime(dt),
            _ => panic!("invalid jan fixture"),
        };
        let feb = match Utc.with_ymd_and_hms(2026, 2, 15, 8, 0, 0) {
            chrono::offset::LocalResult::Single(dt) => ExpirationDate::DateTime(dt),
            _ => panic!("invalid feb fixture"),
        };

        let manager = ExpirationOrderBookManager::new("BTC");

        // Distinguish the two books by strike population.
        let jan_book = manager.get_or_create(jan);
        jan_book.get_or_create_strike(50000);

        let feb_book = manager.get_or_create(feb);
        feb_book.get_or_create_strike(50000);
        feb_book.get_or_create_strike(51000);

        // Both expirations survive — the second insert did not overwrite the first.
        assert_eq!(manager.len(), 2);
        assert!(manager.contains(&jan));
        assert!(manager.contains(&feb));

        // `get` returns the correct, independent book for each expiration.
        let jan_got = match manager.get(&jan) {
            Ok(book) => book,
            Err(err) => panic!("jan not found: {}", err),
        };
        let feb_got = match manager.get(&feb) {
            Ok(book) => book,
            Err(err) => panic!("feb not found: {}", err),
        };
        assert_eq!(jan_got.strike_count(), 1);
        assert_eq!(feb_got.strike_count(), 2);
        assert_eq!(*jan_got.expiration(), jan);
        assert_eq!(*feb_got.expiration(), feb);

        // Removing one leaves the other intact.
        assert!(manager.remove(&jan));
        assert_eq!(manager.len(), 1);
        assert!(!manager.contains(&jan));
        assert!(manager.contains(&feb));
    }

    #[test]
    fn test_expiration_order_book_expiration() {
        let exp = test_expiration();
        let book = ExpirationOrderBook::new("BTC", exp);
        assert_eq!(*book.expiration(), exp);
    }

    #[test]
    fn test_expiration_order_book_chain() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());
        drop(book.get_or_create_strike(50000));
        let chain = book.chain();
        assert_eq!(chain.strike_count(), 1);
    }

    #[test]
    fn test_expiration_order_book_get_strike() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());
        drop(book.get_or_create_strike(50000));

        assert!(book.get_strike(50000).is_ok());
        assert!(book.get_strike(99999).is_err());
    }

    #[test]
    fn test_expiration_order_book_atm_strike() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());

        drop(book.get_or_create_strike(45000));
        drop(book.get_or_create_strike(50000));
        drop(book.get_or_create_strike(55000));

        let atm1 = match book.atm_strike(48000) {
            Ok(s) => s,
            Err(err) => panic!("atm_strike failed: {}", err),
        };
        assert_eq!(atm1, 50000);
        let atm2 = match book.atm_strike(53000) {
            Ok(s) => s,
            Err(err) => panic!("atm_strike failed: {}", err),
        };
        assert_eq!(atm2, 55000);
    }

    #[test]
    fn test_expiration_order_book_atm_strike_empty() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());
        assert!(book.atm_strike(50000).is_err());
    }

    #[test]
    fn test_expiration_manager_get() {
        let manager = ExpirationOrderBookManager::new("BTC");
        let exp = test_expiration();

        drop(manager.get_or_create(exp));

        assert!(manager.get(&exp).is_ok());
        assert!(
            manager
                .get(&ExpirationDate::Days(pos_or_panic!(999.0)))
                .is_err()
        );
    }

    #[test]
    fn test_expiration_manager_contains() {
        let manager = ExpirationOrderBookManager::new("BTC");
        let exp = test_expiration();

        drop(manager.get_or_create(exp));

        assert!(manager.contains(&exp));
        assert!(!manager.contains(&ExpirationDate::Days(pos_or_panic!(999.0))));
    }

    #[test]
    fn test_expiration_manager_remove() {
        let manager = ExpirationOrderBookManager::new("BTC");
        let exp = test_expiration();

        drop(manager.get_or_create(exp));
        assert_eq!(manager.len(), 1);

        assert!(manager.remove(&exp));
        assert_eq!(manager.len(), 0);
        assert!(!manager.remove(&exp));
    }

    #[test]
    fn test_expiration_manager_total_order_count() {
        let manager = ExpirationOrderBookManager::new("BTC");

        let exp_book = manager.get_or_create(test_expiration());
        let strike = exp_book.get_or_create_strike(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(strike);
        drop(exp_book);

        assert_eq!(manager.total_order_count(), 1);
    }

    #[test]
    fn test_expiration_manager_total_strike_count() {
        let manager = ExpirationOrderBookManager::new("BTC");

        let exp_book = manager.get_or_create(test_expiration());
        exp_book.get_or_create_strike(50000);
        exp_book.get_or_create_strike(55000);
        drop(exp_book);

        assert_eq!(manager.total_strike_count(), 2);
    }

    #[test]
    fn test_expiration_manager_stats() {
        let manager = ExpirationOrderBookManager::new("BTC");

        let exp_book = manager.get_or_create(test_expiration());
        let strike = exp_book.get_or_create_strike(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(strike);
        drop(exp_book);

        let stats = manager.stats();
        assert_eq!(stats.underlying, "BTC");
        assert_eq!(stats.expiration_count, 1);
        assert_eq!(stats.total_strikes, 1);
        assert_eq!(stats.total_orders, 1);

        let display = format!("{}", stats);
        assert!(display.contains("BTC"));
    }

    #[test]
    fn test_expiration_set_validation() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());
        let config = ValidationConfig::new().with_tick_size(100);
        book.set_validation(config.clone());

        assert_eq!(book.validation_config(), Some(config));

        let strike = book.get_or_create_strike(50000);
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 10)
                .is_ok()
        );
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_err()
        );
    }

    #[test]
    fn test_expiration_manager_set_validation_propagates() {
        let manager = ExpirationOrderBookManager::new("BTC");
        let config = ValidationConfig::new().with_tick_size(100);
        manager.set_validation(config);

        let exp = manager.get_or_create(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 10)
                .is_ok()
        );
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_err()
        );
    }

    #[test]
    fn test_expiration_manager_existing_book_unaffected() {
        let manager = ExpirationOrderBookManager::new("BTC");

        let exp_before = manager.get_or_create(ExpirationDate::Days(pos_or_panic!(30.0)));

        manager.set_validation(ValidationConfig::new().with_tick_size(100));

        // Existing expiration is NOT affected
        let strike = exp_before.get_or_create_strike(50000);
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_ok()
        );

        // New expiration IS affected
        let exp_after = manager.get_or_create(ExpirationDate::Days(pos_or_panic!(60.0)));
        let strike2 = exp_after.get_or_create_strike(50000);
        assert!(
            strike2
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_err()
        );
    }

    // ── Order Lifecycle Tests ──────────────────────────────────────────────

    #[test]
    fn test_expiration_find_order() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());
        let order_id = OrderId::new();

        let strike = book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(order_id, Side::Buy, 100, 10)
            .expect("add order");
        drop(strike);

        let result = book.find_order(order_id);
        assert!(result.is_some());
    }

    #[test]
    fn test_expiration_find_order_not_found() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());
        let result = book.find_order(OrderId::new());
        assert!(result.is_none());
    }

    #[test]
    fn test_expiration_total_active_orders() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());

        let strike = book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add call");
        strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 80, 5)
            .expect("add put");
        drop(strike);

        assert_eq!(book.total_active_orders(), 2);
    }

    #[test]
    fn test_expiration_orders_by_user() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        let strike = book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
            .expect("add a1");
        strike
            .put()
            .add_limit_order_with_user(OrderId::new(), Side::Sell, 80, 5, user_a)
            .expect("add a2");
        strike
            .call()
            .add_limit_order_with_user(OrderId::new(), Side::Sell, 110, 5, user_b)
            .expect("add b1");
        drop(strike);

        let a_orders = book.orders_by_user(user_a);
        assert_eq!(a_orders.len(), 2);

        let b_orders = book.orders_by_user(user_b);
        assert_eq!(b_orders.len(), 1);
    }

    #[test]
    fn test_expiration_terminal_order_summary() {
        let book = ExpirationOrderBook::new("BTC", test_expiration());

        let strike = book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        drop(strike);

        let summary = book.terminal_order_summary();
        assert_eq!(summary.filled, 2);
        assert_eq!(summary.total(), 2);
    }

    #[test]
    fn test_expiration_purge_terminal_states() {
        use std::thread;
        use std::time::Duration;

        let book = ExpirationOrderBook::new("BTC", test_expiration());

        let strike = book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        drop(strike);

        thread::sleep(Duration::from_millis(10));
        let purged = book.purge_terminal_states(Duration::from_millis(1));
        assert_eq!(purged, 2);
    }

    #[test]
    fn test_expiration_set_trade_id_namespace_delegates() {
        let expiration = ExpirationOrderBook::new("BTC", test_expiration());
        assert!(expiration.trade_id_namespace().is_none());

        let root = Uuid::from_u128(0x0BAD_C0DE);
        expiration.set_trade_id_namespace(root);
        assert_eq!(expiration.trade_id_namespace(), Some(root));

        expiration.clear_trade_id_namespace();
        assert!(expiration.trade_id_namespace().is_none());
    }
}
