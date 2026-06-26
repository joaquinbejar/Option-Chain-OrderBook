//! Underlying order book module.
//!
//! This module provides the [`UnderlyingOrderBook`] and [`UnderlyingOrderBookManager`]
//! for managing all underlyings in the system.

use super::contract_specs::ContractSpecs;
use super::expiration::{
    ExpirationMassCancelResult, ExpirationOrderBook, ExpirationOrderBookManager,
};
use super::expiry_cycle::{ExpiryCycleConfig, SharedExpiryCycleConfig};
use super::fees::SharedFeeSchedule;
use super::index_price_feed::IndexPriceFeed;
use super::instrument_registry::{InstrumentInfo, InstrumentRegistry};
use super::stp::SharedSTPMode;
use super::strike_range::{ExpiryType, SharedStrikeRangeConfigs, StrikeRangeConfig};
use super::symbol_index::SymbolIndex;
use super::validation::ValidationConfig;
use crate::error::{Error, Result};
use crossbeam_skiplist::SkipMap;
use optionstratlib::ExpirationDate;
use orderbook_rs::{FeeSchedule, OrderId, OrderStatus, STPMode, Side};
use pricelevel::Hash32;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::book::TerminalOrderSummary;

/// Order book for a single underlying asset.
///
/// Contains all expirations for a specific underlying.
///
/// ## Architecture
///
/// ```text
/// UnderlyingOrderBook (per underlying)
///   └── ExpirationOrderBookManager
///         └── ExpirationOrderBook (per expiry)
///               └── OptionChainOrderBook
///                     └── StrikeOrderBook (per strike)
/// ```
pub struct UnderlyingOrderBook {
    /// The underlying asset symbol.
    underlying: String,
    /// Expiration order book manager.
    expirations: ExpirationOrderBookManager,
    /// Instrument registry propagated to expiration managers.
    registry: Option<Arc<InstrumentRegistry>>,
    /// Symbol index for O(1) lookup by symbol string.
    symbol_index: Option<Arc<SymbolIndex>>,
    /// Strike range configurations per expiry type.
    strike_range_configs: SharedStrikeRangeConfigs,
    /// Expiry cycle configuration for automatic expiration date generation.
    expiry_cycle_config: SharedExpiryCycleConfig,
    /// External index price feed for mark price computation.
    index_feed: Mutex<Option<Arc<dyn IndexPriceFeed>>>,
}

/// Underlying-level mass cancel summary.
///
/// # Description
///
/// Aggregates per-expiration mass cancel results for an underlying.
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
/// use option_chain_orderbook::orderbook::UnderlyingMassCancelResult;
///
/// let result = UnderlyingMassCancelResult { per_child: Vec::new() };
/// assert_eq!(result.total_cancelled(), 0);
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct UnderlyingMassCancelResult {
    /// Per-expiration cancellation results keyed by expiration.
    pub per_child: Vec<(String, ExpirationMassCancelResult)>,
}

impl UnderlyingMassCancelResult {
    /// Returns the number of expiration books with cancelled orders.
    ///
    /// # Description
    ///
    /// Counts how many expiration books recorded at least one cancelled order.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Number of expiration books affected (books).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::UnderlyingMassCancelResult;
    ///
    /// let result = UnderlyingMassCancelResult { per_child: Vec::new() };
    /// assert_eq!(result.books_affected(), 0);
    /// ```
    #[must_use]
    pub fn books_affected(&self) -> usize {
        self.per_child
            .iter()
            .filter(|(_, result)| result.total_cancelled() > 0)
            .count()
    }

    /// Returns the total number of cancelled orders across the underlying.
    ///
    /// # Description
    ///
    /// Sums cancelled orders across every expiration for this underlying.
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
    /// use option_chain_orderbook::orderbook::UnderlyingMassCancelResult;
    ///
    /// let result = UnderlyingMassCancelResult { per_child: Vec::new() };
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

impl UnderlyingOrderBook {
    /// Creates a new underlying order book.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol (e.g., "BTC")
    #[must_use]
    pub fn new(underlying: impl Into<String>) -> Self {
        let underlying = underlying.into();

        Self {
            expirations: ExpirationOrderBookManager::new(&underlying),
            underlying,
            registry: None,
            symbol_index: None,
            strike_range_configs: SharedStrikeRangeConfigs::new(),
            expiry_cycle_config: SharedExpiryCycleConfig::new(),
            index_feed: Mutex::new(None),
        }
    }

    /// Creates a new underlying order book with an instrument registry.
    ///
    /// The registry is propagated to the internal [`ExpirationOrderBookManager`]
    /// and all subsequently created expirations, chains, and strikes.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `registry` - The instrument registry for ID allocation
    #[must_use]
    pub fn new_with_registry(
        underlying: impl Into<String>,
        registry: Arc<InstrumentRegistry>,
    ) -> Self {
        let underlying = underlying.into();

        Self {
            expirations: ExpirationOrderBookManager::new_with_registry(
                &underlying,
                Arc::clone(&registry),
            ),
            underlying,
            registry: Some(registry),
            symbol_index: None,
            strike_range_configs: SharedStrikeRangeConfigs::new(),
            expiry_cycle_config: SharedExpiryCycleConfig::new(),
            index_feed: Mutex::new(None),
        }
    }

    /// Creates a new underlying order book with both instrument registry and symbol index.
    ///
    /// Both are propagated through the hierarchy for ID allocation and symbol lookup.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `registry` - The instrument registry for ID allocation
    /// * `symbol_index` - The symbol index for O(1) lookups
    #[must_use]
    pub fn new_with_registry_and_index(
        underlying: impl Into<String>,
        registry: Arc<InstrumentRegistry>,
        symbol_index: Arc<SymbolIndex>,
    ) -> Self {
        let underlying = underlying.into();

        Self {
            expirations: ExpirationOrderBookManager::new_with_registry_and_index(
                &underlying,
                Arc::clone(&registry),
                Arc::clone(&symbol_index),
            ),
            underlying,
            registry: Some(registry),
            symbol_index: Some(symbol_index),
            strike_range_configs: SharedStrikeRangeConfigs::new(),
            expiry_cycle_config: SharedExpiryCycleConfig::new(),
            index_feed: Mutex::new(None),
        }
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns a reference to the expiration manager.
    #[must_use]
    pub const fn expirations(&self) -> &ExpirationOrderBookManager {
        &self.expirations
    }

    /// Returns a reference to the instrument registry, if any.
    #[must_use]
    pub fn registry(&self) -> Option<&Arc<InstrumentRegistry>> {
        self.registry.as_ref()
    }

    /// Returns a reference to the symbol index, if any.
    #[must_use]
    pub fn symbol_index(&self) -> Option<&Arc<SymbolIndex>> {
        self.symbol_index.as_ref()
    }

    /// Sets the contract specifications for this underlying.
    ///
    /// Automatically derives and applies a [`ValidationConfig`] from the specs'
    /// tick size, lot size, min/max order size fields. This validation config
    /// is propagated to all future expirations and strikes.
    ///
    /// Existing expiration books and strikes are not affected by the derived
    /// validation config.
    pub fn set_specs(&self, specs: ContractSpecs) {
        let validation = specs.to_validation_config();
        self.expirations.set_specs(specs);
        self.expirations.set_validation(validation);
    }

    /// Returns the current contract specifications, if any.
    ///
    /// Delegates to the expiration manager to maintain a single source of truth.
    #[must_use]
    pub fn specs(&self) -> Option<ContractSpecs> {
        self.expirations.specs()
    }

    /// Sets the validation config for all future expirations and strikes
    /// created within this underlying.
    ///
    /// Delegates to [`ExpirationOrderBookManager::set_validation`].
    /// Existing expiration books and strikes are not affected.
    pub fn set_validation(&self, config: ValidationConfig) {
        self.expirations.set_validation(config);
    }

    /// Returns the current validation config, if any.
    #[must_use]
    pub fn validation_config(&self) -> Option<ValidationConfig> {
        self.expirations.validation_config()
    }

    /// Sets the STP mode for all future option books created within this underlying.
    ///
    /// Delegates to [`ExpirationOrderBookManager::set_stp_mode`].
    /// Existing books are not affected.
    #[inline]
    pub fn set_stp_mode(&self, mode: STPMode) {
        self.expirations.set_stp_mode(mode);
    }

    /// Returns the current STP mode.
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.expirations.stp_mode()
    }

    /// Sets the fee schedule for future expirations created within this underlying.
    ///
    /// Only expirations created after this call (via the internal
    /// [`ExpirationOrderBookManager`]) inherit the schedule. Existing
    /// expirations and their strikes are not affected.
    #[inline]
    pub fn set_fee_schedule(&self, schedule: FeeSchedule) {
        self.expirations.set_fee_schedule(schedule);
    }

    /// Clears the fee schedule so future option books have no fees configured.
    ///
    /// Delegates to [`ExpirationOrderBookManager::clear_fee_schedule`].
    /// Existing books are not affected.
    #[inline]
    pub fn clear_fee_schedule(&self) {
        self.expirations.clear_fee_schedule();
    }

    /// Returns the current fee schedule, or `None` if no fees are configured.
    #[must_use]
    #[inline]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.expirations.fee_schedule()
    }

