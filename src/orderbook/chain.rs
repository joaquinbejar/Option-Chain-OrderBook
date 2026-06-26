//! Option chain order book module.
//!
//! This module provides the [`OptionChainOrderBook`] and [`OptionChainOrderBookManager`]
//! for managing all strikes within a single expiration.

use super::contract_specs::{ContractSpecs, SharedContractSpecs};
use super::expiration_key::ExpirationKey;
use super::fees::SharedFeeSchedule;
use super::instrument_registry::InstrumentRegistry;
use super::stp::SharedSTPMode;
use super::strike::{StrikeMassCancelResult, StrikeOrderBook, StrikeOrderBookManager};
use super::symbol_index::SymbolIndex;
use super::validation::{SharedValidationConfig, ValidationConfig};
use crate::error::{Error, Result};
use crossbeam_skiplist::SkipMap;
use optionstratlib::ExpirationDate;
use orderbook_rs::{FeeSchedule, OrderId, OrderStatus, STPMode, Side};
use pricelevel::Hash32;
use std::sync::Arc;
use std::time::Duration;

use super::book::TerminalOrderSummary;

/// Option chain order book for a single expiration.
///
/// Contains all strikes for a specific expiration date.
///
/// ## Architecture
///
/// ```text
/// OptionChainOrderBook (per expiration)
///   └── StrikeOrderBookManager
///         └── StrikeOrderBook (per strike)
///               ├── OptionOrderBook (call)
///               └── OptionOrderBook (put)
/// ```
pub struct OptionChainOrderBook {
    /// The underlying asset symbol.
    underlying: String,
    /// The expiration date.
    expiration: ExpirationDate,
    /// Strike order book manager.
    strikes: Arc<StrikeOrderBookManager>,
    /// Unique identifier for this option chain order book.
    id: OrderId,
    /// Instrument registry propagated to strike managers.
    registry: Option<Arc<InstrumentRegistry>>,
    /// Symbol index for O(1) lookup by symbol string.
    /// Stored for future use in hierarchy traversal.
    #[allow(dead_code)]
    symbol_index: Option<Arc<SymbolIndex>>,
    /// Stored NATS publisher handles for lifecycle management.
    /// Each entry is a `(call_handles, put_handles)` pair per strike.
    #[cfg(feature = "nats")]
    nats_handles: std::sync::Mutex<
        Vec<(
            super::book::NatsPublisherHandles,
            super::book::NatsPublisherHandles,
        )>,
    >,
}

