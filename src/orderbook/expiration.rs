//! Expiration order book module.
//!
//! This module provides the [`ExpirationOrderBook`] and [`ExpirationOrderBookManager`]
//! for managing all expirations for a single underlying asset.

use super::chain::{ChainMassCancelResult, OptionChainOrderBook};
use super::contract_specs::{ContractSpecs, SharedContractSpecs};
use super::expiration_key::ExpirationKey;
use super::fees::SharedFeeSchedule;
use super::instrument_registry::InstrumentRegistry;
use super::stp::SharedSTPMode;
use super::strike::StrikeOrderBook;
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

    /// Returns the ATM strike closest to the given spot price.
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
    validation_config: SharedValidationConfig,
    /// Contract specs propagated to newly created expiration books.
    contract_specs: SharedContractSpecs,
    /// Instrument registry propagated to newly created expiration books.
    registry: Option<Arc<InstrumentRegistry>>,
    /// Symbol index for O(1) lookup by symbol string.
    symbol_index: Option<Arc<SymbolIndex>>,
    /// STP mode propagated to newly created expiration books.
    stp_mode: SharedSTPMode,
    /// Fee schedule propagated to newly created expiration books.
    fee_schedule: SharedFeeSchedule,
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
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: None,
            symbol_index: None,
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
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
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: Some(registry),
            symbol_index: None,
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
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
            validation_config: SharedValidationConfig::new(),
            contract_specs: SharedContractSpecs::new(),
            registry: Some(registry),
            symbol_index: Some(symbol_index),
            stp_mode: SharedSTPMode::new(),
            fee_schedule: SharedFeeSchedule::new(),
        }
    }

    /// Sets the contract specs for all future expirations created by this manager.
    ///
    /// Existing expiration books are not affected. Only newly created books
    /// via [`get_or_create`](Self::get_or_create) will have these specs propagated.
    pub fn set_specs(&self, specs: ContractSpecs) {
        self.contract_specs.set(specs);
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
        self.validation_config.set(config);
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
    /// is derived purely from the atomic insert result with no extra map probe.
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
        // Keep a handle to the locally built book so we can identify whether it
        // won the race after the atomic publish.
        let candidate = Arc::clone(&fresh);
        // Atomic, idempotent publish: first inserter wins and is never evicted.
        let entry = self.expirations.get_or_insert(key, fresh);
        // `inserted` is true iff the published value is the book we just built,
        // i.e. this call is the unique `get_or_insert` winner for this key.
        let inserted = Arc::ptr_eq(entry.value(), &candidate);
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
    /// Returns the number of chain books with cancelled orders.
    ///
    /// # Description
    ///
    /// Counts how many chain books recorded at least one cancelled order.
    ///
    /// # Arguments
    ///
    /// None.
    ///
    /// # Returns
    ///
    /// Number of chain books affected (books).
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
            .filter(|(_, result)| result.total_cancelled() > 0)
            .count()
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
}
