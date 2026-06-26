//! Symbol index module.
//!
//! This module provides [`SymbolIndex`] for O(1) lookup of any
//! [`OptionOrderBook`](super::book::OptionOrderBook) by its symbol string.
//!
//! The index is owned by
//! [`UnderlyingOrderBookManager`](super::underlying::UnderlyingOrderBookManager)
//! and propagated as `Arc<SymbolIndex>` through the hierarchy. When a new
//! [`StrikeOrderBook`](super::strike::StrikeOrderBook) is created, both call
//! and put symbols are automatically registered.

use dashmap::DashMap;
use optionstratlib::{ExpirationDate, OptionStyle};

/// Reference to locate an [`OptionOrderBook`](super::book::OptionOrderBook)
/// within the hierarchy.
///
/// Contains the coordinates needed to traverse the hierarchy and retrieve
/// the target order book: underlying, expiration, strike, and option style.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolRef {
    /// The underlying asset symbol (e.g., "BTC").
    underlying: String,
    /// The expiration date.
    expiration: ExpirationDate,
    /// The strike price.
    strike: u64,
    /// The option style (Call or Put).
    option_style: OptionStyle,
}

impl SymbolRef {
    /// Creates a new symbol reference.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `expiration` - The expiration date
    /// * `strike` - The strike price
    /// * `option_style` - Call or Put
    #[must_use]
    pub fn new(
        underlying: impl Into<String>,
        expiration: ExpirationDate,
        strike: u64,
        option_style: OptionStyle,
    ) -> Self {
        Self {
            underlying: underlying.into(),
            expiration,
            strike,
            option_style,
        }
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    #[inline]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the expiration date.
    // No `#[must_use]`: the return type `ExpirationDate` is itself `#[must_use]`,
    // so a function-level attribute would be a `clippy::double_must_use` error.
    #[inline]
    pub const fn expiration(&self) -> &ExpirationDate {
        &self.expiration
    }

    /// Returns the strike price.
    #[must_use]
    #[inline]
    pub const fn strike(&self) -> u64 {
        self.strike
    }

    /// Returns the option style (Call or Put).
    #[must_use]
    #[inline]
    pub const fn option_style(&self) -> OptionStyle {
        self.option_style
    }
}

impl std::fmt::Display for SymbolRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{:?}:{}:{:?}",
            self.underlying, self.expiration, self.strike, self.option_style
        )
    }
}

/// Thread-safe symbol-to-book index using [`DashMap`].
///
/// Provides O(1) lookup of symbol references by their string representation.
/// The index is automatically populated when strikes are created through
/// the hierarchy and cleaned up when strikes are removed.
///
/// ## Thread Safety
///
/// Uses [`DashMap`] for concurrent reads and writes via sharded locking.
/// Multiple threads can safely register, deregister, and look up symbols
/// simultaneously.
///
/// ## Example
///
/// ```rust
/// use option_chain_orderbook::orderbook::symbol_index::{SymbolIndex, SymbolRef};
/// use optionstratlib::{ExpirationDate, OptionStyle};
/// use optionstratlib::prelude::pos_or_panic;
///
/// let index = SymbolIndex::new();
/// let sym_ref = SymbolRef::new(
///     "BTC",
///     ExpirationDate::Days(pos_or_panic!(30.0)),
///     50000,
///     OptionStyle::Call,
/// );
/// index.register("BTC-20260130-50000-C", sym_ref.clone());
///
/// assert!(index.get("BTC-20260130-50000-C").is_some());
/// assert!(index.get("ETH-20260130-3000-P").is_none());
/// ```
pub struct SymbolIndex {
    /// Symbol string → SymbolRef mapping.
    index: DashMap<String, SymbolRef>,
}