impl OptionChainOrderBook {
    /// Creates a new option chain order book.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol (e.g., "BTC")
    /// * `expiration` - The expiration date
    #[must_use]
    pub fn new(underlying: impl Into<String>, expiration: ExpirationDate) -> Self {
        let underlying = underlying.into();

        Self {
            strikes: Arc::new(StrikeOrderBookManager::new(&underlying, expiration)),
            underlying,
            expiration,
            id: OrderId::new(),
            registry: None,
            symbol_index: None,
            #[cfg(feature = "nats")]
            nats_handles: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Creates a new option chain order book with an instrument registry.
    ///
    /// The registry is propagated to the internal [`StrikeOrderBookManager`]
    /// so that newly created strikes get unique instrument IDs.
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
            strikes: Arc::new(StrikeOrderBookManager::new_with_registry(
                &underlying,
                expiration,
                Arc::clone(&registry),
            )),
            underlying,
            expiration,
            id: OrderId::new(),
            registry: Some(registry),
            symbol_index: None,
            #[cfg(feature = "nats")]
            nats_handles: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Creates a new option chain order book with both instrument registry and symbol index.
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
            strikes: Arc::new(StrikeOrderBookManager::new_with_registry_and_index(
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
            #[cfg(feature = "nats")]
            nats_handles: std::sync::Mutex::new(Vec::new()),
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

    /// Returns the unique identifier for this option chain order book.
    #[must_use]
    pub const fn id(&self) -> OrderId {
        self.id
    }

    /// Returns a reference to the strike manager.
    #[must_use]
    pub fn strikes(&self) -> &StrikeOrderBookManager {
        &self.strikes
    }

    /// Returns a reference to the instrument registry, if any.
    #[must_use]
    pub fn registry(&self) -> Option<&Arc<InstrumentRegistry>> {
        self.registry.as_ref()
    }

    /// Returns an Arc reference to the strike manager.
    #[must_use]
    pub fn strikes_arc(&self) -> Arc<StrikeOrderBookManager> {
        Arc::clone(&self.strikes)
    }

    /// Sets the contract specifications for this chain.
    ///
    /// Also propagates the specs to the strike manager for newly created strikes.
    pub fn set_specs(&self, specs: ContractSpecs) {
        self.strikes.set_specs(specs);
    }

    /// Returns the current contract specifications, if any.
    ///
    /// Delegates to the strike manager to maintain a single source of truth.
    #[must_use]
    pub fn specs(&self) -> Option<ContractSpecs> {
        self.strikes.specs()
    }

    /// Sets the validation config for all future strikes created within this chain.
    ///
    /// Delegates to the underlying [`StrikeOrderBookManager::set_validation`].
    /// Existing strikes are not affected.
    pub fn set_validation(&self, config: ValidationConfig) {
        self.strikes.set_validation(config);
    }

    /// Returns the current validation config, if any.
    #[must_use]
    pub fn validation_config(&self) -> Option<ValidationConfig> {
        self.strikes.validation_config()
    }

    /// Sets the STP mode for all future option books created within this chain.
    ///
    /// Delegates to the underlying [`StrikeOrderBookManager::set_stp_mode`].
    /// Existing books are not affected.
    #[inline]
    pub fn set_stp_mode(&self, mode: STPMode) {
        self.strikes.set_stp_mode(mode);
    }

    /// Returns the current STP mode.
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.strikes.stp_mode()
    }

    /// Sets the fee schedule for all future option books created within this chain.
    ///
    /// Delegates to the underlying [`StrikeOrderBookManager::set_fee_schedule`].
    /// Existing books are not affected.
    #[inline]
    pub fn set_fee_schedule(&self, schedule: FeeSchedule) {
        self.strikes.set_fee_schedule(schedule);
    }

    /// Clears the fee schedule so future option books have no fees configured.
    ///
    /// Delegates to the underlying [`StrikeOrderBookManager::clear_fee_schedule`].
    /// Existing books are not affected.
    #[inline]
    pub fn clear_fee_schedule(&self) {
        self.strikes.clear_fee_schedule();
    }

    /// Returns the current fee schedule, or `None` if no fees are configured.
    #[must_use]
    #[inline]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.strikes.fee_schedule()
    }

    /// Gets or creates a strike order book, returning an Arc reference.
    pub fn get_or_create_strike(&self, strike: u64) -> Arc<StrikeOrderBook> {
        self.strikes.get_or_create(strike)
    }

    /// Gets a strike order book.
    ///
    /// # Errors
    ///
    /// Returns `Error::StrikeNotFound` if the strike does not exist.
    pub fn get_strike(&self, strike: u64) -> Result<Arc<StrikeOrderBook>> {
        self.strikes.get(strike)
    }

    /// Returns the number of strikes.
    #[must_use]
    pub fn strike_count(&self) -> usize {
        self.strikes.len()
    }

    /// Returns true if there are no strikes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strikes.is_empty()
    }

    /// Returns all strike prices (sorted).
    pub fn strike_prices(&self) -> Vec<u64> {
        self.strikes.strike_prices()
    }

    /// Returns the total order count across all strikes.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.strikes.total_order_count()
    }

    /// Cancels all resting orders across every strike in the chain.
    ///
    /// # Description
    ///
    /// Cancels every resting order across all strikes and returns the aggregated
    /// cancellation details.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// A [`ChainMassCancelResult`] containing per-strike results plus aggregated
    /// counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::OptionChainOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    ///
    /// let chain = OptionChainOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)));
    /// let result = match chain.cancel_all() {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_all(&self) -> Result<ChainMassCancelResult> {
        let mut per_child = Vec::new();

        for entry in self.strikes.iter() {
            let strike_key = entry.key().to_string();
            let result = entry.value().cancel_all()?;
            per_child.push((strike_key, result));
        }

