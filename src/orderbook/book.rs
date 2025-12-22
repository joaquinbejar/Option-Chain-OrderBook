//! Option order book wrapper.
//!
//! This module provides the [`OptionOrderBook`] structure that wraps the
//! OrderBook-rs `OrderBook<T>` implementation with option-specific functionality.
//!
//! The `OrderBook` from OrderBook-rs is the core component that handles:
//! - Lock-free bid/ask price level management using SkipMap
//! - Order matching with price-time priority
//! - Multiple order types (limit, iceberg, post-only, etc.)
//! - Market metrics (VWAP, spread, imbalance, etc.)

use super::quote::Quote;
use crate::Result;
use orderbook_rs::{DefaultOrderBook, OrderBookSnapshot, OrderId, Side, TimeInForce};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

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
/// OptionChainManager
///   └── ExpirationManager (per expiry)
///         └── StrikeManager (per strike)
///               └── OptionOrderBook (per call/put) ← This struct
///                     └── OrderBook<T> (from OrderBook-rs)
/// ```
pub struct OptionOrderBook {
    /// The option contract symbol.
    symbol: String,
    /// Hash of the symbol for efficient comparison.
    symbol_hash: u64,
    /// The underlying order book from OrderBook-rs.
    /// Uses u64 for prices (smallest price units).
    book: DefaultOrderBook,
    /// Last known quote for change detection.
    last_quote: Quote,
}

impl OptionOrderBook {
    /// Creates a new option order book for the given symbol.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The option contract symbol (e.g., "BTC-20240329-50000-C")
    ///
    /// # Returns
    ///
    /// A new `OptionOrderBook` instance with an empty order book.
    #[must_use]
    pub fn new(symbol: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let symbol_hash = Self::hash_symbol(&symbol);

        Self {
            symbol: symbol.clone(),
            symbol_hash,
            book: DefaultOrderBook::new(&symbol),
            last_quote: Quote::empty(0),
        }
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

    /// Returns a reference to the underlying OrderBook from OrderBook-rs.
    #[must_use]
    pub const fn inner(&self) -> &DefaultOrderBook {
        &self.book
    }

    /// Returns a mutable reference to the underlying OrderBook.
    pub fn inner_mut(&mut self) -> &mut DefaultOrderBook {
        &mut self.book
    }

    /// Adds a limit order to the book.
    ///
    /// # Arguments
    ///
    /// * `order_id` - Unique identifier for the order
    /// * `side` - Buy or Sell side
    /// * `price` - Limit price in smallest units (u64)
    /// * `quantity` - Order quantity in smallest units (u64)
    ///
    /// # Returns
    ///
    /// `Ok(())` if the order was added successfully.
    pub fn add_limit_order(
        &self,
        order_id: OrderId,
        side: Side,
        price: u64,
        quantity: u64,
    ) -> Result<()> {
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
    /// * `price` - Limit price in smallest units (u64)
    /// * `quantity` - Order quantity in smallest units (u64)
    /// * `tif` - Time-in-force (GTC, IOC, FOK, etc.)
    pub fn add_limit_order_with_tif(
        &self,
        order_id: OrderId,
        side: Side,
        price: u64,
        quantity: u64,
        tif: TimeInForce,
    ) -> Result<()> {
        self.book
            .add_limit_order(order_id, price, quantity, side, tif, None)
            .map_err(|e| crate::Error::orderbook(e.to_string()))?;
        Ok(())
    }

    /// Cancels an order by its ID.
    ///
    /// # Arguments
    ///
    /// * `order_id` - The ID of the order to cancel
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the order was found and cancelled, `Ok(false)` if not found.
    pub fn cancel_order(&self, order_id: OrderId) -> Result<bool> {
        match self.book.cancel_order(order_id) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Returns the current best quote.
    ///
    /// # Returns
    ///
    /// A `Quote` with the best bid and ask prices and sizes.
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
    pub fn best_bid(&self) -> Option<u64> {
        self.book.best_bid()
    }

    /// Returns the best ask price.
    #[must_use]
    pub fn best_ask(&self) -> Option<u64> {
        self.book.best_ask()
    }

    /// Returns the mid price if both sides exist.
    #[must_use]
    pub fn mid_price(&self) -> Option<f64> {
        self.book.mid_price()
    }

    /// Returns the spread if both sides exist.
    #[must_use]
    pub fn spread(&self) -> Option<u64> {
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

    /// Clears all orders from the book by restoring from an empty snapshot.
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
        let changed = current != self.last_quote;
        self.last_quote = current;
        changed
    }

    /// Returns the last known quote.
    #[must_use]
    pub const fn last_quote(&self) -> &Quote {
        &self.last_quote
    }

    /// Returns depth at a specific price level on the bid side.
    #[must_use]
    pub fn bid_depth_at_price(&self, price: u64) -> u64 {
        let (bid_volumes, _) = self.book.get_volume_by_price();
        bid_volumes.get(&price).copied().unwrap_or(0)
    }

    /// Returns depth at a specific price level on the ask side.
    #[must_use]
    pub fn ask_depth_at_price(&self, price: u64) -> u64 {
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
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        assert_eq!(book.symbol(), "BTC-20240329-50000-C");
        assert!(book.is_empty());
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_add_limit_orders() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();

        assert_eq!(book.order_count(), 2);
        assert_eq!(book.bid_level_count(), 1);
        assert_eq!(book.ask_level_count(), 1);
    }

    #[test]
    fn test_best_quote() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();

        let quote = book.best_quote();

        assert_eq!(quote.bid_price(), Some(100));
        assert_eq!(quote.bid_size(), 10);
        assert_eq!(quote.ask_price(), Some(101));
        assert_eq!(quote.ask_size(), 5);
        assert!(quote.is_two_sided());
    }

    #[test]
    fn test_mid_price_and_spread() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 102, 5)
            .unwrap();

        assert_eq!(book.mid_price(), Some(101.0));
        assert_eq!(book.spread(), Some(2));
    }

