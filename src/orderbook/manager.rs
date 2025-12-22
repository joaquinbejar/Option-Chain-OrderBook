//! Option order book manager.
//!
//! This module provides the [`OptionOrderBookManager`] for managing order books
//! across the entire option chain. Each option contract (call/put at a specific
//! strike and expiration) has its own `OptionOrderBook` backed by OrderBook-rs.

use super::book::OptionOrderBook;
use super::quote::Quote;
use crate::{Error, Result};
use std::collections::HashMap;

/// Manages order books for all options in a chain.
///
/// Provides centralized access to order books for each option contract,
/// with methods for aggregating data across the entire chain.
#[derive(Default)]
pub struct OptionOrderBookManager {
    /// Order books indexed by symbol.
    books: HashMap<String, OptionOrderBook>,
    /// Order books indexed by symbol hash for fast lookup.
    books_by_hash: HashMap<u64, String>,
}

impl OptionOrderBookManager {
    /// Creates a new empty order book manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
            books_by_hash: HashMap::new(),
        }
    }

    /// Creates a new order book manager with pre-allocated capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Expected number of option contracts
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            books: HashMap::with_capacity(capacity),
            books_by_hash: HashMap::with_capacity(capacity),
        }
    }

    /// Returns the number of order books.
    #[must_use]
    pub fn len(&self) -> usize {
        self.books.len()
    }

    /// Returns true if there are no order books.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }

    /// Gets or creates an order book for the given symbol.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The option contract symbol
    ///
    /// # Returns
    ///
    /// A mutable reference to the order book.
    pub fn get_or_create(&mut self, symbol: impl Into<String>) -> &mut OptionOrderBook {
        let symbol = symbol.into();
        if !self.books.contains_key(&symbol) {
            let book = OptionOrderBook::new(&symbol);
            let hash = book.symbol_hash();
            self.books_by_hash.insert(hash, symbol.clone());
            self.books.insert(symbol.clone(), book);
        }
        self.books.get_mut(&symbol).unwrap()
    }

    /// Gets an order book by symbol.
    #[must_use]
    pub fn get(&self, symbol: &str) -> Option<&OptionOrderBook> {
        self.books.get(symbol)
    }

    /// Gets a mutable order book by symbol.
    pub fn get_mut(&mut self, symbol: &str) -> Option<&mut OptionOrderBook> {
        self.books.get_mut(symbol)
    }

    /// Gets an order book by symbol hash.
    #[must_use]
    pub fn get_by_hash(&self, hash: u64) -> Option<&OptionOrderBook> {
        self.books_by_hash
            .get(&hash)
            .and_then(|symbol| self.books.get(symbol))
    }

    /// Returns true if an order book exists for the symbol.
    #[must_use]
    pub fn contains(&self, symbol: &str) -> bool {
        self.books.contains_key(symbol)
    }

    /// Removes an order book by symbol.
    ///
    /// # Returns
    ///
    /// The removed order book if it existed.
    pub fn remove(&mut self, symbol: &str) -> Option<OptionOrderBook> {
        if let Some(book) = self.books.remove(symbol) {
            self.books_by_hash.remove(&book.symbol_hash());
            Some(book)
        } else {
            None
        }
    }

    /// Clears all order books.
    pub fn clear(&mut self) {
        self.books.clear();
        self.books_by_hash.clear();
    }

    /// Returns an iterator over all symbols.
    pub fn symbols(&self) -> impl Iterator<Item = &str> {
        self.books.keys().map(String::as_str)
    }

    /// Returns an iterator over all order books.
    pub fn books(&self) -> impl Iterator<Item = &OptionOrderBook> {
        self.books.values()
    }

    /// Returns a mutable iterator over all order books.
    pub fn books_mut(&mut self) -> impl Iterator<Item = &mut OptionOrderBook> {
        self.books.values_mut()
    }

    /// Returns an iterator over (symbol, book) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &OptionOrderBook)> {
        self.books.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Gets the best quote for a symbol.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The option contract symbol
    ///
    /// # Returns
    ///
    /// The best quote if the order book exists.
    pub fn best_quote(&self, symbol: &str) -> Result<Quote> {
        self.books
            .get(symbol)
            .map(OptionOrderBook::best_quote)
            .ok_or_else(|| Error::contract_not_found(symbol))
    }

    /// Gets all quotes for all order books.
    ///
    /// # Returns
    ///
    /// A vector of (symbol, quote) pairs.
    #[must_use]
    pub fn all_quotes(&self) -> Vec<(&str, Quote)> {
        self.books
            .iter()
            .map(|(symbol, book)| (symbol.as_str(), book.best_quote()))
            .collect()
    }

    /// Returns the total number of orders across all books.
    #[must_use]
    pub fn total_order_count(&self) -> usize {
        self.books.values().map(OptionOrderBook::order_count).sum()
    }

    /// Returns the total bid depth across all books.
    #[must_use]
    pub fn total_bid_depth(&self) -> u64 {
        self.books
            .values()
            .map(OptionOrderBook::total_bid_depth)
            .sum()
    }

    /// Returns the total ask depth across all books.
    #[must_use]
    pub fn total_ask_depth(&self) -> u64 {
        self.books
            .values()
            .map(OptionOrderBook::total_ask_depth)
            .sum()
    }

    /// Clears all orders from all books.
    pub fn clear_all_orders(&self) {
        for book in self.books.values() {
            book.clear();
        }
    }

    /// Returns statistics about the order book manager.
    #[must_use]
    pub fn stats(&self) -> OrderBookManagerStats {
        let book_count = self.len();
        let total_orders = self.total_order_count();
        let total_bid_depth = self.total_bid_depth();
        let total_ask_depth = self.total_ask_depth();
        let two_sided_count = self
            .books
            .values()
            .filter(|b| b.best_quote().is_two_sided())
            .count();

        OrderBookManagerStats {
            book_count,
            total_orders,
            total_bid_depth,
            total_ask_depth,
            two_sided_count,
        }
    }
}