        Ok(ChainMassCancelResult { per_child })
    }

    /// Cancels all resting orders on a specific side across every strike.
    ///
    /// # Description
    ///
    /// Cancels every resting order on the provided side across all strikes and
    /// returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// * `side` - Side to cancel ([`Side::Buy`] or [`Side::Sell`]).
    ///
    /// # Returns
    ///
    /// A [`ChainMassCancelResult`] containing per-strike results plus aggregated
    /// counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::OptionChainOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    /// use orderbook_rs::Side;
    ///
    /// let chain = OptionChainOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)));
    /// let result = match chain.cancel_by_side(Side::Buy) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_side(&self, side: Side) -> Result<ChainMassCancelResult> {
        let mut per_child = Vec::new();

        for entry in self.strikes.iter() {
            let strike_key = entry.key().to_string();
            let result = entry.value().cancel_by_side(side)?;
            per_child.push((strike_key, result));
        }

        Ok(ChainMassCancelResult { per_child })
    }

    /// Cancels all resting orders for a specific user across every strike.
    ///
    /// # Description
    ///
    /// Cancels every resting order attributed to the provided user identifier
    /// across all strikes and returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// * `user_id` - User identifier to cancel (32-byte hash).
    ///
    /// # Returns
    ///
    /// A [`ChainMassCancelResult`] containing per-strike results plus aggregated
    /// counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::OptionChainOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    /// use pricelevel::Hash32;
    ///
    /// let chain = OptionChainOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)));
    /// let user = Hash32::from([1u8; 32]);
    /// let result = match chain.cancel_by_user(user) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_user(&self, user_id: Hash32) -> Result<ChainMassCancelResult> {
        let mut per_child = Vec::new();

        for entry in self.strikes.iter() {
            let strike_key = entry.key().to_string();
            let result = entry.value().cancel_by_user(user_id)?;
            per_child.push((strike_key, result));
        }

        Ok(ChainMassCancelResult { per_child })
    }

    // ── Order Lifecycle Queries ────────────────────────────────────────────

    /// Finds an order anywhere in this chain's strikes.
    ///
    /// # Description
    ///
    /// Searches all strikes for the specified order. Returns the option
    /// symbol and current status if found.
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
        for entry in self.strikes.iter() {
            if let Some(result) = entry.value().find_order(order_id) {
                return Some(result);
            }
        }
        None
    }

    /// Returns the total number of active orders across all strikes.
    ///
    /// # Description
    ///
    /// Sums the active order counts from all strikes in the chain.
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
        self.strikes
            .iter()
            .map(|entry| entry.value().total_active_orders())
            .sum()
    }

    /// Removes terminal-state entries older than the specified duration.
    ///
    /// # Description
    ///
    /// Delegates to all strikes and returns the total purged.
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
        self.strikes
            .iter()
            .map(|entry| entry.value().purge_terminal_states(older_than))
            .sum()
    }

    /// Returns all currently active orders for a specific user.
    ///
    /// # Description
    ///
    /// Searches all strikes for resting orders belonging to the specified
    /// user. Returns tuples of (symbol, order_id, status).
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
        self.strikes
            .iter()
            .flat_map(|entry| entry.value().orders_by_user(user_id))
            .collect()
    }

    /// Returns a summary of terminal order transitions.
    ///
    /// # Description
    ///
    /// Aggregates the terminal order summaries from all strikes.
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
        self.strikes
            .iter()
            .map(|entry| entry.value().terminal_order_summary())
            .sum()
    }

    /// Returns the ATM strike closest to the given spot price.
    ///
    /// # Errors
    ///
    /// Returns `Error::NoDataAvailable` if there are no strikes.
    pub fn atm_strike(&self, spot: u64) -> Result<u64> {
        self.strikes.atm_strike(spot)
    }

    /// Returns statistics about this option chain.
    #[must_use]
    pub fn stats(&self) -> OptionChainStats {
        OptionChainStats {
            expiration: self.expiration,
            strike_count: self.strike_count(),
            total_orders: self.total_order_count(),
        }
    }

    // ── NATS Integration ─────────────────────────────────────────────────

    /// Connects NATS publishers to all strikes in this chain.
    ///
    /// # Arguments
    ///
    /// * `config` - NATS configuration with JetStream context and subject prefix
    ///
    /// # Returns
    ///
    /// The number of option books (call + put) successfully connected.
    ///
    /// # Errors
    ///
    /// Returns the first error encountered while connecting strikes.
    #[cfg(feature = "nats")]
    pub fn connect_nats(
        &self,
        config: &super::nats::OptionChainNatsConfig,
    ) -> crate::Result<usize> {
        let mut connected = 0usize;
        let mut new_handles = Vec::new();
        for entry in self.strikes.iter() {
            let (call_handles, put_handles) = entry.value().connect_nats(config)?;
            new_handles.push((call_handles, put_handles));
            connected = connected.saturating_add(2); // call + put
        }

        // Store handles for lifecycle management (shutdown, metrics)
        if let Ok(mut guard) = self.nats_handles.lock() {
            guard.extend(new_handles);
        }

        Ok(connected)
    }

    /// Returns the number of stored NATS publisher handle pairs.
    ///
    /// Each pair represents the call and put publishers for one strike.
    #[cfg(feature = "nats")]
    #[must_use]
    pub fn nats_handle_count(&self) -> usize {
        self.nats_handles.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// Statistics about an option chain.
#[derive(Debug, Clone)]
pub struct OptionChainStats {
    /// The expiration date.
    pub expiration: ExpirationDate,
    /// Number of strikes.
    pub strike_count: usize,
    /// Total number of orders.
    pub total_orders: usize,
}

impl std::fmt::Display for OptionChainStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} strikes, {} orders",
            self.expiration, self.strike_count, self.total_orders
        )
    }
}

/// Chain-level mass cancel summary.
///
/// # Description
///
/// Aggregates per-strike mass cancel results for an option chain.
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
/// use option_chain_orderbook::orderbook::ChainMassCancelResult;
///
/// let result = ChainMassCancelResult { per_child: Vec::new() };
/// assert_eq!(result.total_cancelled(), 0);
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct ChainMassCancelResult {
    /// Per-strike cancellation results keyed by strike price.
    pub per_child: Vec<(String, StrikeMassCancelResult)>,
}

impl ChainMassCancelResult {
    /// Returns the number of strike books with cancelled orders.
    ///
    /// # Description
    ///
    /// Counts how many strike books recorded at least one cancelled order.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Number of strike books affected (books).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::ChainMassCancelResult;
    ///
    /// let result = ChainMassCancelResult { per_child: Vec::new() };
    /// assert_eq!(result.books_affected(), 0);
    /// ```
    #[must_use]
    pub fn books_affected(&self) -> usize {
        self.per_child
            .iter()
            .filter(|(_, result)| result.total_cancelled() > 0)
            .count()
    }