impl SymbolIndex {
    /// Creates a new empty symbol index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            index: DashMap::new(),
        }
    }

    /// Registers a symbol in the index.
    ///
    /// If the symbol already exists, it is overwritten and this method
    /// returns `true` to indicate a duplicate registration occurred.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The full option symbol (e.g., "BTC-20260130-50000-C")
    /// * `sym_ref` - The symbol reference for hierarchy traversal
    ///
    /// # Returns
    ///
    /// `true` if the symbol was already present (overwritten), `false` if new.
    pub fn register(&self, symbol: impl Into<String>, sym_ref: SymbolRef) -> bool {
        self.index.insert(symbol.into(), sym_ref).is_some()
    }

    /// Deregisters a symbol from the index.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The symbol to remove
    ///
    /// # Returns
    ///
    /// `true` if the symbol was present and removed, `false` otherwise.
    pub fn deregister(&self, symbol: &str) -> bool {
        self.index.remove(symbol).is_some()
    }

    /// Looks up a symbol reference by its string representation.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The symbol to look up
    ///
    /// # Returns
    ///
    /// The symbol reference if found, `None` otherwise.
    #[must_use]
    #[inline]
    pub fn get(&self, symbol: &str) -> Option<SymbolRef> {
        self.index.get(symbol).map(|entry| entry.value().clone())
    }

    /// Returns `true` if the symbol is registered.
    #[must_use]
    #[inline]
    pub fn contains(&self, symbol: &str) -> bool {
        self.index.contains_key(symbol)
    }

    /// Returns the number of registered symbols.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Returns `true` if no symbols are registered.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Returns an iterator over all registered symbols.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        self.index.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Returns a snapshot of all registered `(symbol, SymbolRef)` pairs.
    ///
    /// The returned `Vec` is a point-in-time snapshot — concurrent
    /// registrations that occur after the call begins may or may not
    /// be included. The order of entries is arbitrary.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use option_chain_orderbook::orderbook::symbol_index::{SymbolIndex, SymbolRef};
    /// use optionstratlib::{ExpirationDate, OptionStyle};
    /// use optionstratlib::prelude::pos_or_panic;
    ///
    /// let index = SymbolIndex::new();
    /// let sym_ref = SymbolRef::new(
    ///     "BTC",
    ///     ExpirationDate::Days(pos_or_panic!(30.0)),
    ///     50000,
    ///     OptionStyle::Call,
    /// );
    /// index.register("BTC-20260130-50000-C", sym_ref);
    ///
    /// let entries = index.entries();
    /// assert_eq!(entries.len(), 1);
    /// assert_eq!(entries[0].0, "BTC-20260130-50000-C");
    /// ```
    #[must_use]
    pub fn entries(&self) -> Vec<(String, SymbolRef)> {
        self.index
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optionstratlib::prelude::pos_or_panic;

    fn test_expiration() -> ExpirationDate {
        ExpirationDate::Days(pos_or_panic!(30.0))
    }

    #[test]
    fn test_symbol_ref_new() {
        let sym_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);
        assert_eq!(sym_ref.underlying(), "BTC");
        assert_eq!(sym_ref.strike(), 50000);
        assert_eq!(sym_ref.option_style(), OptionStyle::Call);
    }

    #[test]
    fn test_symbol_ref_display() {
        let sym_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);
        let display = sym_ref.to_string();
        assert!(display.contains("BTC"));
        assert!(display.contains("50000"));
    }

    #[test]
    fn test_symbol_index_new() {
        let index = SymbolIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_symbol_index_register_and_get() {
        let index = SymbolIndex::new();
        let sym_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);

        index.register("BTC-20260130-50000-C", sym_ref.clone());

        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());

        let retrieved = index.get("BTC-20260130-50000-C");
        assert!(retrieved.is_some());
        let retrieved = retrieved.expect("should be present");
        assert_eq!(retrieved.underlying(), "BTC");
        assert_eq!(retrieved.strike(), 50000);
        assert_eq!(retrieved.option_style(), OptionStyle::Call);
    }

    #[test]
    fn test_symbol_index_get_not_found() {
        let index = SymbolIndex::new();
        assert!(index.get("BTC-20260130-50000-C").is_none());
    }

    #[test]
    fn test_symbol_index_contains() {
        let index = SymbolIndex::new();
        let sym_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);

        assert!(!index.contains("BTC-20260130-50000-C"));
        index.register("BTC-20260130-50000-C", sym_ref);
        assert!(index.contains("BTC-20260130-50000-C"));
    }

    #[test]
    fn test_symbol_index_deregister() {
        let index = SymbolIndex::new();
        let sym_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);

        index.register("BTC-20260130-50000-C", sym_ref);
        assert_eq!(index.len(), 1);

        let removed = index.deregister("BTC-20260130-50000-C");
        assert!(removed);
        assert!(index.is_empty());
        assert!(index.get("BTC-20260130-50000-C").is_none());
    }

    #[test]
    fn test_symbol_index_deregister_not_found() {
        let index = SymbolIndex::new();
        let removed = index.deregister("BTC-20260130-50000-C");
        assert!(!removed);
    }

    #[test]
    fn test_symbol_index_multiple_symbols() {
        let index = SymbolIndex::new();

        let call_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);
        let put_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Put);

        index.register("BTC-20260130-50000-C", call_ref);
        index.register("BTC-20260130-50000-P", put_ref);

        assert_eq!(index.len(), 2);

        let call = index.get("BTC-20260130-50000-C");
        assert!(call.is_some());
        assert_eq!(
            call.expect("should exist").option_style(),
            OptionStyle::Call
        );

        let put = index.get("BTC-20260130-50000-P");
        assert!(put.is_some());
        assert_eq!(put.expect("should exist").option_style(), OptionStyle::Put);
    }

    #[test]
    fn test_symbol_index_overwrite() {
        let index = SymbolIndex::new();

        let ref1 = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);
        let ref2 = SymbolRef::new("BTC", test_expiration(), 55000, OptionStyle::Call);

        index.register("BTC-20260130-50000-C", ref1);
        index.register("BTC-20260130-50000-C", ref2);

        assert_eq!(index.len(), 1);
        let retrieved = index.get("BTC-20260130-50000-C").expect("should exist");
        assert_eq!(retrieved.strike(), 55000);
    }

    #[test]
    fn test_symbol_index_symbols() {
        let index = SymbolIndex::new();

        let call_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);
        let put_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Put);

        index.register("BTC-20260130-50000-C", call_ref);
        index.register("BTC-20260130-50000-P", put_ref);

        let symbols = index.symbols();
        assert_eq!(symbols.len(), 2);
        assert!(symbols.contains(&"BTC-20260130-50000-C".to_string()));
        assert!(symbols.contains(&"BTC-20260130-50000-P".to_string()));
    }

    #[test]
    fn test_symbol_index_entries_empty() {
        let index = SymbolIndex::new();
        let entries = index.entries();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_symbol_index_entries() {
        let index = SymbolIndex::new();

        let call_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Call);
        let put_ref = SymbolRef::new("BTC", test_expiration(), 50000, OptionStyle::Put);

        index.register("BTC-20260130-50000-C", call_ref.clone());
        index.register("BTC-20260130-50000-P", put_ref.clone());

        let entries = index.entries();
        assert_eq!(entries.len(), 2);

        let call_entry = entries
            .iter()
            .find(|(sym, _)| sym == "BTC-20260130-50000-C");
        assert!(call_entry.is_some());
        let (_, sym_ref) = call_entry.expect("call entry should exist");
        assert_eq!(sym_ref.option_style(), OptionStyle::Call);

        let put_entry = entries
            .iter()
            .find(|(sym, _)| sym == "BTC-20260130-50000-P");
        assert!(put_entry.is_some());
        let (_, sym_ref) = put_entry.expect("put entry should exist");
        assert_eq!(sym_ref.option_style(), OptionStyle::Put);
    }
}
