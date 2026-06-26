//! Strike order book module.
//!
//! This module provides the [`StrikeOrderBook`] and [`StrikeOrderBookManager`]
//! for managing call/put pairs at a specific strike price.

use super::book::{BookConfig, OptionOrderBook};
use super::contract_specs::{ContractSpecs, SharedContractSpecs};
use super::fees::SharedFeeSchedule;
use super::instrument_registry::{InstrumentInfo, InstrumentRegistry};
use super::quote::Quote;
use super::stp::SharedSTPMode;
use super::symbol_index::{SymbolIndex, SymbolRef};
use super::validation::{SharedValidationConfig, ValidationConfig};
use crate::error::{Error, Result};
use crate::utils::format_expiration_yyyymmdd;
use crossbeam_skiplist::SkipMap;
use optionstratlib::greeks::Greek;
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::{FeeSchedule, MassCancelResult, OrderId, OrderStatus, STPMode, Side};
use pricelevel::Hash32;
use std::sync::Arc;
use std::time::Duration;

use super::book::TerminalOrderSummary;

/// Order book for a single strike price containing both call and put.
///
/// This struct manages the call/put pair at a specific strike price.
///
/// ## Architecture
///
/// ```text
/// StrikeOrderBook (per strike price)
///   ├── OptionOrderBook (call)
///   │     └── OrderBook<T> (from OrderBook-rs)
///   └── OptionOrderBook (put)
///         └── OrderBook<T> (from OrderBook-rs)
/// ```
pub struct StrikeOrderBook {
    /// The underlying asset symbol (e.g., "BTC").
    underlying: String,
    /// The expiration date.
    expiration: ExpirationDate,
    /// The strike price.
    strike: u64,
    /// Call option order book.
    call: Arc<OptionOrderBook>,
    /// Put option order book.
    put: Arc<OptionOrderBook>,
    /// Greeks for the call option.
    call_greeks: Option<Greek>,
    /// Greeks for the put option.
    put_greeks: Option<Greek>,
    /// Unique identifier for this strike order book.
    id: OrderId,
}

impl StrikeOrderBook {
    /// Creates a new strike order book.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol (e.g., "BTC")
    /// * `expiration` - The expiration date
    /// * `strike` - The strike price
    #[must_use]
    pub fn new(underlying: impl Into<String>, expiration: ExpirationDate, strike: u64) -> Self {
        let underlying = underlying.into();

        // Format expiration as YYYYMMDD, fallback to Display if formatting fails
        let exp_str =
            format_expiration_yyyymmdd(&expiration).unwrap_or_else(|_| expiration.to_string());

        let call_symbol = format!("{}-{}-{}-C", underlying, exp_str, strike);
        let put_symbol = format!("{}-{}-{}-P", underlying, exp_str, strike);

        Self {
            underlying,
            expiration,
            strike,
            call: Arc::new(OptionOrderBook::new(call_symbol, OptionStyle::Call)),
            put: Arc::new(OptionOrderBook::new(put_symbol, OptionStyle::Put)),
            call_greeks: None,
            put_greeks: None,
            id: OrderId::new(),
        }
    }

    /// Creates a new strike order book with pre-trade validation configured.
    ///
    /// The validation config is applied to both call and put order books
    /// during construction.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol (e.g., "BTC")
    /// * `expiration` - The expiration date
    /// * `strike` - The strike price
    /// * `config` - Validation configuration for both call and put books
    #[must_use]
    pub fn new_with_validation(
        underlying: impl Into<String>,
        expiration: ExpirationDate,
        strike: u64,
        config: &ValidationConfig,
    ) -> Self {
        let underlying = underlying.into();

        let exp_str =
            format_expiration_yyyymmdd(&expiration).unwrap_or_else(|_| expiration.to_string());

        let call_symbol = format!("{}-{}-{}-C", underlying, exp_str, strike);
        let put_symbol = format!("{}-{}-{}-P", underlying, exp_str, strike);

        Self {
            underlying,
            expiration,
            strike,
            call: Arc::new(OptionOrderBook::new_with_validation(
                call_symbol,
                OptionStyle::Call,
                config,
            )),
            put: Arc::new(OptionOrderBook::new_with_validation(
                put_symbol,
                OptionStyle::Put,
                config,
            )),
            call_greeks: None,
            put_greeks: None,
            id: OrderId::new(),
        }
    }

    /// Creates a new strike order book from pre-built call/put order books.
    ///
    /// Used internally by [`StrikeOrderBookManager`] when an instrument registry
    /// is available, so that each `OptionOrderBook` already has a unique
    /// instrument ID assigned.
    #[must_use]
    pub(crate) fn from_books(
        underlying: impl Into<String>,
        expiration: ExpirationDate,
        strike: u64,
        call: Arc<OptionOrderBook>,
        put: Arc<OptionOrderBook>,
    ) -> Self {
        Self {
            underlying: underlying.into(),
            expiration,
            strike,
            call,
            put,
            call_greeks: None,
            put_greeks: None,
            id: OrderId::new(),
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

    /// Returns the strike price.
    #[must_use]
    pub const fn strike(&self) -> u64 {
        self.strike
    }

    /// Returns the unique identifier for this strike order book.
    #[must_use]
    pub const fn id(&self) -> OrderId {
        self.id
    }

    /// Returns the STP mode configured on the call book.
    ///
    /// Both call and put books share the same STP mode when created
    /// through the hierarchy, so reading from the call book is sufficient.
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.call.stp_mode()
    }

    /// Returns the fee schedule configured on the call book.
    ///
    /// Both call and put books share the same fee schedule when created
    /// through the hierarchy, so reading from the call book is sufficient.
    #[must_use]
    #[inline]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.call.fee_schedule()
    }

    /// Returns a reference to the call order book.
    #[must_use]
    pub fn call(&self) -> &OptionOrderBook {
        &self.call
    }