    /// Returns the total number of cancelled orders across the chain.
    ///
    /// # Description
    ///
    /// Sums cancelled orders across every strike in the chain.
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
    /// use option_chain_orderbook::orderbook::ChainMassCancelResult;
    ///
    /// let result = ChainMassCancelResult { per_child: Vec::new() };
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

/// Manages option chain order books for multiple expirations.
///
/// Uses `SkipMap` for thread-safe concurrent access.
pub struct OptionChainOrderBookManager {
    /// Option chains indexed by a deterministic [`ExpirationKey`].
    ///
    /// Keyed on [`ExpirationKey`] rather than [`ExpirationDate`] directly
    /// because the latter's `Ord`/`Eq` is wall-clock-relative and collides
    /// distinct expirations (see [`ExpirationKey`] docs). Each stored
    /// [`OptionChainOrderBook`] retains its original [`ExpirationDate`], so the
    /// public API still hands back `ExpirationDate` values.
    chains: SkipMap<ExpirationKey, Arc<OptionChainOrderBook>>,
    /// The underlying asset symbol.
    underlying: String,
    /// Validation config applied to newly created chains.
    validation_config: SharedValidationConfig,
    /// Contract specs propagated to newly created chains.
    contract_specs: SharedContractSpecs,
    /// Instrument registry propagated to newly created chains.
    registry: Option<Arc<InstrumentRegistry>>,
    /// STP mode propagated to newly created chains.
    stp_mode: SharedSTPMode,
    /// Fee schedule propagated to newly created chains.
    fee_schedule: SharedFeeSchedule,
}

impl OptionChainOrderBookManager {
    /// Creates a new option chain manager.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    #[must_use]
    pub fn new(underlying: impl Into<String>) -> Self {
        Self {
            chains: SkipMap::new(),
            underlying: underlying.into(),
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: None,
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Creates a new option chain manager with an instrument registry.
    ///
    /// The registry is propagated to newly created chains and their
    /// strike managers.
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
            chains: SkipMap::new(),
            underlying: underlying.into(),
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: Some(registry),
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Sets the contract specs for all future chains created by this manager.
    ///
    /// Existing chains are not affected. Only newly created chains
    /// via [`get_or_create`](Self::get_or_create) will have these specs propagated.
    pub fn set_specs(&self, specs: ContractSpecs) {
        self.contract_specs.set(specs);
    }

    /// Returns the current contract specs, if any.
    #[must_use]
    pub fn specs(&self) -> Option<ContractSpecs> {
        self.contract_specs.get()
    }

    /// Sets the validation config for all future chains created by this manager.
    ///
    /// Existing chains are not affected. Only newly created chains
    /// via [`get_or_create`](Self::get_or_create) will have this config applied.
    pub fn set_validation(&self, config: ValidationConfig) {
        self.validation_config.set(config);
    }

    /// Returns the current validation config, if any.
    #[must_use]
    pub fn validation_config(&self) -> Option<ValidationConfig> {
        self.validation_config.get()
    }

    /// Sets the STP mode for all future chains created by this manager.
    ///
    /// Existing chains are not affected. Only newly created chains
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

    /// Sets the fee schedule for all future chains created by this manager.
    ///
    /// Existing chains are not affected. Only newly created chains
    /// via [`get_or_create`](Self::get_or_create) will have this schedule propagated.
    #[inline]
    pub fn set_fee_schedule(&self, schedule: FeeSchedule) {
        self.fee_schedule.set(Some(schedule));
    }

    /// Clears the fee schedule so future chains have no fees configured.
    ///
    /// Existing chains are not affected. Only newly created chains
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

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the number of option chains.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chains.len()
    }

    /// Returns true if there are no option chains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chains.is_empty()
    }

    /// Gets or creates an option chain for the given expiration.
    ///
    /// If a validation config has been set via [`set_validation`](Self::set_validation),
    /// newly created chains will have that config propagated to their strike manager.
    ///
    /// # Concurrency
    ///
    /// This is atomic and idempotent. The fresh chain is fully configured before
    /// it is published, and publishing uses [`SkipMap::get_or_insert`], which
    /// inserts only when the key is absent and never evicts an existing entry.
    /// The first inserter wins and is never orphaned; concurrent losers receive
    /// the winning handle and drop their fresh chain. Building and configuring a
    /// chain has no global side effects (instrument IDs and symbol-index entries
    /// are only allocated lazily at strike creation), so a dropped loser leaks
    /// nothing.
    pub fn get_or_create(&self, expiration: ExpirationDate) -> Arc<OptionChainOrderBook> {
        let key = ExpirationKey::from(&expiration);
        if let Some(entry) = self.chains.get(&key) {
            return Arc::clone(entry.value());
        }
        // Build and fully configure the fresh chain BEFORE publishing it, so the
        // winning chain is configured-before-visible. Configuration only mutates
        // the fresh object (no global side effects), so a race loser's chain is
        // dropped harmlessly.
        let fresh = if let Some(ref reg) = self.registry {
            Arc::new(OptionChainOrderBook::new_with_registry(
                &self.underlying,
                expiration,
                Arc::clone(reg),
            ))
        } else {
            Arc::new(OptionChainOrderBook::new(&self.underlying, expiration))
        };
        if let Some(ref config) = self.validation_config.get() {
            fresh.set_validation(config.clone());
        }
        if let Some(ref specs) = self.contract_specs.get() {
            fresh.set_specs(specs.clone());
        }
        let stp = self.stp_mode.get();
        if stp != STPMode::None {
            fresh.set_stp_mode(stp);
        }
        if let Some(schedule) = self.fee_schedule.get() {
            fresh.set_fee_schedule(schedule);
        }
        // Atomic, idempotent publish: first inserter wins and is never evicted.
        let entry = self.chains.get_or_insert(key, fresh);
        Arc::clone(entry.value())
    }