    /// Sets the strike range configuration for a specific expiry type.
    ///
    /// This configuration determines how strikes are generated around the ATM
    /// price for the given expiration type. The configuration is validated
    /// before being stored.
    ///
    /// # Arguments
    ///
    /// * `expiry_type` - The expiration type to configure
    /// * `config` - The strike range configuration
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if the config is invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{UnderlyingOrderBook, ExpiryType, StrikeRangeConfig};
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// let config = StrikeRangeConfig::builder()
    ///     .range_pct(0.15)
    ///     .strike_interval(1000)
    ///     .build()
    ///     .expect("valid config");
    ///
    /// book.set_strike_range_config(ExpiryType::Weekly, config)
    ///     .expect("config should be valid");
    /// ```
    #[inline]
    pub fn set_strike_range_config(
        &self,
        expiry_type: ExpiryType,
        config: StrikeRangeConfig,
    ) -> Result<()> {
        config.validate()?;
        self.strike_range_configs.set(expiry_type, config);
        Ok(())
    }

    /// Returns the strike range configuration for a specific expiry type.
    ///
    /// # Arguments
    ///
    /// * `expiry_type` - The expiration type to query
    ///
    /// # Returns
    ///
    /// The configuration if set, or `None` if no configuration exists for this type.
    #[must_use]
    #[inline]
    pub fn strike_range_config(&self, expiry_type: ExpiryType) -> Option<StrikeRangeConfig> {
        self.strike_range_configs.get(expiry_type)
    }

    /// Returns all strike range configurations for this underlying.
    ///
    /// # Returns
    ///
    /// A map of expiry type to configuration.
    #[must_use]
    pub fn strike_range_configs(&self) -> HashMap<ExpiryType, StrikeRangeConfig> {
        self.strike_range_configs.get_all()
    }

    /// Removes the strike range configuration for a specific expiry type.
    ///
    /// # Arguments
    ///
    /// * `expiry_type` - The expiration type to remove
    ///
    /// # Returns
    ///
    /// The removed configuration if it existed.
    pub fn remove_strike_range_config(&self, expiry_type: ExpiryType) -> Option<StrikeRangeConfig> {
        self.strike_range_configs.remove(expiry_type)
    }

    /// Clears all strike range configurations for this underlying.
    pub fn clear_strike_range_configs(&self) {
        self.strike_range_configs.clear();
    }

    /// Sets the expiry cycle configuration for this underlying.
    ///
    /// The configuration is validated before being stored. It determines which
    /// expiration dates are auto-created and when they expire.
    ///
    /// # Arguments
    ///
    /// * `config` - The expiry cycle configuration to store
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if the config is invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{UnderlyingOrderBook, ExpiryCycleConfig};
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// book.set_expiry_cycle_config(ExpiryCycleConfig::default())
    ///     .expect("default config should be valid");
    /// ```
    #[inline]
    pub fn set_expiry_cycle_config(&self, config: ExpiryCycleConfig) -> Result<()> {
        config.validate()?;
        self.expiry_cycle_config.set(config);
        Ok(())
    }

    /// Returns the current expiry cycle configuration, or `None` if unset.
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{UnderlyingOrderBook, ExpiryCycleConfig};
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// assert!(book.expiry_cycle_config().is_none());
    /// book.set_expiry_cycle_config(ExpiryCycleConfig::default()).expect("valid");
    /// assert!(book.expiry_cycle_config().is_some());
    /// ```
    #[must_use]
    #[inline]
    pub fn expiry_cycle_config(&self) -> Option<ExpiryCycleConfig> {
        self.expiry_cycle_config.get()
    }

    /// Clears the expiry cycle configuration for this underlying.
    ///
    /// After this call, [`expiry_cycle_config`](Self::expiry_cycle_config) returns `None`.
    #[inline]
    pub fn clear_expiry_cycle_config(&self) {
        self.expiry_cycle_config.clear();
    }

    /// Sets the external index price feed for this underlying.
    ///
    /// The feed is stored and its latest price is available via
    /// [`index_feed`](Self::index_feed). Replaces any previously set feed.
    ///
    /// # Arguments
    ///
    /// * `feed` - The index price feed to attach
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use option_chain_orderbook::orderbook::{
    ///     UnderlyingOrderBook, MockPriceFeed, IndexPriceFeed,
    /// };
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// let feed: Arc<dyn IndexPriceFeed> = Arc::new(MockPriceFeed::new());
    /// book.set_index_feed(Arc::clone(&feed));
    /// assert!(book.index_feed().is_some());
    /// ```
    pub fn set_index_feed(&self, feed: Arc<dyn IndexPriceFeed>) {
        let mut guard = self.index_feed.lock().unwrap_or_else(|p| p.into_inner());
        *guard = Some(feed);
    }

    /// Returns the currently attached index price feed, if any.
    #[must_use]
    pub fn index_feed(&self) -> Option<Arc<dyn IndexPriceFeed>> {
        self.index_feed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Gets or creates an expiration order book, returning an Arc reference.
    pub fn get_or_create_expiration(&self, expiration: ExpirationDate) -> Arc<ExpirationOrderBook> {
        self.expirations.get_or_create(expiration)
    }

    /// Gets or creates an expiration order book, reporting whether this call
    /// performed the insertion.
    ///
    /// Returns `(book, inserted)` where `inserted` is `true` for exactly one
    /// caller across all concurrent callers for a given expiration — the one
    /// whose freshly built book won the atomic publish. Delegates to
    /// [`ExpirationOrderBookManager::get_or_create_inserted`].
    ///
    /// This is the race-free signal the expiry scheduler uses to decide that an
    /// expiration was genuinely created now, replacing a separate
    /// [`get_expiration`](Self::get_expiration) probe that reopens a
    /// check-then-act race under concurrency.
    #[must_use]
    pub fn get_or_create_expiration_inserted(
        &self,
        expiration: ExpirationDate,
    ) -> (Arc<ExpirationOrderBook>, bool) {
        self.expirations.get_or_create_inserted(expiration)
    }

    /// Gets an expiration order book.
    ///
    /// # Errors
    ///
    /// Returns `Error::ExpirationNotFound` if the expiration does not exist.
    pub fn get_expiration(&self, expiration: &ExpirationDate) -> Result<Arc<ExpirationOrderBook>> {
        self.expirations.get(expiration)
    }

    /// Returns the number of expirations.
    #[must_use]
    pub fn expiration_count(&self) -> usize {
        self.expirations.len()
    }

    /// Returns true if there are no expirations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.expirations.is_empty()
    }

    /// Returns the total order count across all expirations.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.expirations.total_order_count()
    }

    /// Cancels all resting orders across every expiration.
    ///
    /// # Description
    ///
    /// Cancels every resting order across all expirations for this underlying
    /// and returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// An [`UnderlyingMassCancelResult`] containing per-expiration results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::UnderlyingOrderBook;
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// let result = match book.cancel_all() {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_all(&self) -> Result<UnderlyingMassCancelResult> {
        let mut per_child = Vec::new();

        for (expiration, book) in self.expirations.iter() {
            let expiration_key = expiration.to_string();
            let result = book.cancel_all()?;
            per_child.push((expiration_key, result));
        }

        Ok(UnderlyingMassCancelResult { per_child })
    }

    /// Cancels all resting orders on a specific side across every expiration.
    ///
    /// # Description
    ///
    /// Cancels every resting order on the provided side across all expirations
    /// for this underlying and returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// * `side` - Side to cancel ([`Side::Buy`] or [`Side::Sell`]).
    ///
    /// # Returns
    ///
    /// An [`UnderlyingMassCancelResult`] containing per-expiration results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{Side, UnderlyingOrderBook};
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// let result = match book.cancel_by_side(Side::Buy) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_side(&self, side: Side) -> Result<UnderlyingMassCancelResult> {
        let mut per_child = Vec::new();

        for (expiration, book) in self.expirations.iter() {
            let expiration_key = expiration.to_string();
            let result = book.cancel_by_side(side)?;
            per_child.push((expiration_key, result));
        }

        Ok(UnderlyingMassCancelResult { per_child })
    }

    /// Cancels all resting orders for a specific user across every expiration.
    ///
    /// # Description
    ///
    /// Cancels every resting order attributed to the provided user identifier
    /// across all expirations for this underlying and returns the aggregated
    /// cancellation details.
    ///
    /// # Arguments
    ///
    /// * `user_id` - User identifier to cancel (32-byte hash).
    ///
    /// # Returns
    ///
    /// An [`UnderlyingMassCancelResult`] containing per-expiration results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::UnderlyingOrderBook;
    /// use pricelevel::Hash32;
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// let user = Hash32::from([1u8; 32]);
    /// let result = match book.cancel_by_user(user) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_user(&self, user_id: Hash32) -> Result<UnderlyingMassCancelResult> {
        let mut per_child = Vec::new();

        for (expiration, book) in self.expirations.iter() {
            let expiration_key = expiration.to_string();
            let result = book.cancel_by_user(user_id)?;
            per_child.push((expiration_key, result));
        }

        Ok(UnderlyingMassCancelResult { per_child })
    }

    // ── Order Lifecycle Queries ────────────────────────────────────────────

    /// Finds an order anywhere in this underlying's expirations.
    ///
    /// # Description
    ///
    /// Searches all expirations for the specified order. Returns the option
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
        for (_, book) in self.expirations.iter() {
            if let Some(result) = book.find_order(order_id) {
                return Some(result);
            }
        }
        None
    }

    /// Returns the total number of active orders across all expirations.
    ///
    /// # Description
    ///
    /// Sums the active order counts from all expirations.
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
        self.expirations
            .iter()
            .map(|(_, book)| book.total_active_orders())
            .sum()
    }

    /// Removes terminal-state entries older than the specified duration.
    ///
    /// # Description
    ///
    /// Delegates to all expirations and returns the total purged.
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
        self.expirations
            .iter()
            .map(|(_, book)| book.purge_terminal_states(older_than))
            .sum()
    }

    /// Returns all currently active orders for a specific user.
    ///
    /// # Description
    ///
    /// Searches all expirations for resting orders belonging to the specified
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
        self.expirations
            .iter()
            .flat_map(|(_, book)| book.orders_by_user(user_id))
            .collect()
    }

    /// Returns a summary of terminal order transitions.
    ///
    /// # Description
    ///
    /// Aggregates the terminal order summaries from all expirations.
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
        self.expirations
            .iter()
            .map(|(_, book)| book.terminal_order_summary())
            .sum()
    }

    /// Returns the total strike count across all expirations.
    #[must_use]
    pub fn total_strike_count(&self) -> usize {
        self.expirations.total_strike_count()
    }

    /// Returns statistics about this underlying.
    #[must_use]
    pub fn stats(&self) -> UnderlyingStats {
        UnderlyingStats {
            underlying: self.underlying.clone(),
            expiration_count: self.expiration_count(),
            total_strikes: self.total_strike_count(),
            total_orders: self.total_order_count(),
        }
    }
}