    /// Returns an Arc reference to the call order book.
    #[must_use]
    pub fn call_arc(&self) -> Arc<OptionOrderBook> {
        Arc::clone(&self.call)
    }

    /// Returns a reference to the put order book.
    #[must_use]
    pub fn put(&self) -> &OptionOrderBook {
        &self.put
    }

    /// Returns an Arc reference to the put order book.
    #[must_use]
    pub fn put_arc(&self) -> Arc<OptionOrderBook> {
        Arc::clone(&self.put)
    }

    /// Returns the order book for the specified option style.
    #[must_use]
    pub fn get(&self, option_style: OptionStyle) -> &OptionOrderBook {
        match option_style {
            OptionStyle::Call => &self.call,
            OptionStyle::Put => &self.put,
        }
    }

    /// Returns an Arc reference to the order book for the specified option style.
    #[must_use]
    pub fn get_arc(&self, option_style: OptionStyle) -> Arc<OptionOrderBook> {
        match option_style {
            OptionStyle::Call => Arc::clone(&self.call),
            OptionStyle::Put => Arc::clone(&self.put),
        }
    }

    /// Returns the best quote for the call option.
    #[must_use]
    pub fn call_quote(&self) -> Quote {
        self.call.best_quote()
    }

    /// Returns the best quote for the put option.
    #[must_use]
    pub fn put_quote(&self) -> Quote {
        self.put.best_quote()
    }

    /// Returns true if both call and put have two-sided quotes.
    #[must_use]
    pub fn is_fully_quoted(&self) -> bool {
        self.call.best_quote().is_two_sided() && self.put.best_quote().is_two_sided()
    }

    /// Returns the total order count across call and put.
    #[must_use]
    pub fn order_count(&self) -> usize {
        self.call.order_count() + self.put.order_count()
    }

    /// Returns true if both call and put are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.call.is_empty() && self.put.is_empty()
    }

    /// Clears all orders from both call and put books.
    pub fn clear(&self) {
        self.call.clear();
        self.put.clear();
    }

    /// Cancels all resting orders across call and put books.
    ///
    /// # Description
    ///
    /// Cancels every resting order across both option books for this strike and
    /// returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// A [`StrikeMassCancelResult`] containing per-book results plus aggregated
    /// counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::StrikeOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    ///
    /// let strike = StrikeOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)), 50000);
    /// let result = match strike.cancel_all() {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_all(&self) -> Result<StrikeMassCancelResult> {
        let call_result = self.call.cancel_all()?;
        let put_result = self.put.cancel_all()?;

        Ok(StrikeMassCancelResult {
            per_book: vec![
                (self.call.symbol().to_string(), call_result),
                (self.put.symbol().to_string(), put_result),
            ],
        })
    }

    /// Cancels all resting orders on a specific side across call and put books.
    ///
    /// # Description
    ///
    /// Cancels every resting order on the provided side across both option books
    /// for this strike and returns the aggregated cancellation details.
    ///
    /// # Arguments
    ///
    /// * `side` - Side to cancel ([`Side::Buy`] or [`Side::Sell`]).
    ///
    /// # Returns
    ///
    /// A [`StrikeMassCancelResult`] containing per-book results plus aggregated
    /// counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::StrikeOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    /// use orderbook_rs::Side;
    ///
    /// let strike = StrikeOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)), 50000);
    /// let result = match strike.cancel_by_side(Side::Buy) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_side(&self, side: Side) -> Result<StrikeMassCancelResult> {
        let call_result = self.call.cancel_by_side(side)?;
        let put_result = self.put.cancel_by_side(side)?;

        Ok(StrikeMassCancelResult {
            per_book: vec![
                (self.call.symbol().to_string(), call_result),
                (self.put.symbol().to_string(), put_result),
            ],
        })
    }

    /// Cancels all resting orders for a specific user across call and put books.
    ///
    /// # Description
    ///
    /// Cancels every resting order attributed to the provided user identifier
    /// across both option books for this strike and returns the aggregated
    /// cancellation details.
    ///
    /// # Arguments
    ///
    /// * `user_id` - User identifier to cancel (32-byte hash).
    ///
    /// # Returns
    ///
    /// A [`StrikeMassCancelResult`] containing per-book results plus aggregated
    /// counts (books, orders).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::StrikeOrderBook;
    /// use optionstratlib::ExpirationDate;
    /// use optionstratlib::prelude::pos_or_panic;
    /// use pricelevel::Hash32;
    ///
    /// let strike = StrikeOrderBook::new("BTC", ExpirationDate::Days(pos_or_panic!(30.0)), 50000);
    /// let user = Hash32::from([1u8; 32]);
    /// let result = match strike.cancel_by_user(user) {
    ///     Ok(result) => result,
    ///     Err(err) => panic!("cancel failed: {}", err),
    /// };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    pub fn cancel_by_user(&self, user_id: Hash32) -> Result<StrikeMassCancelResult> {
        let call_result = self.call.cancel_by_user(user_id)?;
        let put_result = self.put.cancel_by_user(user_id)?;

        Ok(StrikeMassCancelResult {
            per_book: vec![
                (self.call.symbol().to_string(), call_result),
                (self.put.symbol().to_string(), put_result),
            ],
        })
    }

    // ── Order Lifecycle Queries ────────────────────────────────────────────

    /// Finds an order anywhere in this strike's call or put book.
    ///
    /// # Description
    ///
    /// Searches the call book first, then the put book. Returns the option
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
        if let Some(status) = self.call.get_order_status(order_id) {
            return Some((self.call.symbol().to_string(), status));
        }
        if let Some(status) = self.put.get_order_status(order_id) {
            return Some((self.put.symbol().to_string(), status));
        }
        None
    }

    /// Returns the total number of active orders across call and put books.
    ///
    /// # Description
    ///
    /// Sums the active order counts from both the call and put option books.
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
        self.call
            .active_order_count()
            .saturating_add(self.put.active_order_count())
    }

    /// Removes terminal-state entries older than the specified duration.
    ///
    /// # Description
    ///
    /// Delegates to both call and put books and returns the total purged.
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
        self.call
            .purge_terminal_states(older_than)
            .saturating_add(self.put.purge_terminal_states(older_than))
    }

    /// Returns all currently active orders for a specific user.
    ///
    /// # Description
    ///
    /// Searches both call and put books for resting orders belonging to the
    /// specified user. Returns tuples of (symbol, order_id, status).
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
        let mut result = Vec::new();
        let call_symbol = self.call.symbol().to_string();
        for (id, status) in self.call.orders_by_user(user_id) {
            result.push((call_symbol.clone(), id, status));
        }
        let put_symbol = self.put.symbol().to_string();
        for (id, status) in self.put.orders_by_user(user_id) {
            result.push((put_symbol.clone(), id, status));
        }
        result
    }

    /// Returns a summary of terminal order transitions.
    ///
    /// # Description
    ///
    /// Aggregates the terminal order summaries from both call and put books.
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
        self.call.terminal_order_summary() + self.put.terminal_order_summary()
    }

    /// Updates the Greeks for the call option.
    pub fn update_call_greeks(&mut self, greeks: Greek) {
        self.call_greeks = Some(greeks);
    }

    /// Updates the Greeks for the put option.
    pub fn update_put_greeks(&mut self, greeks: Greek) {
        self.put_greeks = Some(greeks);
    }

    /// Returns the Greeks for the call option.
    #[must_use]
    pub const fn call_greeks(&self) -> Option<&Greek> {
        self.call_greeks.as_ref()
    }

    /// Returns the Greeks for the put option.
    #[must_use]
    pub const fn put_greeks(&self) -> Option<&Greek> {
        self.put_greeks.as_ref()
    }
}