    /// Gets an option chain by expiration.
    ///
    /// # Errors
    ///
    /// Returns `Error::ExpirationNotFound` if the expiration does not exist.
    pub fn get(&self, expiration: &ExpirationDate) -> Result<Arc<OptionChainOrderBook>> {
        self.chains
            .get(&ExpirationKey::from(expiration))
            .map(|e| Arc::clone(e.value()))
            .ok_or_else(|| Error::expiration_not_found(expiration.to_string()))
    }

    /// Returns true if an option chain exists for the expiration.
    #[must_use]
    pub fn contains(&self, expiration: &ExpirationDate) -> bool {
        self.chains.contains_key(&ExpirationKey::from(expiration))
    }

    /// Returns an iterator over all chains, ordered by expiration.
    ///
    /// Yields `(ExpirationDate, Arc<OptionChainOrderBook>)` tuples in ascending
    /// order. Ordering is driven by an internal deterministic expiration key
    /// (clock-independent and collision-free, unlike `ExpirationDate`'s own
    /// `Ord`), so the traversal order is stable and replay-safe. The
    /// `ExpirationDate` is the original value stored in each chain; the internal
    /// key is never exposed.
    pub fn iter(&self) -> impl Iterator<Item = (ExpirationDate, Arc<OptionChainOrderBook>)> + '_ {
        self.chains
            .iter()
            .map(|e| (*e.value().expiration(), Arc::clone(e.value())))
    }

    /// Removes an option chain.
    pub fn remove(&self, expiration: &ExpirationDate) -> bool {
        self.chains
            .remove(&ExpirationKey::from(expiration))
            .is_some()
    }