/// Statistics about an underlying order book.
#[derive(Debug, Clone)]
pub struct UnderlyingStats {
    /// The underlying asset symbol.
    pub underlying: String,
    /// Number of expirations.
    pub expiration_count: usize,
    /// Total number of strikes.
    pub total_strikes: usize,
    /// Total number of orders.
    pub total_orders: usize,
}

impl std::fmt::Display for UnderlyingStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} expirations, {} strikes, {} orders",
            self.underlying, self.expiration_count, self.total_strikes, self.total_orders
        )
    }
}

/// Manages underlying order books for all assets.
///
/// This is the top-level manager for the entire order book hierarchy.
/// Uses `SkipMap` for thread-safe concurrent access.
///
/// ## Architecture
///
/// ```text
/// UnderlyingOrderBookManager (root)
///   └── UnderlyingOrderBook (per underlying: BTC, ETH, SPX, etc.)
///         └── ExpirationOrderBookManager
///               └── ExpirationOrderBook (per expiry)
///                     └── OptionChainOrderBook
///                           └── StrikeOrderBook (per strike)
///                                 ├── OptionOrderBook (call)
///                                 └── OptionOrderBook (put)
/// ```
pub struct UnderlyingOrderBookManager {
    /// Underlying order books indexed by symbol.
    underlyings: SkipMap<String, Arc<UnderlyingOrderBook>>,
    /// Shared instrument registry for allocating unique IDs.
    registry: Arc<InstrumentRegistry>,
    /// Shared symbol index for O(1) lookup by symbol string.
    symbol_index: Arc<SymbolIndex>,
    /// STP mode propagated to newly created underlying books.
    stp_mode: SharedSTPMode,
    /// Fee schedule propagated to newly created underlying books.
    fee_schedule: SharedFeeSchedule,
}

impl Default for UnderlyingOrderBookManager {
    fn default() -> Self {
        Self::new()
    }
}

impl UnderlyingOrderBookManager {
    /// Creates a new underlying order book manager.
    ///
    /// Instrument IDs start from 1. ID 0 is reserved for standalone
    /// [`OptionOrderBook`](super::book::OptionOrderBook) instances
    /// created outside the hierarchy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            underlyings: SkipMap::new(),
            registry: Arc::new(InstrumentRegistry::new()),
            symbol_index: Arc::new(SymbolIndex::new()),
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Creates a new underlying order book manager with a seed for the
    /// instrument ID allocator.
    ///
    /// Use this to resume ID allocation after a hierarchy rebuild,
    /// ensuring previously assigned IDs are not reused.
    ///
    /// # Arguments
    ///
    /// * `seed` - The starting instrument ID value
    #[must_use]
    pub fn new_with_seed(seed: u32) -> Self {
        Self {
            underlyings: SkipMap::new(),
            registry: Arc::new(InstrumentRegistry::new_with_seed(seed)),
            symbol_index: Arc::new(SymbolIndex::new()),
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Returns the number of underlyings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.underlyings.len()
    }

    /// Returns true if there are no underlyings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.underlyings.is_empty()
    }

    /// Gets or creates an underlying order book.
    ///
    /// The shared [`InstrumentRegistry`] is automatically propagated
    /// so that all [`OptionOrderBook`](super::book::OptionOrderBook)
    /// instances created through the hierarchy receive unique IDs.
    ///
    /// # Concurrency
    ///
    /// This is atomic and idempotent. The fresh underlying book is fully
    /// configured before it is published, and publishing uses
    /// [`SkipMap::get_or_insert`], which inserts only when the key is absent and
    /// never evicts an existing entry. The first inserter wins and is never
    /// orphaned; concurrent losers receive the winning handle and drop their
    /// fresh book. Building and configuring the book has no global side effects
    /// (instrument IDs and symbol-index entries are only allocated lazily at
    /// strike creation), so a dropped loser leaks nothing.
    pub fn get_or_create(&self, underlying: impl Into<String>) -> Arc<UnderlyingOrderBook> {
        let underlying = underlying.into();
        if let Some(entry) = self.underlyings.get(&underlying) {
            return Arc::clone(entry.value());
        }
        // Build and fully configure the fresh book BEFORE publishing it, so the
        // winning book is configured-before-visible. Configuration only mutates
        // the fresh object (no global side effects), so a race loser's book is
        // dropped harmlessly.
        let fresh = Arc::new(UnderlyingOrderBook::new_with_registry_and_index(
            &underlying,
            Arc::clone(&self.registry),
            Arc::clone(&self.symbol_index),
        ));
        let stp = self.stp_mode.get();
        if stp != STPMode::None {
            fresh.set_stp_mode(stp);
        }
        if let Some(schedule) = self.fee_schedule.get() {
            fresh.set_fee_schedule(schedule);
        }
        // Atomic, idempotent publish: first inserter wins and is never evicted.
        let entry = self.underlyings.get_or_insert(underlying, fresh);
        Arc::clone(entry.value())
    }

    /// Sets the STP mode for all future underlying books created by this manager.
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

    /// Sets the fee schedule for all future underlying books created by this manager.
    ///
    /// Existing books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will have this schedule propagated.
    #[inline]
    pub fn set_fee_schedule(&self, schedule: FeeSchedule) {
        self.fee_schedule.set(Some(schedule));
    }

    /// Clears the fee schedule so future underlying books have no fees configured.
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

    /// Returns a reference to the symbol index.
    #[must_use]
    #[inline]
    pub fn symbol_index(&self) -> &Arc<SymbolIndex> {
        &self.symbol_index
    }

    /// Looks up an [`OptionOrderBook`](super::book::OptionOrderBook) by its symbol string.
    ///
    /// This provides O(1) lookup across the entire hierarchy by using the
    /// symbol index to find the coordinates, then traversing to the target book.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The full option symbol (e.g., "BTC-20260130-50000-C")
    ///
    /// # Returns
    ///
    /// An `Arc<OptionOrderBook>` reference to the target book.
    ///
    /// # Errors
    ///
    /// Returns `Error::SymbolNotFound` if the symbol is not registered.
    /// Returns hierarchy errors if the underlying/expiration/strike no longer exists.
    ///
    /// # Note
    ///
    /// There is a potential race condition: if a strike is removed between the
    /// index lookup and the hierarchy traversal, this method returns
    /// `StrikeNotFound` rather than `SymbolNotFound`. This is acceptable since
    /// strike removal is rare and the symbol is correctly deregistered.
    pub fn get_by_symbol(&self, symbol: &str) -> Result<Arc<super::book::OptionOrderBook>> {
        let sym_ref = self
            .symbol_index
            .get(symbol)
            .ok_or_else(|| Error::symbol_not_found(symbol))?;

        let underlying = self.get(sym_ref.underlying())?;
        let expiration = underlying.get_expiration(sym_ref.expiration())?;
        let strike = expiration.get_strike(sym_ref.strike())?;
        Ok(strike.get_arc(sym_ref.option_style()))
    }

    /// Gets an underlying order book.
    ///
    /// # Errors
    ///
    /// Returns `Error::UnderlyingNotFound` if the underlying does not exist.
    pub fn get(&self, underlying: &str) -> Result<Arc<UnderlyingOrderBook>> {
        self.underlyings
            .get(underlying)
            .map(|e| Arc::clone(e.value()))
            .ok_or_else(|| Error::underlying_not_found(underlying))
    }

    /// Returns true if an underlying exists.
    #[must_use]
    pub fn contains(&self, underlying: &str) -> bool {
        self.underlyings.contains_key(underlying)
    }