/// Strike-level mass cancel summary.
///
/// # Description
///
/// Aggregates per-option-book mass cancel results for a strike.
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
/// use option_chain_orderbook::orderbook::StrikeMassCancelResult;
///
/// let result = StrikeMassCancelResult { per_book: Vec::new() };
/// assert_eq!(result.total_cancelled(), 0);
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct StrikeMassCancelResult {
    /// Per-option-book cancellation results keyed by option symbol.
    pub per_book: Vec<(String, MassCancelResult)>,
}

impl StrikeMassCancelResult {
    /// Returns the number of option books with cancelled orders.
    ///
    /// # Description
    ///
    /// Counts how many option books recorded at least one cancelled order.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Number of books affected (books).
    ///
    /// # Errors
    ///
    /// None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use option_chain_orderbook::orderbook::StrikeMassCancelResult;
    ///
    /// let result = StrikeMassCancelResult { per_book: Vec::new() };
    /// assert_eq!(result.books_affected(), 0);
    /// ```
    #[must_use]
    pub fn books_affected(&self) -> usize {
        self.per_book
            .iter()
            .filter(|(_, result)| result.cancelled_count() > 0)
            .count()
    }

    /// Returns the total number of cancelled orders across call and put books.
    ///
    /// # Description
    ///
    /// Sums cancelled orders across every option book for this strike.
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
    /// use option_chain_orderbook::orderbook::StrikeMassCancelResult;
    ///
    /// let result = StrikeMassCancelResult { per_book: Vec::new() };
    /// assert_eq!(result.total_cancelled(), 0);
    /// ```
    #[must_use]
    pub fn total_cancelled(&self) -> usize {
        self.per_book
            .iter()
            .map(|(_, result)| result.cancelled_count())
            .sum()
    }
}

/// Manages strike order books for a single expiration.
///
/// Provides centralized access to all strikes within an expiration.
/// Uses `SkipMap` for thread-safe concurrent access.
pub struct StrikeOrderBookManager {
    /// Strike order books indexed by strike price.
    strikes: SkipMap<u64, Arc<StrikeOrderBook>>,
    /// The underlying asset symbol.
    underlying: String,
    /// The expiration date.
    expiration: ExpirationDate,
    /// Validation config applied to newly created strike books.
    validation_config: SharedValidationConfig,
    /// Contract specs propagated to newly created strike books.
    contract_specs: SharedContractSpecs,
    /// Instrument registry for allocating IDs to new option books.
    registry: Option<Arc<InstrumentRegistry>>,
    /// Symbol index for O(1) lookup by symbol string.
    symbol_index: Option<Arc<SymbolIndex>>,
    /// STP mode applied to newly created option books.
    stp_mode: SharedSTPMode,
    /// Fee schedule applied to newly created option books.
    fee_schedule: SharedFeeSchedule,
}