    /// Returns the total order count across all chains.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.chains
            .iter()
            .map(|e| e.value().total_order_count())
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
    fn test_option_chain_creation() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        assert_eq!(chain.underlying(), "BTC");
        assert!(chain.is_empty());
    }

    #[test]
    fn test_option_chain_strikes() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        drop(chain.get_or_create_strike(50000));
        drop(chain.get_or_create_strike(55000));
        drop(chain.get_or_create_strike(45000));

        assert_eq!(chain.strike_count(), 3);
        assert_eq!(chain.strike_prices(), vec![45000, 50000, 55000]);
    }

    #[test]
    fn test_option_chain_orders() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        {
            let strike = chain.get_or_create_strike(50000);
            if let Err(err) = strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            {
                panic!("add order failed: {}", err);
            }
            if let Err(err) = strike
                .put()
                .add_limit_order(OrderId::new(), Side::Sell, 50, 5)
            {
                panic!("add order failed: {}", err);
            }
        }

        assert_eq!(chain.total_order_count(), 2);
    }

    #[test]
    fn test_option_chain_stats() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        {
            let strike = chain.get_or_create_strike(50000);
            if let Err(err) = strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            {
                panic!("add order failed: {}", err);
            }
            if let Err(err) = strike
                .call()
                .add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            {
                panic!("add order failed: {}", err);
            }
            if let Err(err) = strike
                .put()
                .add_limit_order(OrderId::new(), Side::Buy, 50, 10)
            {
                panic!("add order failed: {}", err);
            }
            if let Err(err) = strike
                .put()
                .add_limit_order(OrderId::new(), Side::Sell, 51, 5)
            {
                panic!("add order failed: {}", err);
            }
        }

        let stats = chain.stats();
        assert_eq!(stats.strike_count, 1);
        assert_eq!(stats.total_orders, 4);
    }

    #[test]
    fn test_option_chain_manager() {
        let manager = OptionChainOrderBookManager::new("BTC");

        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(30.0))));
        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(60.0))));
        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(90.0))));

        assert_eq!(manager.len(), 3);
    }

    #[test]
    fn test_option_chain_manager_get_or_create_returns_same_handle() {
        let manager = OptionChainOrderBookManager::new("BTC");
        let exp = test_expiration();

        let first = manager.get_or_create(exp);
        let second = manager.get_or_create(exp);

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_option_chain_manager_get_or_create_concurrent_returns_same_handle() {
        use std::sync::Barrier;
        use std::thread;

        // Race N threads to create the SAME chain via a barrier for lockstep.
        // With `get_or_insert` the first inserter wins and is never evicted, so
        // every thread must observe one identical handle and the map must hold
        // exactly one entry (no orphan, no split-brain).
        const N: usize = 16;
        let manager = Arc::new(OptionChainOrderBookManager::new("BTC"));
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

        let chains: Vec<Arc<OptionChainOrderBook>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        let first = chains.first().expect("no chains returned");
        for chain in &chains {
            assert!(Arc::ptr_eq(first, chain));
        }
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_option_chain_manager_same_day_of_month_distinct_months_both_retrievable() {
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

        let manager = OptionChainOrderBookManager::new("BTC");

        let jan_chain = manager.get_or_create(jan);
        jan_chain.get_or_create_strike(50000);

        let feb_chain = manager.get_or_create(feb);
        feb_chain.get_or_create_strike(50000);
        feb_chain.get_or_create_strike(51000);

        assert_eq!(manager.len(), 2);
        assert!(manager.contains(&jan));
        assert!(manager.contains(&feb));

        let jan_got = match manager.get(&jan) {
            Ok(chain) => chain,
            Err(err) => panic!("jan not found: {}", err),
        };
        let feb_got = match manager.get(&feb) {
            Ok(chain) => chain,
            Err(err) => panic!("feb not found: {}", err),
        };
        assert_eq!(jan_got.strike_count(), 1);
        assert_eq!(feb_got.strike_count(), 2);
        assert_eq!(*jan_got.expiration(), jan);
        assert_eq!(*feb_got.expiration(), feb);

        assert!(manager.remove(&jan));
        assert_eq!(manager.len(), 1);
        assert!(!manager.contains(&jan));
        assert!(manager.contains(&feb));
    }

    #[test]
    fn test_option_chain_expiration() {
        let exp = test_expiration();
        let chain = OptionChainOrderBook::new("BTC", exp);
        assert_eq!(*chain.expiration(), exp);
    }

    #[test]
    fn test_option_chain_strikes_ref() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        drop(chain.get_or_create_strike(50000));
        let strikes = chain.strikes();
        assert_eq!(strikes.len(), 1);
    }

    #[test]
    fn test_option_chain_get_strike() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        drop(chain.get_or_create_strike(50000));

        assert!(chain.get_strike(50000).is_ok());
        assert!(chain.get_strike(99999).is_err());
    }

    #[test]
    fn test_option_chain_atm_strike() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        drop(chain.get_or_create_strike(45000));
        drop(chain.get_or_create_strike(50000));
        drop(chain.get_or_create_strike(55000));

        let atm1 = match chain.atm_strike(48000) {
            Ok(s) => s,
            Err(err) => panic!("atm_strike failed: {}", err),
        };
        assert_eq!(atm1, 50000);
        let atm2 = match chain.atm_strike(53000) {
            Ok(s) => s,
            Err(err) => panic!("atm_strike failed: {}", err),
        };
        assert_eq!(atm2, 55000);
    }

    #[test]
    fn test_option_chain_atm_strike_empty() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        assert!(chain.atm_strike(50000).is_err());
    }

    #[test]
    fn test_option_chain_stats_display() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        drop(chain.get_or_create_strike(50000));

        let stats = chain.stats();
        let display = format!("{}", stats);
        assert!(display.contains("1 strikes"));
    }

    #[test]
    fn test_option_chain_manager_underlying() {
        let manager = OptionChainOrderBookManager::new("BTC");
        assert_eq!(manager.underlying(), "BTC");
    }

    #[test]
    fn test_option_chain_manager_is_empty() {
        let manager = OptionChainOrderBookManager::new("BTC");
        assert!(manager.is_empty());

        drop(manager.get_or_create(test_expiration()));
        assert!(!manager.is_empty());
    }

    #[test]
    fn test_option_chain_manager_get() {
        let manager = OptionChainOrderBookManager::new("BTC");
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
    fn test_option_chain_manager_contains() {
        let manager = OptionChainOrderBookManager::new("BTC");
        let exp = test_expiration();

        drop(manager.get_or_create(exp));

        assert!(manager.contains(&exp));
        assert!(!manager.contains(&ExpirationDate::Days(pos_or_panic!(999.0))));
    }

    #[test]
    fn test_option_chain_manager_remove() {
        let manager = OptionChainOrderBookManager::new("BTC");
        let exp = test_expiration();

        drop(manager.get_or_create(exp));
        assert_eq!(manager.len(), 1);

        assert!(manager.remove(&exp));
        assert_eq!(manager.len(), 0);
        assert!(!manager.remove(&exp));
    }

    #[test]
    fn test_option_chain_manager_total_order_count() {
        let manager = OptionChainOrderBookManager::new("BTC");

        let chain = manager.get_or_create(test_expiration());
        let strike = chain.get_or_create_strike(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(strike);
        drop(chain);

        assert_eq!(manager.total_order_count(), 1);
    }

    #[test]
    fn test_option_chain_set_validation() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = ValidationConfig::new().with_tick_size(100);
        chain.set_validation(config.clone());

        assert_eq!(chain.validation_config(), Some(config));

        // New strike inherits validation
        let strike = chain.get_or_create_strike(50000);
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
    fn test_option_chain_no_validation_by_default() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        assert!(chain.validation_config().is_none());
    }

    #[test]
    fn test_option_chain_manager_set_validation_propagates() {
        let manager = OptionChainOrderBookManager::new("BTC");
        let config = ValidationConfig::new().with_tick_size(100);
        manager.set_validation(config);

        // New chain should inherit validation
        let chain = manager.get_or_create(test_expiration());
        let strike = chain.get_or_create_strike(50000);
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
    fn test_chain_cancel_all() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        let s1 = chain.get_or_create_strike(50000);
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

        let s2 = chain.get_or_create_strike(52000);
        if let Err(err) = s2.call().add_limit_order(OrderId::new(), Side::Buy, 80, 10) {
            panic!("add order failed: {}", err);
        }
        drop(s2);

        assert_eq!(chain.total_order_count(), 3);

        let result = match chain.cancel_all() {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 3);
        assert_eq!(result.books_affected(), 2);
        assert_eq!(chain.total_order_count(), 0);
    }

    #[test]
    fn test_chain_cancel_by_side() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        let s1 = chain.get_or_create_strike(50000);
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

        let s2 = chain.get_or_create_strike(52000);
        if let Err(err) = s2.put().add_limit_order(OrderId::new(), Side::Buy, 50, 10) {
            panic!("add order failed: {}", err);
        }
        drop(s2);

        assert_eq!(chain.total_order_count(), 3);

        let result = match chain.cancel_by_side(Side::Buy) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 2);
        assert_eq!(chain.total_order_count(), 1);
    }

    #[test]
    fn test_chain_cancel_by_user() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        let s1 = chain.get_or_create_strike(50000);
        if let Err(err) =
            s1.call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
        {
            panic!("add order failed: {}", err);
        }
        drop(s1);

        let s2 = chain.get_or_create_strike(52000);
        if let Err(err) =
            s2.put()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 60, 5, user_a)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) =
            s2.call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 80, 10, user_b)
        {
            panic!("add order failed: {}", err);
        }
        drop(s2);

        assert_eq!(chain.total_order_count(), 3);

        let result = match chain.cancel_by_user(user_a) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 2);
        assert_eq!(chain.total_order_count(), 1);
    }

    #[test]
    fn test_option_chain_manager_existing_chain_unaffected() {
        let manager = OptionChainOrderBookManager::new("BTC");

        // Create chain before setting validation
        let chain_before = manager.get_or_create(ExpirationDate::Days(pos_or_panic!(30.0)));

        // Set validation after
        manager.set_validation(ValidationConfig::new().with_tick_size(100));

        // Existing chain's new strikes are NOT affected
        let strike = chain_before.get_or_create_strike(50000);
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_ok()
        );

        // New chain IS affected
        let chain_after = manager.get_or_create(ExpirationDate::Days(pos_or_panic!(60.0)));
        let strike2 = chain_after.get_or_create_strike(50000);
        assert!(
            strike2
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_err()
        );
    }

    // ── Order Lifecycle Tests ──────────────────────────────────────────────

    #[test]
    fn test_chain_find_order_across_strikes() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let order_id = OrderId::new();

        let s1 = chain.get_or_create_strike(50000);
        s1.call()
            .add_limit_order(order_id, Side::Buy, 100, 10)
            .expect("add order");
        drop(s1);

        let result = chain.find_order(order_id);
        assert!(result.is_some());
    }

    #[test]
    fn test_chain_find_order_not_found() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let result = chain.find_order(OrderId::new());
        assert!(result.is_none());
    }

    #[test]
    fn test_chain_total_active_orders() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        let s1 = chain.get_or_create_strike(50000);
        s1.call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add s1");
        drop(s1);

        let s2 = chain.get_or_create_strike(52000);
        s2.put()
            .add_limit_order(OrderId::new(), Side::Sell, 80, 5)
            .expect("add s2");
        drop(s2);

        assert_eq!(chain.total_active_orders(), 2);
    }

    #[test]
    fn test_chain_orders_by_user() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        let s1 = chain.get_or_create_strike(50000);
        s1.call()
            .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
            .expect("add a1");
        drop(s1);

        let s2 = chain.get_or_create_strike(52000);
        s2.put()
            .add_limit_order_with_user(OrderId::new(), Side::Sell, 80, 5, user_a)
            .expect("add a2");
        s2.call()
            .add_limit_order_with_user(OrderId::new(), Side::Buy, 90, 5, user_b)
            .expect("add b1");
        drop(s2);

        let a_orders = chain.orders_by_user(user_a);
        assert_eq!(a_orders.len(), 2);

        let b_orders = chain.orders_by_user(user_b);
        assert_eq!(b_orders.len(), 1);
    }

    #[test]
    fn test_chain_terminal_order_summary() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        let s1 = chain.get_or_create_strike(50000);
        s1.call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        s1.call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        drop(s1);

        let summary = chain.terminal_order_summary();
        assert_eq!(summary.filled, 2);
        assert_eq!(summary.total(), 2);
    }

    #[cfg(feature = "nats")]
    #[test]
    fn test_nats_handle_count_initially_zero() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        assert_eq!(chain.nats_handle_count(), 0);
    }

    #[test]
    fn test_chain_purge_terminal_states() {
        use std::thread;
        use std::time::Duration;

        let chain = OptionChainOrderBook::new("BTC", test_expiration());

        let s1 = chain.get_or_create_strike(50000);
        s1.call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        s1.call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        drop(s1);

        thread::sleep(Duration::from_millis(10));
        let purged = chain.purge_terminal_states(Duration::from_millis(1));
        assert_eq!(purged, 2);
    }

    // ── OptionChainOrderBook accessor coverage ───────────────────────────

    #[test]
    fn test_chain_id_unique() {
        let a = OptionChainOrderBook::new("BTC", test_expiration());
        let b = OptionChainOrderBook::new("BTC", test_expiration());
        // Each chain should get a distinct OrderId
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn test_chain_registry_none_by_default() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        assert!(chain.registry().is_none());
    }

    #[test]
    fn test_chain_strikes_arc() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        drop(chain.get_or_create_strike(50000));

        let arc = chain.strikes_arc();
        assert_eq!(arc.len(), 1);
    }

    #[test]
    fn test_chain_stp_mode_default_and_set() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        assert_eq!(chain.stp_mode(), STPMode::None);

        chain.set_stp_mode(STPMode::CancelTaker);
        assert_eq!(chain.stp_mode(), STPMode::CancelTaker);
    }

    #[test]
    fn test_chain_fee_schedule_set_clear() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        assert!(chain.fee_schedule().is_none());

        let schedule = FeeSchedule::new(-2, 5);
        chain.set_fee_schedule(schedule);
        assert!(chain.fee_schedule().is_some());

        chain.clear_fee_schedule();
        assert!(chain.fee_schedule().is_none());
    }

    // ── OptionChainOrderBookManager config propagation ───────────────────

    #[test]
    fn test_manager_set_specs_propagates() {
        let manager = OptionChainOrderBookManager::new("BTC");

        let specs = ContractSpecs::builder().tick_size(100).lot_size(1).build();
        manager.set_specs(specs);

        let chain = manager.get_or_create(test_expiration());
        assert!(chain.specs().is_some());
    }

    #[test]
    fn test_manager_specs_accessor() {
        let manager = OptionChainOrderBookManager::new("BTC");
        assert!(manager.specs().is_none());

        let specs = ContractSpecs::builder().tick_size(100).lot_size(1).build();
        manager.set_specs(specs);
        assert!(manager.specs().is_some());
    }

    #[test]
    fn test_manager_stp_mode_propagates() {
        let manager = OptionChainOrderBookManager::new("BTC");
        assert_eq!(manager.stp_mode(), STPMode::None);

        manager.set_stp_mode(STPMode::CancelTaker);
        assert_eq!(manager.stp_mode(), STPMode::CancelTaker);

        let chain = manager.get_or_create(test_expiration());
        assert_eq!(chain.stp_mode(), STPMode::CancelTaker);
    }

    #[test]
    fn test_manager_fee_schedule_propagates() {
        let manager = OptionChainOrderBookManager::new("BTC");
        assert!(manager.fee_schedule().is_none());

        let schedule = FeeSchedule::new(-2, 5);
        manager.set_fee_schedule(schedule);
        assert!(manager.fee_schedule().is_some());

        let chain = manager.get_or_create(test_expiration());
        assert!(chain.fee_schedule().is_some());
    }

    #[test]
    fn test_manager_clear_fee_schedule() {
        let manager = OptionChainOrderBookManager::new("BTC");

        manager.set_fee_schedule(FeeSchedule::new(-2, 5));
        assert!(manager.fee_schedule().is_some());

        manager.clear_fee_schedule();
        assert!(manager.fee_schedule().is_none());

        // New chain should NOT inherit fees
        let chain = manager.get_or_create(test_expiration());
        assert!(chain.fee_schedule().is_none());
    }

    #[test]
    fn test_manager_validation_config_accessor() {
        let manager = OptionChainOrderBookManager::new("BTC");
        assert!(manager.validation_config().is_none());

        let config = ValidationConfig::new().with_tick_size(100);
        manager.set_validation(config);
        assert!(manager.validation_config().is_some());
    }

    #[test]
    fn test_manager_iter() {
        let manager = OptionChainOrderBookManager::new("BTC");

        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(30.0))));
        drop(manager.get_or_create(ExpirationDate::Days(pos_or_panic!(60.0))));

        let count = manager.iter().count();
        assert_eq!(count, 2);
    }
}