    /// Returns an iterator over all underlyings.
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = crossbeam_skiplist::map::Entry<'_, String, Arc<UnderlyingOrderBook>>>
    {
        self.underlyings.iter()
    }

    /// Removes an underlying order book.
    pub fn remove(&self, underlying: &str) -> bool {
        self.underlyings.remove(underlying).is_some()
    }

    /// Returns all underlying symbols (sorted).
    /// SkipMap maintains sorted order, so no additional sorting needed.
    pub fn underlying_symbols(&self) -> Vec<String> {
        self.underlyings.iter().map(|e| e.key().clone()).collect()
    }

    /// Returns the total order count across all underlyings.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.underlyings
            .iter()
            .map(|e| e.value().total_order_count())
            .sum()
    }

    /// Cancels all resting orders across every underlying.
    ///
    /// # Description
    ///
    /// Cancels every resting order across all underlyings and returns the
    /// aggregated cancellation details. Complexity is O(U × E × S) where U is
    /// the number of underlyings, E expirations per underlying, and S strikes
    /// per expiration.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// A [`GlobalMassCancelResult`] containing per-underlying results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
    ///
    /// let manager = UnderlyingOrderBookManager::new();
    /// let result = match manager.cancel_all_across_underlyings() {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_all_across_underlyings(&self) -> Result<GlobalMassCancelResult> {
        let mut per_child = Vec::new();

        for entry in self.underlyings.iter() {
            let underlying_key = entry.key().clone();
            let result = entry.value().cancel_all()?;
            per_child.push((underlying_key, result));
        }

        Ok(GlobalMassCancelResult { per_child })
    }

    /// Cancels all resting orders on a specific side across every underlying.
    ///
    /// # Description
    ///
    /// Cancels every resting order on the provided side across all underlyings
    /// and returns the aggregated cancellation details. Complexity is O(U × E × S)
    /// where U is the number of underlyings, E expirations, and S strikes.
    ///
    /// # Arguments
    ///
    /// * `side` - Side to cancel ([`Side::Buy`] or [`Side::Sell`]).
    ///
    /// # Returns
    ///
    /// A [`GlobalMassCancelResult`] containing per-underlying results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::{Side, UnderlyingOrderBookManager};
    ///
    /// let manager = UnderlyingOrderBookManager::new();
    /// let result = match manager.cancel_by_side_across_underlyings(Side::Buy) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_side_across_underlyings(&self, side: Side) -> Result<GlobalMassCancelResult> {
        let mut per_child = Vec::new();

        for entry in self.underlyings.iter() {
            let underlying_key = entry.key().clone();
            let result = entry.value().cancel_by_side(side)?;
            per_child.push((underlying_key, result));
        }

        Ok(GlobalMassCancelResult { per_child })
    }

    /// Cancels all resting orders for a specific user across every underlying.
    ///
    /// # Description
    ///
    /// Cancels every resting order attributed to the provided user identifier
    /// across all underlyings and returns the aggregated cancellation details.
    /// Complexity is O(U × E × S) where U is the number of underlyings,
    /// E expirations, and S strikes.
    ///
    /// # Arguments
    ///
    /// * `user_id` - User identifier to cancel (32-byte hash).
    ///
    /// # Returns
    ///
    /// A [`GlobalMassCancelResult`] containing per-underlying results plus
    /// aggregated counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
    /// use pricelevel::Hash32;
    ///
    /// let manager = UnderlyingOrderBookManager::new();
    /// let user = Hash32::from([1u8; 32]);
    /// let result = match manager.cancel_by_user_across_underlyings(user) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_user_across_underlyings(
        &self,
        user_id: Hash32,
    ) -> Result<GlobalMassCancelResult> {
        let mut per_child = Vec::new();

        for entry in self.underlyings.iter() {
            let underlying_key = entry.key().clone();
            let result = entry.value().cancel_by_user(user_id)?;
            per_child.push((underlying_key, result));
        }

        Ok(GlobalMassCancelResult { per_child })
    }

    // ── Order Lifecycle Queries ────────────────────────────────────────────

    /// Finds an order anywhere across all underlyings.
    ///
    /// # Description
    ///
    /// Searches all underlyings for the specified order. Returns the option
    /// symbol and current status if found. Complexity is O(U × E × S) in the
    /// worst case.
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
    pub fn find_order_across_underlyings(
        &self,
        order_id: OrderId,
    ) -> Option<(String, OrderStatus)> {
        for entry in self.underlyings.iter() {
            if let Some(result) = entry.value().find_order(order_id) {
                return Some(result);
            }
        }
        None
    }

    /// Returns the total number of active orders across all underlyings.
    ///
    /// # Description
    ///
    /// Sums the active order counts from all underlyings.
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
    pub fn total_active_orders_across_underlyings(&self) -> usize {
        self.underlyings
            .iter()
            .map(|entry| entry.value().total_active_orders())
            .sum()
    }

    /// Removes terminal-state entries older than the specified duration.
    ///
    /// # Description
    ///
    /// Delegates to all underlyings and returns the total purged.
    /// Complexity is O(U × E × S) where U is the number of underlyings,
    /// E expirations, and S strikes.
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
    pub fn purge_terminal_states_across_underlyings(&self, older_than: Duration) -> usize {
        self.underlyings
            .iter()
            .map(|entry| entry.value().purge_terminal_states(older_than))
            .sum()
    }

    /// Returns all currently active orders for a specific user across all underlyings.
    ///
    /// # Description
    ///
    /// Searches all underlyings for resting orders belonging to the specified
    /// user. Returns tuples of (symbol, order_id, status). Complexity is
    /// O(U × E × S) where U is the number of underlyings, E expirations,
    /// and S strikes.
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
    pub fn orders_by_user_across_underlyings(
        &self,
        user_id: Hash32,
    ) -> Vec<(String, OrderId, OrderStatus)> {
        self.underlyings
            .iter()
            .flat_map(|entry| entry.value().orders_by_user(user_id))
            .collect()
    }

    /// Returns a summary of terminal order transitions across all underlyings.
    ///
    /// # Description
    ///
    /// Aggregates the terminal order summaries from all underlyings.
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
    pub fn terminal_order_summary_across_underlyings(&self) -> TerminalOrderSummary {
        self.underlyings
            .iter()
            .map(|entry| entry.value().terminal_order_summary())
            .sum()
    }

    /// Returns the total expiration count across all underlyings.
    #[must_use]
    pub fn total_expiration_count(&self) -> usize {
        self.underlyings
            .iter()
            .map(|e| e.value().expiration_count())
            .sum()
    }

    /// Returns the total strike count across all underlyings.
    #[must_use]
    pub fn total_strike_count(&self) -> usize {
        self.underlyings
            .iter()
            .map(|e| e.value().total_strike_count())
            .sum()
    }

    /// Looks up instrument info by numeric instrument ID.
    ///
    /// Returns `None` if the ID is not registered.
    ///
    /// # Arguments
    ///
    /// * `id` - The instrument ID to look up
    #[must_use]
    pub fn get_by_instrument_id(&self, id: u32) -> Option<InstrumentInfo> {
        self.registry.get(id)
    }

    /// Returns the number of registered instruments across all underlyings.
    #[must_use]
    pub fn instrument_count(&self) -> usize {
        self.registry.len()
    }

    /// Returns the current instrument ID counter value.
    ///
    /// This is the next ID that will be allocated. Useful for persisting
    /// the counter state before shutdown so it can be used as a seed
    /// for [`new_with_seed`](Self::new_with_seed).
    #[must_use]
    pub fn current_instrument_id(&self) -> u32 {
        self.registry.current_id()
    }

    /// Returns a reference to the shared instrument registry.
    #[must_use]
    pub fn registry(&self) -> &Arc<InstrumentRegistry> {
        &self.registry
    }

    /// Returns statistics about the entire order book system.
    #[must_use]
    pub fn stats(&self) -> GlobalStats {
        GlobalStats {
            underlying_count: self.len(),
            total_expirations: self.total_expiration_count(),
            total_strikes: self.total_strike_count(),
            total_orders: self.total_order_count(),
        }
    }
}

/// Global statistics about the order book system.
#[derive(Debug, Clone)]
pub struct GlobalStats {
    /// Number of underlyings.
    pub underlying_count: usize,
    /// Total number of expirations.
    pub total_expirations: usize,
    /// Total number of strikes.
    pub total_strikes: usize,
    /// Total number of orders.
    pub total_orders: usize,
}

impl std::fmt::Display for GlobalStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} underlyings, {} expirations, {} strikes, {} orders",
            self.underlying_count, self.total_expirations, self.total_strikes, self.total_orders
        )
    }
}

/// Global mass cancel summary.
///
/// # Description
///
/// Aggregates per-underlying mass cancel results across the entire hierarchy.
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
/// use option_chain_orderbook::orderbook::GlobalMassCancelResult;
///
/// let result = GlobalMassCancelResult { per_child: Vec::new() };
/// assert_eq!(result.total_cancelled(), 0);
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct GlobalMassCancelResult {
    /// Per-underlying cancellation results keyed by underlying symbol.
    pub per_child: Vec<(String, UnderlyingMassCancelResult)>,
}