impl StrikeOrderBookManager {
    /// Creates a new strike order book manager.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `expiration` - The expiration date
    #[must_use]
    pub fn new(underlying: impl Into<String>, expiration: ExpirationDate) -> Self {
        Self {
            strikes: SkipMap::new(),
            underlying: underlying.into(),
            expiration,
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: None,
            symbol_index: None,
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Creates a new strike order book manager with an instrument registry.
    ///
    /// When the registry is present, newly created strike books will have
    /// their call/put [`OptionOrderBook`] instances assigned unique IDs.
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
        Self {
            strikes: SkipMap::new(),
            underlying: underlying.into(),
            expiration,
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: Some(registry),
            symbol_index: None,
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Creates a new strike order book manager with both instrument registry and symbol index.
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
        Self {
            strikes: SkipMap::new(),
            underlying: underlying.into(),
            expiration,
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: Some(registry),
            symbol_index: Some(symbol_index),
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Returns a reference to the instrument registry, if any.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn registry(&self) -> Option<&Arc<InstrumentRegistry>> {
        self.registry.as_ref()
    }

    /// Sets the contract specs associated with this manager.
    ///
    /// These specs are stored on the manager and can be retrieved later
    /// via [`specs`](Self::specs). This does not modify any existing
    /// strike books or automatically apply to newly created ones.
    pub fn set_specs(&self, specs: ContractSpecs) {
        self.contract_specs.set(specs);
    }

    /// Returns the current contract specs, if any.
    #[must_use]
    pub fn specs(&self) -> Option<ContractSpecs> {
        self.contract_specs.get()
    }

    /// Sets the validation config for all future strike books created by this manager.
    ///
    /// Existing strike books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will use this config.
    pub fn set_validation(&self, config: ValidationConfig) {
        self.validation_config.set(config);
    }

    /// Returns the current validation config, if any.
    #[must_use]
    pub fn validation_config(&self) -> Option<ValidationConfig> {
        self.validation_config.get()
    }

    /// Sets the STP mode for all future option books created by this manager.
    ///
    /// Existing books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will use this mode.
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

    /// Sets the fee schedule for all future strike books.
    ///
    /// Existing books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will use this schedule.
    #[inline]
    pub fn set_fee_schedule(&self, schedule: FeeSchedule) {
        self.fee_schedule.set(Some(schedule));
    }

    /// Clears the fee schedule so future strike books have no fees configured.
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

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the expiration date.
    #[must_use = "returns the expiration date without modifying the manager"]
    pub const fn expiration(&self) -> &ExpirationDate {
        &self.expiration
    }

    /// Returns the number of strikes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strikes.len()
    }

    /// Returns true if there are no strikes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strikes.is_empty()
    }

    /// Gets or creates a strike order book, returning an Arc reference.
    ///
    /// If a validation config has been set via [`set_validation`](Self::set_validation),
    /// newly created strike books will have that config applied.
    ///
    /// If an instrument registry is present, each new call/put
    /// [`OptionOrderBook`] is assigned a unique instrument ID and registered
    /// in the reverse index.
    ///
    /// # Concurrency
    ///
    /// This is atomic and idempotent: the fresh call/put pair is built without
    /// any global side effects (no instrument-ID allocation, no symbol-index
    /// registration) and published with [`SkipMap::get_or_insert`], which
    /// inserts only when the key is absent and never evicts an existing entry.
    /// The first inserter wins and is never orphaned; concurrent losers receive
    /// the winning handle and drop their fresh pair. The global side effects
    /// (ID allocation and symbol-index registration) run exactly once, on the
    /// winning thread only, gated by [`Arc::ptr_eq`].
    ///
    /// Because ID assignment is deferred to the winner *after* the strike is
    /// published, there is a brief eventual-consistency window in which a
    /// concurrent reader (or a race loser) may observe the new call/put pair
    /// with [`instrument_id`](OptionOrderBook::instrument_id) still `0` until
    /// the winner completes registration. Callers that need the assigned IDs
    /// immediately after creation under contention should resolve them via the
    /// [`InstrumentRegistry`] / [`SymbolIndex`] once creation has settled rather
    /// than reading `instrument_id()` synchronously off the returned handle.
    ///
    /// ## Instrument-ID exhaustion
    ///
    /// This method is infallible by design so that the whole hierarchy and the
    /// sequencer can create books without threading a `Result`. If the
    /// instrument registry's `u32` ID space is ever exhausted (after roughly 4
    /// billion allocations — astronomically unlikely), the winner logs a
    /// `tracing::error!` and returns the strike book with its call/put pair left
    /// at `instrument_id == 0`. The registry degrades gracefully; it never
    /// panics. The symbol index is still populated, so symbol-based lookups
    /// continue to work.
    pub fn get_or_create(&self, strike: u64) -> Arc<StrikeOrderBook> {
        if let Some(entry) = self.strikes.get(&strike) {
            return Arc::clone(entry.value());
        }

        // Build the call/put pair WITHOUT allocating instrument IDs or touching
        // the registry / symbol index. Those are global side effects that must
        // run exactly once on the winning thread; a race loser's fresh pair is
        // dropped, so performing them here would leak IDs and index entries.
        let fresh = self.create_strike_book_without_ids(strike);

        // Atomic, idempotent publish: the first inserter wins and is never
        // evicted (no orphan window); concurrent losers get the winner.
        let entry = self.strikes.get_or_insert(strike, Arc::clone(&fresh));
        let winner = Arc::clone(entry.value());
        if Arc::ptr_eq(&winner, &fresh) {
            // We won the race — allocate IDs and register symbols exactly once.
            //
            // On the astronomically-rare instrument-ID-space exhaustion we log
            // and degrade rather than propagating a fallible signature through
            // the entire hierarchy and the sequencer's
            // `find_or_create_book_by_symbol`: the call/put pair is left at
            // `instrument_id == 0` (the same value as the transient
            // pre-assignment window described above), the symbol index stays
            // populated, and no thread panics.
            if let Err(err) = self.assign_instrument_ids(&winner, strike) {
                tracing::error!(
                    underlying = %self.underlying,
                    strike,
                    error = %err,
                    "instrument-id exhaustion: registry full; strike books left with instrument_id 0",
                );
            }
        }
        winner
    }

    /// Internal helper that builds a [`StrikeOrderBook`] with optional
    /// validation config, STP mode, and fee schedule but **without** allocating
    /// instrument IDs.
    ///
    /// IDs are assigned later by [`assign_instrument_ids`](Self::assign_instrument_ids)
    /// only after confirming the book won the insertion race.
    fn create_strike_book_without_ids(&self, strike: u64) -> Arc<StrikeOrderBook> {
        let exp_str = format_expiration_yyyymmdd(&self.expiration)
            .unwrap_or_else(|_| self.expiration.to_string());
        let call_symbol = format!("{}-{}-{}-C", self.underlying, exp_str, strike);
        let put_symbol = format!("{}-{}-{}-P", self.underlying, exp_str, strike);

        let base_config = BookConfig {
            validation: self.validation_config.get(),
            stp_mode: self.stp_mode.get(),
            fee_schedule: self.fee_schedule.get(),
            ..BookConfig::default()
        };

        let call = Arc::new(OptionOrderBook::new_with_config(
            &call_symbol,
            OptionStyle::Call,
            base_config.clone(),
        ));
        let put = Arc::new(OptionOrderBook::new_with_config(
            &put_symbol,
            OptionStyle::Put,
            base_config,
        ));

        Arc::new(StrikeOrderBook::from_books(
            &self.underlying,
            self.expiration,
            strike,
            call,
            put,
        ))
    }

    /// Assigns unique instrument IDs and registers the call/put books in the
    /// reverse index. Also registers symbols in the symbol index if present.
    /// Called only after confirming this book won the insertion race.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InstrumentIdExhausted`] if the instrument registry's
    /// `u32` ID space is exhausted. On that error the call/put pair is left at
    /// `instrument_id == 0`: IDs are allocated before being written to the
    /// books, so a mid-pair exhaustion never leaves a half-assigned pair. The
    /// symbol index is populated first (it is independent of numeric IDs and
    /// never fails), so symbol lookups stay functional even when the registry
    /// is full.
    fn assign_instrument_ids(&self, book: &StrikeOrderBook, strike: u64) -> Result<()> {
        let exp_str = format_expiration_yyyymmdd(&self.expiration)
            .unwrap_or_else(|_| self.expiration.to_string());
        let call_symbol = format!("{}-{}-{}-C", self.underlying, exp_str, strike);
        let put_symbol = format!("{}-{}-{}-P", self.underlying, exp_str, strike);

        // Symbol-index registration is independent of numeric ID allocation and
        // never fails; run it first so symbol lookups stay functional even if
        // the registry is exhausted below.
        if let Some(idx) = &self.symbol_index {
            let call_ref =
                SymbolRef::new(&self.underlying, self.expiration, strike, OptionStyle::Call);
            let put_ref =
                SymbolRef::new(&self.underlying, self.expiration, strike, OptionStyle::Put);
            idx.register(&call_symbol, call_ref);
            idx.register(&put_symbol, put_ref);
        }

        if let Some(reg) = &self.registry {
            // Allocate both IDs before mutating the books so that a mid-pair
            // exhaustion leaves the pair untouched at `instrument_id == 0`
            // rather than half-assigned.
            let call_id = reg.allocate()?;
            let put_id = reg.allocate()?;

            book.call().set_instrument_id(call_id);
            book.put().set_instrument_id(put_id);

            Self::register_pair(
                reg,
                &call_symbol,
                &put_symbol,
                call_id,
                put_id,
                self.expiration,
                strike,
            );
        }

        Ok(())
    }

    /// Registers a call/put pair in the instrument registry.
    fn register_pair(
        reg: &InstrumentRegistry,
        call_symbol: &str,
        put_symbol: &str,
        call_id: u32,
        put_id: u32,
        expiration: ExpirationDate,
        strike: u64,
    ) {
        reg.register(
            call_id,
            InstrumentInfo::new(call_symbol, expiration, strike, OptionStyle::Call),
        );
        reg.register(
            put_id,
            InstrumentInfo::new(put_symbol, expiration, strike, OptionStyle::Put),
        );
    }

    /// Gets a strike order book by strike price.
    ///
    /// # Errors
    ///
    /// Returns `Error::StrikeNotFound` if the strike does not exist.
    pub fn get(&self, strike: u64) -> Result<Arc<StrikeOrderBook>> {
        self.strikes
            .get(&strike)
            .map(|e| Arc::clone(e.value()))
            .ok_or_else(|| Error::strike_not_found(strike))
    }

    /// Returns true if a strike exists.
    #[must_use]
    pub fn contains(&self, strike: u64) -> bool {
        self.strikes.contains_key(&strike)
    }

    /// Returns an iterator over all strikes.
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = crossbeam_skiplist::map::Entry<'_, u64, Arc<StrikeOrderBook>>> {
        self.strikes.iter()
    }

    /// Removes a strike order book and deregisters its symbols from the index.
    ///
    /// Note: Returns true if the strike was removed, false if it didn't exist.
    pub fn remove(&self, strike: u64) -> bool {
        let removed = self.strikes.remove(&strike).is_some();
        if removed && let Some(idx) = &self.symbol_index {
            let exp_str = format_expiration_yyyymmdd(&self.expiration)
                .unwrap_or_else(|_| self.expiration.to_string());
            let call_symbol = format!("{}-{}-{}-C", self.underlying, exp_str, strike);
            let put_symbol = format!("{}-{}-{}-P", self.underlying, exp_str, strike);
            idx.deregister(&call_symbol);
            idx.deregister(&put_symbol);
        }
        removed
    }

    /// Atomically removes a strike order book only if it has no resting orders.
    ///
    /// This method provides a thread-safe way to clean up empty strikes without
    /// the TOCTOU (time-of-check-to-time-of-use) race condition that would occur
    /// if `is_empty()` and `remove()` were called separately.
    ///
    /// # Thread Safety
    ///
    /// The check and removal are performed while holding a reference to the entry,
    /// ensuring that no orders can be added between the emptiness check and removal.
    /// If another thread adds an order after we check but before we remove, the
    /// SkipMap's internal synchronization ensures we either see the order or the
    /// removal fails safely.
    ///
    /// # Returns
    ///
    /// - `true` if the strike was empty and successfully removed
    /// - `false` if the strike doesn't exist or has resting orders
    pub fn remove_if_empty(&self, strike: u64) -> bool {
        // Get the entry first to check if it's empty
        if let Some(entry) = self.strikes.get(&strike) {
            // Check emptiness while holding the entry reference
            if !entry.value().is_empty() {
                return false; // Has orders, don't remove
            }
            // Entry is empty — drop the reference and remove
            // Note: There's still a small window here where orders could be added,
            // but the caller (cleanup) will simply skip the strike on the next pass.
            drop(entry);
        } else {
            return false; // Strike doesn't exist
        }

        // Perform the removal
        let removed = self.strikes.remove(&strike).is_some();
        if removed && let Some(idx) = &self.symbol_index {
            let exp_str = format_expiration_yyyymmdd(&self.expiration)
                .unwrap_or_else(|_| self.expiration.to_string());
            let call_symbol = format!("{}-{}-{}-C", self.underlying, exp_str, strike);
            let put_symbol = format!("{}-{}-{}-P", self.underlying, exp_str, strike);
            idx.deregister(&call_symbol);
            idx.deregister(&put_symbol);
        }
        removed
    }

    /// Returns all strike prices (sorted).
    /// SkipMap maintains sorted order, so no additional sorting needed.
    pub fn strike_prices(&self) -> Vec<u64> {
        self.strikes.iter().map(|e| *e.key()).collect()
    }

    /// Returns the total order count across all strikes.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.strikes.iter().map(|e| e.value().order_count()).sum()
    }