    #[test]
    fn test_cancel_order() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        let order_id = OrderId::new();
        book.add_limit_order(order_id, Side::Buy, 100, 10).unwrap();
        assert_eq!(book.order_count(), 1);

        let cancelled = book.cancel_order(order_id).unwrap();
        assert!(cancelled);
        assert_eq!(book.order_count(), 0);
    }

    #[test]
    fn test_total_depth() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Buy, 99, 20)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();

        assert_eq!(book.total_bid_depth(), 30);
        assert_eq!(book.total_ask_depth(), 5);
    }

    #[test]
    fn test_empty_quote() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");
        let quote = book.best_quote();

        assert!(quote.bid_price().is_none());
        assert!(quote.ask_price().is_none());
        assert!(quote.is_empty());
        assert!(quote.mid_price().is_none());
    }

    #[test]
    fn test_update_last_quote() {
        let mut book = OptionOrderBook::new("BTC-20240329-50000-C");

        // First update - timestamp changes so it's considered a change
        let _ = book.update_last_quote();

        // Add order and update
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        let changed = book.update_last_quote();
        assert!(changed); // Price changed

        // Verify last quote has the bid
        assert_eq!(book.last_quote().bid_price(), Some(100));
    }

    #[test]
    fn test_symbol_hash() {
        let book1 = OptionOrderBook::new("BTC-20240329-50000-C");
        let book2 = OptionOrderBook::new("BTC-20240329-50000-C");
        let book3 = OptionOrderBook::new("BTC-20240329-50000-P");

        assert_eq!(book1.symbol_hash(), book2.symbol_hash());
        assert_ne!(book1.symbol_hash(), book3.symbol_hash());
    }

    #[test]
    fn test_vwap() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        book.add_limit_order(OrderId::new(), Side::Sell, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 105, 15)
            .unwrap();

        // VWAP for buying 20 units: (100*10 + 105*10) / 20 = 102.5
        let vwap = book.vwap(20, Side::Buy);
        assert!(vwap.is_some());
        assert!((vwap.unwrap() - 102.5).abs() < 0.01);
    }

    #[test]
    fn test_imbalance() {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");

        book.add_limit_order(OrderId::new(), Side::Buy, 100, 60)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 40)
            .unwrap();

        // Imbalance = (60 - 40) / (60 + 40) = 0.2
        let imbalance = book.imbalance(5);
        assert!((imbalance - 0.2).abs() < 0.01);
    }
}