impl GlobalMassCancelResult {
    /// Returns the number of underlying books with cancelled orders.
    ///
    /// # Description
    ///
    /// Counts how many underlying books recorded at least one cancelled order.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Number of underlying books affected (books).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::GlobalMassCancelResult;
    ///
    /// let result = GlobalMassCancelResult { per_child: Vec::new() };
    /// assert_eq!(result.books_affected(), 0);
    /// ```
    #[must_use]
    pub fn books_affected(&self) -> usize {
        self.per_child
            .iter()
            .filter(|(_, result)| result.total_cancelled() > 0)
            .count()
    }

    /// Returns the total number of cancelled orders across all underlyings.
    ///
    /// # Description
    ///
    /// Sums cancelled orders across every underlying in the hierarchy.
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
    /// use option_chain_orderbook::orderbook::GlobalMassCancelResult;
    ///
    /// let result = GlobalMassCancelResult { per_child: Vec::new() };
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

#[cfg(test)]
mod tests {
    use super::*;
    use optionstratlib::prelude::pos_or_panic;
    use orderbook_rs::{OrderId, Side};

    fn test_expiration() -> ExpirationDate {
        ExpirationDate::Days(pos_or_panic!(30.0))
    }

    #[test]
    fn test_underlying_cancel_all() {
        let book = UnderlyingOrderBook::new("BTC");

        let exp1 = book.get_or_create_expiration(test_expiration());
        let s1 = exp1.get_or_create_strike(50000);
        if let Err(err) = s1
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(s1);
        drop(exp1);

        let exp2 = book.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(60.0)));
        let s2 = exp2.get_or_create_strike(52000);
        if let Err(err) = s2.put().add_limit_order(OrderId::new(), Side::Sell, 60, 5) {
            panic!("add order failed: {}", err);
        }
        drop(s2);
        drop(exp2);

        assert_eq!(book.total_order_count(), 2);