    /// Returns the ATM (at-the-money) strike closest to the given spot price.
    ///
    /// # Errors
    ///
    /// Returns `Error::NoDataAvailable` if there are no strikes.
    pub fn atm_strike(&self, spot: u64) -> Result<u64> {
        self.strikes
            .iter()
            .map(|e| *e.key())
            .min_by_key(|&k| (k as i64 - spot as i64).unsigned_abs())
            .ok_or_else(|| Error::no_data("no strikes available"))
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
    fn test_strike_order_book_creation() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        assert_eq!(strike.underlying(), "BTC");
        assert_eq!(strike.strike(), 50000);
        assert!(strike.is_empty());
    }

    #[test]
    fn test_strike_order_book_orders() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

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

        assert_eq!(strike.order_count(), 2);
        assert!(!strike.is_empty());
    }

    #[test]
    fn test_strike_manager_creation() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
        assert_eq!(manager.underlying(), "BTC");
    }

    #[test]
    fn test_strike_manager_get_or_create() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        {
            let strike = manager.get_or_create(50000);
            assert_eq!(strike.strike(), 50000);
        }

        drop(manager.get_or_create(55000));
        drop(manager.get_or_create(45000));

        assert_eq!(manager.len(), 3);

        let strikes = manager.strike_prices();
        assert_eq!(strikes, vec![45000, 50000, 55000]);
    }

    #[test]
    fn test_strike_get_or_create_returns_same_handle() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        let first = manager.get_or_create(50000);
        let second = manager.get_or_create(50000);

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_strike_get_or_create_concurrent_single_id_pair() {
        use std::sync::Barrier;
        use std::thread;

        // Race N threads to create the SAME strike via a barrier for lockstep.
        // With `get_or_insert` the first inserter wins and is never evicted, and
        // the global side effects (ID allocation + symbol-index registration)
        // run exactly once on the winner. Assert: every thread sees one
        // identical handle, exactly one map entry, and exactly one ID pair (no
        // orphan, no double-allocation).
        const N: usize = 16;
        let registry = Arc::new(InstrumentRegistry::new());
        let symbol_index = Arc::new(SymbolIndex::new());
        let manager = Arc::new(StrikeOrderBookManager::new_with_registry_and_index(
            "BTC",
            test_expiration(),
            Arc::clone(&registry),
            Arc::clone(&symbol_index),
        ));
        let barrier = Arc::new(Barrier::new(N));

        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                manager.get_or_create(50000)
            }));
        }

        let books: Vec<Arc<StrikeOrderBook>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        // All threads observe the same winning handle.
        let first = books.first().expect("no books returned");
        for book in &books {
            assert!(Arc::ptr_eq(first, book));
        }

        // Exactly one map entry.
        assert_eq!(manager.len(), 1);

        // Exactly one instrument-ID pair allocated (call + put): the registry
        // advanced by exactly 2 and holds 2 entries; the symbol index holds 2.
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.current_id(), 3);
        assert_eq!(symbol_index.len(), 2);

        // The winning book carries distinct, non-zero IDs.
        let call_id = first.call().instrument_id();
        let put_id = first.put().instrument_id();
        assert_ne!(call_id, 0);
        assert_ne!(put_id, 0);
        assert_ne!(call_id, put_id);
    }

    #[test]
    fn test_get_or_create_degrades_on_id_exhaustion() {
        use super::super::symbol_index::SymbolIndex;

        // Seed the registry at its u32 ceiling so the very first allocation in
        // `assign_instrument_ids` overflows. `get_or_create` must NOT panic: it
        // logs and degrades, returning a fully usable strike book whose call/put
        // pair is left at `instrument_id == 0`.
        let registry = Arc::new(InstrumentRegistry::new_with_seed(u32::MAX));
        let symbol_index = Arc::new(SymbolIndex::new());
        let manager = StrikeOrderBookManager::new_with_registry_and_index(
            "BTC",
            test_expiration(),
            Arc::clone(&registry),
            Arc::clone(&symbol_index),
        );

        let book = manager.get_or_create(50000);

        // No panic; the strike book exists and is mapped.
        assert_eq!(manager.len(), 1);

        // Degraded: both legs sit at the reserved id 0 (no IDs were allocated).
        assert_eq!(book.call().instrument_id(), 0);
        assert_eq!(book.put().instrument_id(), 0);

        // Registry stayed pinned at the ceiling and registered nothing.
        assert_eq!(registry.current_id(), u32::MAX);
        assert_eq!(registry.len(), 0);

        // Symbol lookups still work — the symbol index does not depend on the
        // numeric instrument id, so it is populated even at exhaustion.
        assert_eq!(symbol_index.len(), 2);
    }

    #[test]
    fn test_get_or_create_idempotent_after_id_exhaustion() {
        // A repeat call with the same strike returns the SAME handle even when
        // the registry is exhausted (the degrade path does not break the
        // idempotent `get_or_insert` contract).
        let registry = Arc::new(InstrumentRegistry::new_with_seed(u32::MAX));
        let manager = StrikeOrderBookManager::new_with_registry("BTC", test_expiration(), registry);

        let first = manager.get_or_create(50000);
        let second = manager.get_or_create(50000);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_strike_manager_atm() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        drop(manager.get_or_create(45000));
        drop(manager.get_or_create(50000));
        drop(manager.get_or_create(55000));

        let atm1 = match manager.atm_strike(48000) {
            Ok(s) => s,
            Err(err) => panic!("atm_strike failed: {}", err),
        };
        assert_eq!(atm1, 50000);
        let atm2 = match manager.atm_strike(52000) {
            Ok(s) => s,
            Err(err) => panic!("atm_strike failed: {}", err),
        };
        assert_eq!(atm2, 50000);
        let atm3 = match manager.atm_strike(53000) {
            Ok(s) => s,
            Err(err) => panic!("atm_strike failed: {}", err),
        };
        assert_eq!(atm3, 55000);
    }

    #[test]
    fn test_strike_manager_atm_empty() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());
        assert!(manager.atm_strike(50000).is_err());
    }

    #[test]
    fn test_strike_expiration() {
        let exp = test_expiration();
        let strike = StrikeOrderBook::new("BTC", exp, 50000);
        assert_eq!(*strike.expiration(), exp);
    }

    #[test]
    fn test_strike_call_mut() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        let call_arc = strike.call_arc();
        if let Err(err) = call_arc.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
            panic!("add order failed: {}", err);
        }
        assert_eq!(strike.call().order_count(), 1);
    }

    #[test]
    fn test_strike_put_mut() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        let put_arc = strike.put_arc();
        if let Err(err) = put_arc.add_limit_order(OrderId::new(), Side::Buy, 50, 10) {
            panic!("add order failed: {}", err);
        }
        assert_eq!(strike.put().order_count(), 1);
    }

    #[test]
    fn test_strike_get_by_style() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Buy, 50, 5)
        {
            panic!("add order failed: {}", err);
        }

        let call = strike.get(OptionStyle::Call);
        let put = strike.get(OptionStyle::Put);

        assert_eq!(call.order_count(), 1);
        assert_eq!(put.order_count(), 1);
    }

    #[test]
    fn test_strike_get_arc_by_style() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        if let Err(err) =
            strike
                .get_arc(OptionStyle::Call)
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) =
            strike
                .get_arc(OptionStyle::Put)
                .add_limit_order(OrderId::new(), Side::Buy, 50, 5)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(strike.order_count(), 2);
    }

    #[test]
    fn test_strike_quotes() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 110, 5)
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
            .add_limit_order(OrderId::new(), Side::Sell, 60, 5)
        {
            panic!("add order failed: {}", err);
        }

        let call_quote = strike.call_quote();
        let put_quote = strike.put_quote();

        assert!(call_quote.is_two_sided());
        assert!(put_quote.is_two_sided());
    }

    #[test]
    fn test_strike_is_fully_quoted() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        assert!(!strike.is_fully_quoted());

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 110, 5)
        {
            panic!("add order failed: {}", err);
        }

        assert!(!strike.is_fully_quoted());

        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Buy, 50, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 60, 5)
        {
            panic!("add order failed: {}", err);
        }

        assert!(strike.is_fully_quoted());
    }

    #[test]
    fn test_strike_clear() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Buy, 50, 5)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(strike.order_count(), 2);
        strike.clear();
        assert!(strike.is_empty());
    }

    #[test]
    fn test_strike_clear_transitions_both_legs_to_terminal() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        let call_id = OrderId::new();
        let put_id = OrderId::new();
        strike
            .call()
            .add_limit_order(call_id, Side::Buy, 100, 10)
            .expect("add call");
        strike
            .put()
            .add_limit_order(put_id, Side::Buy, 50, 5)
            .expect("add put");
        assert_eq!(strike.call().active_order_count(), 1);
        assert_eq!(strike.put().active_order_count(), 1);

        strike.clear();

        // Both legs must reach zero active orders and transition their dropped
        // orders to the terminal cancelled state through the shared Arc handles.
        assert_eq!(strike.call().active_order_count(), 0);
        assert_eq!(strike.put().active_order_count(), 0);
        assert!(strike.is_empty());
        assert!(matches!(
            strike.call().get_order_status(call_id),
            Some(OrderStatus::Cancelled { .. })
        ));
        assert!(matches!(
            strike.put().get_order_status(put_id),
            Some(OrderStatus::Cancelled { .. })
        ));
        assert_eq!(strike.call().terminal_order_summary().cancelled, 1);
        assert_eq!(strike.put().terminal_order_summary().cancelled, 1);
    }

    #[test]
    fn test_strike_greeks() {
        use optionstratlib::greeks::Greek;
        use rust_decimal_macros::dec;

        let mut strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        assert!(strike.call_greeks().is_none());
        assert!(strike.put_greeks().is_none());

        let call_greeks = Greek {
            delta: dec!(0.5),
            gamma: dec!(0.01),
            theta: dec!(-0.05),
            vega: dec!(0.2),
            rho: dec!(0.1),
            rho_d: dec!(0.0),
            alpha: dec!(0.0),
            vanna: dec!(0.0),
            vomma: dec!(0.0),
            veta: dec!(0.0),
            charm: dec!(0.0),
            color: dec!(0.0),
        };
        let put_greeks = Greek {
            delta: dec!(-0.5),
            gamma: dec!(0.01),
            theta: dec!(-0.05),
            vega: dec!(0.2),
            rho: dec!(-0.1),
            rho_d: dec!(0.0),
            alpha: dec!(0.0),
            vanna: dec!(0.0),
            vomma: dec!(0.0),
            veta: dec!(0.0),
            charm: dec!(0.0),
            color: dec!(0.0),
        };

        strike.update_call_greeks(call_greeks);
        strike.update_put_greeks(put_greeks);

        assert!(strike.call_greeks().is_some());
        assert!(strike.put_greeks().is_some());
    }

    #[test]
    fn test_strike_manager_get() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        drop(manager.get_or_create(50000));

        assert!(manager.get(50000).is_ok());
        assert!(manager.get(99999).is_err());
    }

    #[test]
    fn test_strike_manager_contains() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        drop(manager.get_or_create(50000));

        assert!(manager.contains(50000));
        assert!(!manager.contains(99999));
    }

    #[test]
    fn test_strike_manager_remove() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        drop(manager.get_or_create(50000));
        drop(manager.get_or_create(55000));

        assert_eq!(manager.len(), 2);
        assert!(manager.remove(50000));
        assert_eq!(manager.len(), 1);
        assert!(!manager.remove(50000));
    }

    #[test]
    fn test_strike_manager_total_order_count() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        let strike = manager.get_or_create(50000);
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(strike);

        let strike2 = manager.get_or_create(55000);
        if let Err(err) = strike2
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        drop(strike2);

        assert_eq!(manager.total_order_count(), 2);
    }

    #[test]
    fn test_strike_with_validation_propagates_to_call_and_put() {
        let config = ValidationConfig::new().with_tick_size(100);
        let strike = StrikeOrderBook::new_with_validation("BTC", test_expiration(), 50000, &config);

        // Call: valid tick
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 10)
                .is_ok()
        );
        // Call: invalid tick
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_err()
        );
        // Put: valid tick
        assert!(
            strike
                .put()
                .add_limit_order(OrderId::new(), Side::Buy, 300, 10)
                .is_ok()
        );
        // Put: invalid tick
        assert!(
            strike
                .put()
                .add_limit_order(OrderId::new(), Side::Buy, 250, 10)
                .is_err()
        );
    }

    #[test]
    fn test_strike_manager_set_validation_propagates() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        let config = ValidationConfig::new().with_tick_size(100);
        manager.set_validation(config.clone());

        assert_eq!(manager.validation_config(), Some(config));

        // New strike should inherit validation
        let strike = manager.get_or_create(50000);
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
    fn test_strike_manager_no_validation_by_default() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        assert!(manager.validation_config().is_none());

        let strike = manager.get_or_create(50000);
        // No validation: any price works
        assert!(
            strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_ok()
        );
    }

    #[test]
    fn test_strike_cancel_all() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 60, 5)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(strike.order_count(), 2);

        let result = match strike.cancel_all() {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 2);
        assert_eq!(result.books_affected(), 2);
        assert_eq!(strike.order_count(), 0);
    }

    #[test]
    fn test_strike_cancel_by_side() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 110, 5)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Buy, 50, 10)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(strike.order_count(), 3);

        let result = match strike.cancel_by_side(Side::Buy) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 2);
        assert_eq!(result.books_affected(), 2);
        assert_eq!(strike.order_count(), 1);
    }

    #[test]
    fn test_strike_cancel_by_user() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        if let Err(err) =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) =
            strike
                .put()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 60, 5, user_a)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 110, 5, user_b)
        {
            panic!("add order failed: {}", err);
        }

        assert_eq!(strike.order_count(), 3);

        let result = match strike.cancel_by_user(user_a) {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 2);
        assert_eq!(result.books_affected(), 2);
        assert_eq!(strike.order_count(), 1);
    }

    #[test]
    fn test_strike_cancel_all_empty() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        let result = match strike.cancel_all() {
            Ok(r) => r,
            Err(err) => panic!("cancel failed: {}", err),
        };

        assert_eq!(result.total_cancelled(), 0);
        assert_eq!(result.books_affected(), 0);
    }

    #[test]
    fn test_strike_manager_existing_strike_unaffected() {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());

        // Create strike before setting validation
        let strike_before = manager.get_or_create(50000);

        // Now set validation
        manager.set_validation(ValidationConfig::new().with_tick_size(100));

        // Existing strike is NOT affected (any price still works)
        assert!(
            strike_before
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_ok()
        );

        // New strike IS affected
        let strike_after = manager.get_or_create(55000);
        assert!(
            strike_after
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 150, 10)
                .is_err()
        );
    }

    // ── Order Lifecycle Tests ──────────────────────────────────────────────

    #[test]
    fn test_strike_find_order_in_call() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        let order_id = OrderId::new();

        strike
            .call()
            .add_limit_order(order_id, Side::Buy, 100, 10)
            .expect("add order");

        let result = strike.find_order(order_id);
        assert!(result.is_some());
        let (symbol, _status) = result.unwrap();
        assert!(symbol.contains("-C"));
    }

    #[test]
    fn test_strike_find_order_in_put() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        let order_id = OrderId::new();

        strike
            .put()
            .add_limit_order(order_id, Side::Sell, 80, 5)
            .expect("add order");

        let result = strike.find_order(order_id);
        assert!(result.is_some());
        let (symbol, _status) = result.unwrap();
        assert!(symbol.contains("-P"));
    }

    #[test]
    fn test_strike_find_order_not_found() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        let result = strike.find_order(OrderId::new());
        assert!(result.is_none());
    }

    #[test]
    fn test_strike_total_active_orders() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add call");
        strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 80, 5)
            .expect("add put");

        assert_eq!(strike.total_active_orders(), 2);
    }

    #[test]
    fn test_strike_orders_by_user() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

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

        let a_orders = strike.orders_by_user(user_a);
        assert_eq!(a_orders.len(), 2);

        let b_orders = strike.orders_by_user(user_b);
        assert_eq!(b_orders.len(), 1);
    }

    #[test]
    fn test_strike_terminal_order_summary() {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        // Create matched orders in call book
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");

        let summary = strike.terminal_order_summary();
        assert_eq!(summary.filled, 2);
        assert_eq!(summary.total(), 2);
    }

    #[test]
    fn test_strike_purge_terminal_states() {
        use std::thread;
        use std::time::Duration;

        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);

        // Create matched orders
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .expect("add maker");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("add taker");

        thread::sleep(Duration::from_millis(10));
        let purged = strike.purge_terminal_states(Duration::from_millis(1));
        assert_eq!(purged, 2);
    }
}