/// Statistics about the order book manager.
#[derive(Debug, Clone)]
pub struct OrderBookManagerStats {
    /// Number of order books.
    pub book_count: usize,
    /// Total number of orders across all books.
    pub total_orders: usize,
    /// Total bid depth across all books in smallest units.
    pub total_bid_depth: u64,
    /// Total ask depth across all books in smallest units.
    pub total_ask_depth: u64,
    /// Number of books with two-sided quotes.
    pub two_sided_count: usize,
}

impl std::fmt::Display for OrderBookManagerStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} books, {} orders, {} two-sided, bid depth: {}, ask depth: {}",
            self.book_count,
            self.total_orders,
            self.two_sided_count,
            self.total_bid_depth,
            self.total_ask_depth
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orderbook_rs::{OrderId, Side};

    #[test]
    fn test_manager_creation() {
        let manager = OptionOrderBookManager::new();
        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_get_or_create() {
        let mut manager = OptionOrderBookManager::new();

        let book = manager.get_or_create("BTC-20240329-50000-C");
        assert_eq!(book.symbol(), "BTC-20240329-50000-C");

        // Getting again should return the same book
        let book2 = manager.get_or_create("BTC-20240329-50000-C");
        assert_eq!(book2.symbol(), "BTC-20240329-50000-C");

        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_multiple_books() {
        let mut manager = OptionOrderBookManager::new();

        manager.get_or_create("BTC-20240329-50000-C");
        manager.get_or_create("BTC-20240329-50000-P");
        manager.get_or_create("BTC-20240329-55000-C");

        assert_eq!(manager.len(), 3);
        assert!(manager.contains("BTC-20240329-50000-C"));
        assert!(manager.contains("BTC-20240329-50000-P"));
        assert!(!manager.contains("BTC-20240329-60000-C"));
    }

    #[test]
    fn test_get_by_hash() {
        let mut manager = OptionOrderBookManager::new();

        let book = manager.get_or_create("BTC-20240329-50000-C");
        let hash = book.symbol_hash();

        let found = manager.get_by_hash(hash);
        assert!(found.is_some());
        assert_eq!(found.unwrap().symbol(), "BTC-20240329-50000-C");
    }

    #[test]
    fn test_remove() {
        let mut manager = OptionOrderBookManager::new();

        manager.get_or_create("BTC-20240329-50000-C");
        assert_eq!(manager.len(), 1);

        let removed = manager.remove("BTC-20240329-50000-C");
        assert!(removed.is_some());
        assert_eq!(manager.len(), 0);

        let not_found = manager.remove("nonexistent");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_best_quote() {
        let mut manager = OptionOrderBookManager::new();

        let book = manager.get_or_create("BTC-20240329-50000-C");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();

        let quote = manager.best_quote("BTC-20240329-50000-C").unwrap();
        assert_eq!(quote.bid_price(), Some(100));
        assert_eq!(quote.ask_price(), Some(101));

        let err = manager.best_quote("nonexistent");
        assert!(err.is_err());
    }

    #[test]
    fn test_total_depth() {
        let mut manager = OptionOrderBookManager::new();

        let book1 = manager.get_or_create("BTC-20240329-50000-C");
        book1
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book1
            .add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();

        let book2 = manager.get_or_create("BTC-20240329-50000-P");
        book2
            .add_limit_order(OrderId::new(), Side::Buy, 50, 20)
            .unwrap();
        book2
            .add_limit_order(OrderId::new(), Side::Sell, 51, 15)
            .unwrap();

        assert_eq!(manager.total_bid_depth(), 30);
        assert_eq!(manager.total_ask_depth(), 20);
        assert_eq!(manager.total_order_count(), 4);
    }

    #[test]
    fn test_stats() {
        let mut manager = OptionOrderBookManager::new();

        let book = manager.get_or_create("BTC-20240329-50000-C");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();

        manager.get_or_create("BTC-20240329-50000-P"); // Empty book

        let stats = manager.stats();
        assert_eq!(stats.book_count, 2);
        assert_eq!(stats.total_orders, 2);
        assert_eq!(stats.two_sided_count, 1);
        assert_eq!(stats.total_bid_depth, 10);
        assert_eq!(stats.total_ask_depth, 5);
    }

    #[test]
    fn test_all_quotes() {
        let mut manager = OptionOrderBookManager::new();

        let book1 = manager.get_or_create("BTC-20240329-50000-C");
        book1
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();

        let book2 = manager.get_or_create("BTC-20240329-50000-P");
        book2
            .add_limit_order(OrderId::new(), Side::Sell, 50, 5)
            .unwrap();

        let quotes = manager.all_quotes();
        assert_eq!(quotes.len(), 2);
    }

    #[test]
    fn test_symbols_iterator() {
        let mut manager = OptionOrderBookManager::new();

        manager.get_or_create("BTC-20240329-50000-C");
        manager.get_or_create("BTC-20240329-50000-P");

        let symbols: Vec<_> = manager.symbols().collect();
        assert_eq!(symbols.len(), 2);
        assert!(symbols.contains(&"BTC-20240329-50000-C"));
        assert!(symbols.contains(&"BTC-20240329-50000-P"));
    }
}