        let result = match book.cancel_all() {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 2);
        assert_eq!(result.books_affected(), 2);
        assert_eq!(book.total_order_count(), 0);
    }

    #[test]
    fn test_underlying_cancel_by_side() {
        let book = UnderlyingOrderBook::new("BTC");

        let exp = book.get_or_create_expiration(test_expiration());
        let s = exp.get_or_create_strike(50000);
        if let Err(err) = s.call().add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = s.call().add_limit_order(OrderId::new(), Side::Sell, 110, 5) {
            panic!("add order failed: {}", err);
        }
        drop(s);
        drop(exp);

        assert_eq!(book.total_order_count(), 2);

        let result = match book.cancel_by_side(Side::Sell) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 1);
        assert_eq!(book.total_order_count(), 1);
    }

    #[test]
    fn test_underlying_cancel_by_user() {
        let book = UnderlyingOrderBook::new("BTC");
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        let exp = book.get_or_create_expiration(test_expiration());
        let s = exp.get_or_create_strike(50000);
        if let Err(err) =
            s.call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) =
            s.put()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 60, 5, user_b)
        {
            panic!("add order failed: {}", err);
        }
        drop(s);
        drop(exp);

        assert_eq!(book.total_order_count(), 2);

        let result = match book.cancel_by_user(user_a) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 1);
        assert_eq!(book.total_order_count(), 1);
    }

    #[test]
    fn test_underlying_order_book_creation() {
        let book = UnderlyingOrderBook::new("BTC");

        assert_eq!(book.underlying(), "BTC");
        assert!(book.is_empty());
    }

    #[test]
    fn test_underlying_order_book_hierarchy() {
        let book = UnderlyingOrderBook::new("BTC");

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(book.expiration_count(), 1);
        assert_eq!(book.total_strike_count(), 1);
        assert_eq!(book.total_order_count(), 1);
    }

    #[test]
    fn test_underlying_order_book_get_expiration() {
        let book = UnderlyingOrderBook::new("BTC");
        let exp_date = test_expiration();

        book.get_or_create_expiration(exp_date);

        let exp = book.get_expiration(&exp_date);
        assert!(exp.is_ok());

        let missing_exp = ExpirationDate::Days(pos_or_panic!(90.0));
        let missing = book.get_expiration(&missing_exp);
        assert!(missing.is_err());
    }

    #[test]
    fn test_underlying_manager_creation() {
        let manager = UnderlyingOrderBookManager::new();

        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_underlying_manager_get_or_create() {
        let manager = UnderlyingOrderBookManager::new();

        drop(manager.get_or_create("BTC"));
        drop(manager.get_or_create("ETH"));
        drop(manager.get_or_create("SPX"));

        assert_eq!(manager.len(), 3);
    }

    #[test]
    fn test_underlying_manager_get_or_create_returns_same_handle() {
        let manager = UnderlyingOrderBookManager::new();

        let first = manager.get_or_create("BTC");
        let second = manager.get_or_create("BTC");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_underlying_manager_get_or_create_concurrent_returns_same_handle() {
        use std::sync::Barrier;
        use std::thread;

        // Race N threads to create the SAME underlying via a barrier for
        // lockstep. With `get_or_insert` the first inserter wins and is never
        // evicted, so every thread must observe one identical handle and the
        // map must hold exactly one entry (no orphan, no split-brain).
        const N: usize = 16;
        let manager = Arc::new(UnderlyingOrderBookManager::new());
        let barrier = Arc::new(Barrier::new(N));

        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                manager.get_or_create("BTC")
            }));
        }

        let books: Vec<Arc<UnderlyingOrderBook>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        let first = books.first().expect("no books returned");
        for book in &books {
            assert!(Arc::ptr_eq(first, book));
        }
        assert_eq!(manager.len(), 1);

        // End-to-end: creating one strike through the winning handle allocates
        // exactly one instrument-ID pair in the shared registry/index — proving
        // the whole sub-hierarchy shares a single registry with no orphan.
        let exp = first.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        assert_ne!(strike.call().instrument_id(), 0);
        assert_ne!(strike.put().instrument_id(), 0);
        assert_ne!(strike.call().instrument_id(), strike.put().instrument_id());
        assert_eq!(manager.symbol_index().len(), 2);
    }

    #[test]
    fn test_underlying_manager_full_hierarchy() {
        let manager = UnderlyingOrderBookManager::new();
        let exp_date = test_expiration();

        // Create BTC chain
        {
            let btc = manager.get_or_create("BTC");
            let exp = btc.get_or_create_expiration(exp_date);
            let strike = exp.get_or_create_strike(50000);
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

        // Create ETH chain
        {
            let eth = manager.get_or_create("ETH");
            let exp = eth.get_or_create_expiration(exp_date);
            exp.get_or_create_strike(3000);
        }

        let stats = manager.stats();
        assert_eq!(stats.underlying_count, 2);
        assert_eq!(stats.total_expirations, 2);
        assert_eq!(stats.total_strikes, 2);
        assert_eq!(stats.total_orders, 2);
    }

    #[test]
    fn test_underlying_order_book_expirations() {
        let book = UnderlyingOrderBook::new("BTC");
        drop(book.get_or_create_expiration(test_expiration()));
        let expirations = book.expirations();
        assert_eq!(expirations.len(), 1);
    }

    #[test]
    fn test_underlying_order_book_stats() {
        let book = UnderlyingOrderBook::new("BTC");

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(strike);
        drop(exp);

        let stats = book.stats();
        assert_eq!(stats.underlying, "BTC");
        assert_eq!(stats.expiration_count, 1);
        assert_eq!(stats.total_strikes, 1);
        assert_eq!(stats.total_orders, 1);

        let display = format!("{}", stats);
        assert!(display.contains("BTC"));
    }

    #[test]
    fn test_underlying_manager_get() {
        let manager = UnderlyingOrderBookManager::new();

        drop(manager.get_or_create("BTC"));

        assert!(manager.get("BTC").is_ok());
        assert!(manager.get("XRP").is_err());
    }

    #[test]
    fn test_underlying_manager_contains() {
        let manager = UnderlyingOrderBookManager::new();

        drop(manager.get_or_create("BTC"));

        assert!(manager.contains("BTC"));
        assert!(!manager.contains("XRP"));
    }

    #[test]
    fn test_underlying_manager_remove() {
        let manager = UnderlyingOrderBookManager::new();

        drop(manager.get_or_create("BTC"));
        drop(manager.get_or_create("ETH"));

        assert_eq!(manager.len(), 2);
        assert!(manager.remove("BTC"));
        assert_eq!(manager.len(), 1);
        assert!(!manager.remove("BTC"));
    }

    #[test]
    fn test_underlying_manager_underlying_symbols() {
        let manager = UnderlyingOrderBookManager::new();

        drop(manager.get_or_create("BTC"));
        drop(manager.get_or_create("ETH"));
        drop(manager.get_or_create("SPX"));

        let symbols = manager.underlying_symbols();
        assert_eq!(symbols.len(), 3);
        assert!(symbols.contains(&"BTC".to_string()));
        assert!(symbols.contains(&"ETH".to_string()));
        assert!(symbols.contains(&"SPX".to_string()));
    }

    #[test]
    fn test_underlying_manager_total_order_count() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(strike);
        drop(exp);
        drop(btc);

        assert_eq!(manager.total_order_count(), 1);
    }

    #[test]
    fn test_global_stats_display() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        exp.get_or_create_strike(50000);
        drop(exp);
        drop(btc);

        let stats = manager.stats();
        let display = format!("{}", stats);
        assert!(display.contains("1 underlyings"));
        assert!(display.contains("1 expirations"));
        assert!(display.contains("1 strikes"));
    }

    #[test]
    fn test_underlying_set_validation() {
        let book = UnderlyingOrderBook::new("BTC");
        let config = ValidationConfig::new().with_tick_size(100);
        book.set_validation(config.clone());

        assert_eq!(book.validation_config(), Some(config));

        let exp = book.get_or_create_expiration(test_expiration());
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
    fn test_underlying_set_validation_full_hierarchy() {
        let manager = UnderlyingOrderBookManager::new();
        let btc = manager.get_or_create("BTC");

        let config = ValidationConfig::new()
            .with_tick_size(100)
            .with_lot_size(10)
            .with_min_order_size(5)
            .with_max_order_size(1000);
        btc.set_validation(config);

        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        // Valid: price=200 (tick 100), qty=20 (lot 10, range 5..1000)
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 20)
                .is_ok()
        );

        // Invalid tick
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 20)
                .is_err()
        );

        // Invalid lot
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 15)
                .is_err()
        );

        // Too small
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 2)
                .is_err()
        );

        // Too large
        assert!(
            strike
                .put()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 2000)
                .is_err()
        );
    }

    #[test]
    fn test_underlying_no_validation_by_default() {
        let book = UnderlyingOrderBook::new("BTC");
        assert!(book.validation_config().is_none());

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 7)
                .is_ok()
        );
    }

    #[test]
    fn test_underlying_no_specs_by_default() {
        let book = UnderlyingOrderBook::new("BTC");
        assert!(book.specs().is_none());
    }

    #[test]
    fn test_underlying_set_specs() {
        use crate::orderbook::contract_specs::{ContractSpecs, ExerciseStyle, SettlementType};

        let book = UnderlyingOrderBook::new("BTC");
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .contract_size(1)
            .min_order_size(5)
            .max_order_size(1000)
            .settlement(SettlementType::Cash)
            .exercise_style(ExerciseStyle::European)
            .settlement_currency("USDC")
            .build()
            .expect("valid specs");

        book.set_specs(specs.clone());

        assert_eq!(book.specs(), Some(specs));
    }

    #[test]
    fn test_underlying_set_specs_derives_validation() {
        use crate::orderbook::contract_specs::ContractSpecs;

        let book = UnderlyingOrderBook::new("BTC");
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .min_order_size(5)
            .max_order_size(1000)
            .build()
            .expect("valid specs");

        book.set_specs(specs);

        // Validation should be auto-derived
        let config = book.validation_config();
        assert!(config.is_some());
        let config = match config {
            Some(c) => c,
            None => panic!("expected validation config"),
        };
        assert_eq!(config.tick_size(), Some(100));
        assert_eq!(config.lot_size(), Some(10));
        assert_eq!(config.min_order_size(), Some(5));
        assert_eq!(config.max_order_size(), Some(1000));
    }

    #[test]
    fn test_underlying_set_specs_enforces_validation_on_new_strikes() {
        use crate::orderbook::contract_specs::ContractSpecs;

        let book = UnderlyingOrderBook::new("BTC");
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .min_order_size(10)
            .max_order_size(1000)
            .build()
            .expect("valid specs");

        book.set_specs(specs);

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        // Valid: price=200 (tick 100), qty=20 (lot 10, range 10..1000)
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 20)
                .is_ok()
        );

        // Invalid tick
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 20)
                .is_err()
        );

        // Invalid lot
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 15)
                .is_err()
        );

        // Too small
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 5)
                .is_err()
        );

        // Too large
        assert!(
            strike
                .put()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 2000)
                .is_err()
        );
    }

    #[test]
    fn test_underlying_specs_propagate_through_full_hierarchy() {
        use crate::orderbook::contract_specs::{ContractSpecs, ExerciseStyle, SettlementType};

        let manager = UnderlyingOrderBookManager::new();
        let btc = manager.get_or_create("BTC");

        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .contract_size(1)
            .min_order_size(10)
            .max_order_size(1000)
            .settlement(SettlementType::Cash)
            .exercise_style(ExerciseStyle::European)
            .settlement_currency("USDC")
            .build()
            .expect("valid specs");

        btc.set_specs(specs.clone());

        // Create expiration after specs are set
        let exp = btc.get_or_create_expiration(test_expiration());

        // Specs should be accessible from expiration
        assert_eq!(exp.specs(), Some(specs.clone()));

        // Specs should be accessible from chain
        assert_eq!(exp.chain().specs(), Some(specs.clone()));

        // Validation should enforce tick size on new strikes
        let strike = exp.get_or_create_strike(50000);
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 20)
                .is_ok()
        );
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 20)
                .is_err()
        );
    }

    #[test]
    fn test_underlying_specs_existing_expiration_unaffected() {
        use crate::orderbook::contract_specs::ContractSpecs;

        let book = UnderlyingOrderBook::new("BTC");

        // Create expiration BEFORE setting specs
        let exp_before = book.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(30.0)));

        // Set specs after
        book.set_specs(
            ContractSpecs::builder()
                .tick_size(100)
                .build()
                .expect("valid specs"),
        );

        // Existing expiration's new strikes are NOT affected by validation
        let strike = exp_before.get_or_create_strike(50000);
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 7)
                .is_ok()
        );

        // New expiration IS affected
        let exp_after = book.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(60.0)));
        let strike2 = exp_after.get_or_create_strike(50000);
        assert!(
            strike2
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 7)
                .is_err()
        );
    }

    #[test]
    fn test_underlying_default_specs_are_permissive() {
        use crate::orderbook::contract_specs::ContractSpecs;

        let book = UnderlyingOrderBook::new("BTC");
        book.set_specs(ContractSpecs::default());

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        // Default specs: tick=1, lot=1, min=1, max=u64::MAX → everything passes
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 7)
                .is_ok()
        );
    }

    // --- Instrument ID tests ---

    #[test]
    fn test_manager_starts_with_id_one() {
        let manager = UnderlyingOrderBookManager::new();
        assert_eq!(manager.current_instrument_id(), 1);
        assert_eq!(manager.instrument_count(), 0);
    }

    #[test]
    fn test_manager_new_with_seed() {
        let manager = UnderlyingOrderBookManager::new_with_seed(100);
        assert_eq!(manager.current_instrument_id(), 100);
    }

    #[test]
    fn test_strike_creation_assigns_instrument_ids() {
        let manager = UnderlyingOrderBookManager::new();
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        // Call and put should have unique, non-zero IDs
        let call_id = strike.call().instrument_id();
        let put_id = strike.put().instrument_id();

        assert_ne!(call_id, 0);
        assert_ne!(put_id, 0);
        assert_ne!(call_id, put_id);
    }

    #[test]
    fn test_multiple_strikes_get_distinct_ids() {
        let manager = UnderlyingOrderBookManager::new();
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());

        let s1 = exp.get_or_create_strike(50000);
        let s2 = exp.get_or_create_strike(55000);
        let s3 = exp.get_or_create_strike(60000);

        let ids: Vec<u32> = vec![
            s1.call().instrument_id(),
            s1.put().instrument_id(),
            s2.call().instrument_id(),
            s2.put().instrument_id(),
            s3.call().instrument_id(),
            s3.put().instrument_id(),
        ];

        // All 6 IDs should be unique
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 6);

        // All non-zero
        assert!(ids.iter().all(|&id| id != 0));

        // 3 strikes × 2 books = 6 instruments
        assert_eq!(manager.instrument_count(), 6);
    }

    #[test]
    fn test_reverse_lookup_returns_correct_info() {
        let manager = UnderlyingOrderBookManager::new();
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        let call_id = strike.call().instrument_id();
        let put_id = strike.put().instrument_id();

        // Look up call
        let call_info = manager.get_by_instrument_id(call_id);
        assert!(call_info.is_some());
        let call_info = match call_info {
            Some(c) => c,
            None => panic!("expected instrument info"),
        };
        assert!(call_info.symbol().contains("50000"));
        assert!(call_info.symbol().ends_with("-C"));
        assert_eq!(call_info.strike(), 50000);
        assert_eq!(call_info.option_style(), optionstratlib::OptionStyle::Call);

        // Look up put
        let put_info = manager.get_by_instrument_id(put_id);
        assert!(put_info.is_some());
        let put_info = match put_info {
            Some(p) => p,
            None => panic!("expected instrument info"),
        };
        assert!(put_info.symbol().ends_with("-P"));
        assert_eq!(put_info.strike(), 50000);
        assert_eq!(put_info.option_style(), optionstratlib::OptionStyle::Put);
    }

    #[test]
    fn test_reverse_lookup_missing_returns_none() {
        let manager = UnderlyingOrderBookManager::new();
        assert!(manager.get_by_instrument_id(999).is_none());
    }

    #[test]
    fn test_ids_across_underlyings_are_unique() {
        let manager = UnderlyingOrderBookManager::new();
        let exp = test_expiration();

        let btc = manager.get_or_create("BTC");
        let btc_exp = btc.get_or_create_expiration(exp);
        let btc_strike = btc_exp.get_or_create_strike(50000);

        let eth = manager.get_or_create("ETH");
        let eth_exp = eth.get_or_create_expiration(exp);
        let eth_strike = eth_exp.get_or_create_strike(3000);

        let btc_call = btc_strike.call().instrument_id();
        let btc_put = btc_strike.put().instrument_id();
        let eth_call = eth_strike.call().instrument_id();
        let eth_put = eth_strike.put().instrument_id();

        let mut ids = vec![btc_call, btc_put, eth_call, eth_put];
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn test_seed_survives_rebuild() {
        // First manager: create some instruments
        let manager1 = UnderlyingOrderBookManager::new();
        let btc1 = manager1.get_or_create("BTC");
        let exp1 = btc1.get_or_create_expiration(test_expiration());
        exp1.get_or_create_strike(50000);
        let seed = manager1.current_instrument_id();

        // Second manager: rebuild with seed
        let manager2 = UnderlyingOrderBookManager::new_with_seed(seed);
        let btc2 = manager2.get_or_create("BTC");
        let exp2 = btc2.get_or_create_expiration(test_expiration());
        let s2 = exp2.get_or_create_strike(55000);

        // New IDs should start from where the first manager left off
        assert!(s2.call().instrument_id() >= seed);
        assert!(s2.put().instrument_id() >= seed);
    }

    #[test]
    fn test_idempotent_get_or_create_preserves_ids() {
        let manager = UnderlyingOrderBookManager::new();
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let s1 = exp.get_or_create_strike(50000);
        let call_id = s1.call().instrument_id();
        let put_id = s1.put().instrument_id();

        // Second get_or_create returns the same book
        let s1_again = exp.get_or_create_strike(50000);
        assert_eq!(s1_again.call().instrument_id(), call_id);
        assert_eq!(s1_again.put().instrument_id(), put_id);

        // No new instruments registered
        assert_eq!(manager.instrument_count(), 2);
    }

    #[test]
    fn test_registry_accessor() {
        let manager = UnderlyingOrderBookManager::new();
        let registry = manager.registry();
        assert_eq!(registry.current_id(), 1);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_standalone_books_have_zero_id() {
        use crate::orderbook::strike::StrikeOrderBook;

        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        assert_eq!(strike.call().instrument_id(), 0);
        assert_eq!(strike.put().instrument_id(), 0);
    }

    #[test]
    fn test_underlying_stp_default_is_none() {
        let book = UnderlyingOrderBook::new("BTC");
        assert_eq!(book.stp_mode(), STPMode::None);
    }

    #[test]
    fn test_underlying_set_stp_mode() {
        let book = UnderlyingOrderBook::new("BTC");
        book.set_stp_mode(STPMode::CancelTaker);
        assert_eq!(book.stp_mode(), STPMode::CancelTaker);
    }

    #[test]
    fn test_underlying_stp_propagates_to_new_strikes() {
        let book = UnderlyingOrderBook::new("BTC");
        book.set_stp_mode(STPMode::CancelTaker);

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        assert_eq!(strike.call().stp_mode(), STPMode::CancelTaker);
        assert_eq!(strike.put().stp_mode(), STPMode::CancelTaker);
        assert_eq!(strike.stp_mode(), STPMode::CancelTaker);
    }

    #[test]
    fn test_underlying_stp_existing_expiration_unaffected() {
        let book = UnderlyingOrderBook::new("BTC");

        // Create expiration BEFORE setting STP
        let exp_before = book.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(30.0)));

        // Set STP mode
        book.set_stp_mode(STPMode::CancelBoth);

        // Existing expiration's new strikes do NOT get STP
        let strike_old = exp_before.get_or_create_strike(50000);
        assert_eq!(strike_old.call().stp_mode(), STPMode::None);

        // New expiration DOES get STP
        let exp_after = book.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(60.0)));
        let strike_new = exp_after.get_or_create_strike(50000);
        assert_eq!(strike_new.call().stp_mode(), STPMode::CancelBoth);
    }

    #[test]
    fn test_manager_stp_propagates_through_full_hierarchy() {
        let manager = UnderlyingOrderBookManager::new();
        manager.set_stp_mode(STPMode::CancelTaker);

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        assert_eq!(strike.call().stp_mode(), STPMode::CancelTaker);
        assert_eq!(strike.put().stp_mode(), STPMode::CancelTaker);
    }

    #[test]
    fn test_manager_stp_default_is_none() {
        let manager = UnderlyingOrderBookManager::new();
        assert_eq!(manager.stp_mode(), STPMode::None);
    }

    #[test]
    fn test_manager_stp_existing_underlying_unaffected() {
        let manager = UnderlyingOrderBookManager::new();

        // Create underlying BEFORE setting STP
        let btc = manager.get_or_create("BTC");

        manager.set_stp_mode(STPMode::CancelBoth);

        // Existing underlying's new expirations do NOT get STP
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        assert_eq!(strike.call().stp_mode(), STPMode::None);

        // New underlying DOES get STP
        let eth = manager.get_or_create("ETH");
        let exp2 = eth.get_or_create_expiration(test_expiration());
        let strike2 = exp2.get_or_create_strike(50000);
        assert_eq!(strike2.call().stp_mode(), STPMode::CancelBoth);
    }

    #[test]
    fn test_stp_prevents_self_trade_through_hierarchy() {
        use pricelevel::Hash32;

        let manager = UnderlyingOrderBookManager::new();
        manager.set_stp_mode(STPMode::CancelTaker);

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        let user = Hash32::from([1u8; 32]);

        // Place a resting sell order on the call book
        if let Err(err) =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 100, 10, user)
        {
            panic!("add order failed: {}", err);
        }

        // Same user places a crossing buy — STP triggers
        let result =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user);
        assert!(result.is_err());

        // Different user trades normally
        let other_user = Hash32::from([2u8; 32]);
        if let Err(err) =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, other_user)
        {
            panic!("add order failed: {}", err);
        }
        assert_eq!(strike.call().order_count(), 0);
    }

    // ── Fee schedule tests ──────────────────────────────────────────────

    #[test]
    fn test_underlying_fee_schedule_default_is_none() {
        let book = UnderlyingOrderBook::new("BTC");
        assert!(book.fee_schedule().is_none());
    }

    #[test]
    fn test_underlying_set_fee_schedule() {
        let book = UnderlyingOrderBook::new("BTC");
        book.set_fee_schedule(FeeSchedule::new(-2, 5));
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
    fn test_underlying_fee_propagates_to_new_strikes() {
        let book = UnderlyingOrderBook::new("BTC");
        book.set_fee_schedule(FeeSchedule::new(-2, 5));

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        let call_fs = strike.call().fee_schedule();
        assert!(call_fs.is_some());
        let s = match call_fs {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(s.maker_fee_bps, -2);
        assert_eq!(s.taker_fee_bps, 5);

        let put_fs = strike.put().fee_schedule();
        assert!(put_fs.is_some());
        let s = match put_fs {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(s.maker_fee_bps, -2);
        assert_eq!(s.taker_fee_bps, 5);
    }

    #[test]
    fn test_underlying_fee_existing_expiration_unaffected() {
        let book = UnderlyingOrderBook::new("BTC");

        // Create expiration BEFORE setting fee schedule
        let exp_before = book.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(30.0)));

        book.set_fee_schedule(FeeSchedule::new(-2, 5));

        // Existing expiration's new strikes do NOT get fee schedule
        let strike_old = exp_before.get_or_create_strike(50000);
        assert!(strike_old.call().fee_schedule().is_none());

        // New expiration DOES get fee schedule
        let exp_after = book.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(60.0)));
        let strike_new = exp_after.get_or_create_strike(50000);
        let fs = strike_new.call().fee_schedule();
        assert!(fs.is_some());
        let fs = match fs {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(fs.taker_fee_bps, 5);
    }

    #[test]
    fn test_manager_fee_schedule_propagates_through_full_hierarchy() {
        let manager = UnderlyingOrderBookManager::new();
        manager.set_fee_schedule(FeeSchedule::new(-3, 8));

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        let call_fs = match strike.call().fee_schedule() {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(call_fs.maker_fee_bps, -3);
        assert_eq!(call_fs.taker_fee_bps, 8);

        let put_fs = match strike.put().fee_schedule() {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(put_fs.maker_fee_bps, -3);
        assert_eq!(put_fs.taker_fee_bps, 8);
    }

    #[test]
    fn test_manager_fee_schedule_default_is_none() {
        let manager = UnderlyingOrderBookManager::new();
        assert!(manager.fee_schedule().is_none());
    }

    #[test]
    fn test_manager_fee_existing_underlying_unaffected() {
        let manager = UnderlyingOrderBookManager::new();

        // Create underlying BEFORE setting fee schedule
        let btc = manager.get_or_create("BTC");

        manager.set_fee_schedule(FeeSchedule::new(-2, 5));

        // Existing underlying's new expirations do NOT get fee schedule
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        assert!(strike.call().fee_schedule().is_none());

        // New underlying DOES get fee schedule
        let eth = manager.get_or_create("ETH");
        let exp2 = eth.get_or_create_expiration(test_expiration());
        let strike2 = exp2.get_or_create_strike(50000);
        let fs2 = match strike2.call().fee_schedule() {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(fs2.taker_fee_bps, 5);
    }

    #[test]
    fn test_different_fee_schedules_per_underlying() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        btc.set_fee_schedule(FeeSchedule::new(-1, 3));

        let eth = manager.get_or_create("ETH");
        eth.set_fee_schedule(FeeSchedule::new(-5, 10));

        let btc_exp = btc.get_or_create_expiration(test_expiration());
        let btc_strike = btc_exp.get_or_create_strike(50000);

        let eth_exp = eth.get_or_create_expiration(test_expiration());
        let eth_strike = eth_exp.get_or_create_strike(3000);

        let btc_fs = match btc_strike.call().fee_schedule() {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(btc_fs.maker_fee_bps, -1);
        assert_eq!(btc_fs.taker_fee_bps, 3);

        let eth_fs = match eth_strike.call().fee_schedule() {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(eth_fs.maker_fee_bps, -5);
        assert_eq!(eth_fs.taker_fee_bps, 10);
    }

    #[test]
    fn test_fee_schedule_coexists_with_stp() {
        let manager = UnderlyingOrderBookManager::new();
        manager.set_stp_mode(STPMode::CancelTaker);
        manager.set_fee_schedule(FeeSchedule::new(-2, 5));

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        assert_eq!(strike.call().stp_mode(), STPMode::CancelTaker);
        let fs = match strike.call().fee_schedule() {
            Some(s) => s,
            None => panic!("expected fee schedule"),
        };
        assert_eq!(fs.maker_fee_bps, -2);
        assert_eq!(fs.taker_fee_bps, 5);
    }

    #[test]
    fn test_fee_full_order_through_hierarchy() {
        let manager = UnderlyingOrderBookManager::new();
        manager.set_fee_schedule(FeeSchedule::new(0, 5));

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);

        // Place resting sell, then aggressive buy via _full
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
        {
            panic!("add order failed: {}", err);
        }

        let result = match strike
            .call()
            .add_limit_order_full(OrderId::new(), Side::Buy, 100, 10)
        {
            Ok(r) => r,
            Err(err) => panic!("add order failed: {}", err),
        };

        // Trade executed
        assert_eq!(strike.call().order_count(), 0);
        // Result should have the correct symbol
        assert!(result.symbol.contains("BTC"));
    }

    // ── Order Lifecycle Tests ──────────────────────────────────────────────

    #[test]
    fn test_underlying_find_order() {
        let book = UnderlyingOrderBook::new("BTC");
        let order_id = OrderId::new();

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(order_id, Side::Buy, 100, 10)
            .expect("add order");
        drop(strike);
        drop(exp);

        let result = book.find_order(order_id);
        assert!(result.is_some());
    }

    #[test]
    fn test_underlying_find_order_not_found() {
        let book = UnderlyingOrderBook::new("BTC");
        let result = book.find_order(OrderId::new());
        assert!(result.is_none());
    }

    #[test]
    fn test_underlying_total_active_orders() {
        let book = UnderlyingOrderBook::new("BTC");

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add call");
        strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 80, 5)
            .expect("add put");
        drop(strike);
        drop(exp);

        assert_eq!(book.total_active_orders(), 2);
    }

    #[test]
    fn test_underlying_orders_by_user() {
        let book = UnderlyingOrderBook::new("BTC");
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
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
        drop(exp);

        let a_orders = book.orders_by_user(user_a);
        assert_eq!(a_orders.len(), 2);

        let b_orders = book.orders_by_user(user_b);
        assert_eq!(b_orders.len(), 1);
    }

    #[test]
    fn test_underlying_terminal_order_summary() {
        let book = UnderlyingOrderBook::new("BTC");

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        drop(strike);
        drop(exp);

        let summary = book.terminal_order_summary();
        assert_eq!(summary.filled, 2);
        assert_eq!(summary.total(), 2);
    }

    #[test]
    fn test_underlying_purge_terminal_states() {
        use std::thread;
        use std::time::Duration;

        let book = UnderlyingOrderBook::new("BTC");

        let exp = book.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        drop(strike);
        drop(exp);

        thread::sleep(Duration::from_millis(10));
        let purged = book.purge_terminal_states(Duration::from_millis(1));
        assert_eq!(purged, 2);
    }

    #[test]
    fn test_manager_find_order_across_underlyings() {
        let manager = UnderlyingOrderBookManager::new();
        let order_id = OrderId::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(order_id, Side::Buy, 100, 10)
            .expect("add order");
        drop(strike);
        drop(exp);
        drop(btc);

        let result = manager.find_order_across_underlyings(order_id);
        assert!(result.is_some());
    }

    #[test]
    fn test_manager_find_order_across_underlyings_not_found() {
        let manager = UnderlyingOrderBookManager::new();
        let result = manager.find_order_across_underlyings(OrderId::new());
        assert!(result.is_none());
    }

    #[test]
    fn test_manager_total_active_orders_across_underlyings() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add btc");
        drop(strike);
        drop(exp);
        drop(btc);

        let eth = manager.get_or_create("ETH");
        let exp2 = eth.get_or_create_expiration(test_expiration());
        let strike2 = exp2.get_or_create_strike(3000);
        strike2
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 200, 5)
            .expect("add eth");
        drop(strike2);
        drop(exp2);
        drop(eth);

        assert_eq!(manager.total_active_orders_across_underlyings(), 2);
    }

    #[test]
    fn test_manager_terminal_order_summary_across_underlyings() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");
        drop(strike);
        drop(exp);
        drop(btc);

        let summary = manager.terminal_order_summary_across_underlyings();
        assert_eq!(summary.filled, 2);
        assert_eq!(summary.total(), 2);
    }

    #[test]
    fn test_symbol_index_auto_registration() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        let call_symbol = strike.call().symbol().to_string();
        let put_symbol = strike.put().symbol().to_string();
        drop(strike);
        drop(exp);
        drop(btc);

        assert_eq!(manager.symbol_index().len(), 2);
        assert!(manager.symbol_index().contains(&call_symbol));
        assert!(manager.symbol_index().contains(&put_symbol));
    }

    #[test]
    fn test_get_by_symbol_call() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        let call_symbol = strike.call().symbol().to_string();
        drop(strike);
        drop(exp);
        drop(btc);

        let result = manager.get_by_symbol(&call_symbol);
        assert!(result.is_ok());
        let book = result.expect("should find call");
        assert_eq!(book.symbol(), call_symbol);
    }

    #[test]
    fn test_get_by_symbol_put() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        let put_symbol = strike.put().symbol().to_string();
        drop(strike);
        drop(exp);
        drop(btc);

        let result = manager.get_by_symbol(&put_symbol);
        assert!(result.is_ok());
        let book = result.expect("should find put");
        assert_eq!(book.symbol(), put_symbol);
    }

    #[test]
    fn test_get_by_symbol_not_found() {
        let manager = UnderlyingOrderBookManager::new();

        let result = manager.get_by_symbol("BTC-20260130-50000-C");
        assert!(result.is_err());
        match result {
            Err(err) => assert!(err.to_string().contains("symbol not found")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn test_symbol_index_deregistration_on_strike_remove() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let strike = exp.get_or_create_strike(50000);
        let call_symbol = strike.call().symbol().to_string();
        let put_symbol = strike.put().symbol().to_string();
        drop(strike);

        assert_eq!(manager.symbol_index().len(), 2);

        exp.chain().strikes().remove(50000);
        drop(exp);
        drop(btc);

        assert_eq!(manager.symbol_index().len(), 0);
        assert!(!manager.symbol_index().contains(&call_symbol));
        assert!(!manager.symbol_index().contains(&put_symbol));
    }

    #[test]
    fn test_symbol_index_multiple_strikes() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let s1 = exp.get_or_create_strike(45000);
        let s2 = exp.get_or_create_strike(50000);
        let s3 = exp.get_or_create_strike(55000);
        let s1_call = s1.call().symbol().to_string();
        let s2_put = s2.put().symbol().to_string();
        let s3_call = s3.call().symbol().to_string();
        drop(s1);
        drop(s2);
        drop(s3);
        drop(exp);
        drop(btc);

        assert_eq!(manager.symbol_index().len(), 6);
        assert!(manager.symbol_index().contains(&s1_call));
        assert!(manager.symbol_index().contains(&s2_put));
        assert!(manager.symbol_index().contains(&s3_call));
    }

    #[test]
    fn test_symbol_index_multiple_underlyings() {
        let manager = UnderlyingOrderBookManager::new();

        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(test_expiration());
        let btc_strike = exp.get_or_create_strike(50000);
        let btc_call = btc_strike.call().symbol().to_string();
        drop(btc_strike);
        drop(exp);
        drop(btc);

        let eth = manager.get_or_create("ETH");
        let exp2 = eth.get_or_create_expiration(test_expiration());
        let eth_strike = exp2.get_or_create_strike(3000);
        let eth_put = eth_strike.put().symbol().to_string();
        drop(eth_strike);
        drop(exp2);
        drop(eth);

        assert_eq!(manager.symbol_index().len(), 4);
        assert!(manager.symbol_index().contains(&btc_call));
        assert!(manager.symbol_index().contains(&eth_put));
    }
}
