//! Quote types for order book.
//!
//! This module provides the [`Quote`] and [`QuoteUpdate`] types for representing
//! two-sided markets (bid and ask).

use orderbook_rs::OrderId;
use serde::{Deserialize, Serialize};

/// Represents a two-sided quote (bid and ask).
///
/// A quote captures the best bid and ask prices and sizes at a point in time.
/// Prices are in smallest units (e.g., cents, satoshis) as `u64`.
///
/// Note: Equality comparison excludes the `id` field, comparing only market data.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Quote {
    /// Best bid price (None if no bids).
    bid_price: Option<u128>,
    /// Size available at best bid.
    bid_size: u64,
    /// Best ask price (None if no asks).
    ask_price: Option<u128>,
    /// Size available at best ask.
    ask_size: u64,
    /// Timestamp in milliseconds.
    timestamp_ms: u64,
    /// Unique identifier for this quote.
    id: OrderId,
}

impl Quote {
    /// Creates a new quote with the given values.
    ///
    /// # Arguments
    ///
    /// * `bid_price` - Best bid price (None if no bids)
    /// * `bid_size` - Size at best bid
    /// * `ask_price` - Best ask price (None if no asks)
    /// * `ask_size` - Size at best ask
    /// * `timestamp_ms` - Timestamp in milliseconds
    #[must_use]
    pub fn new(
        bid_price: Option<u128>,
        bid_size: u64,
        ask_price: Option<u128>,
        ask_size: u64,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            bid_price,
            bid_size,
            ask_price,
            ask_size,
            timestamp_ms,
            id: OrderId::new(),
        }
    }

    /// Creates an empty quote with no prices.
    #[must_use]
    pub fn empty(timestamp_ms: u64) -> Self {
        Self {
            bid_price: None,
            bid_size: 0,
            ask_price: None,
            ask_size: 0,
            timestamp_ms,
            id: OrderId::new(),
        }
    }

    /// Returns the unique identifier for this quote.
    #[must_use]
    pub const fn id(&self) -> OrderId {
        self.id
    }

    /// Returns the best bid price.
    #[must_use]
    pub const fn bid_price(&self) -> Option<u128> {
        self.bid_price
    }

    /// Returns the size at best bid.
    #[must_use]
    pub const fn bid_size(&self) -> u64 {
        self.bid_size
    }

    /// Returns the best ask price.
    #[must_use]
    pub const fn ask_price(&self) -> Option<u128> {
        self.ask_price
    }

    /// Returns the size at best ask.
    #[must_use]
    pub const fn ask_size(&self) -> u64 {
        self.ask_size
    }

    /// Returns the timestamp in milliseconds.
    #[must_use]
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns true if the quote has both bid and ask.
    #[must_use]
    pub const fn is_two_sided(&self) -> bool {
        self.bid_price.is_some() && self.ask_price.is_some()
    }

    /// Returns true if the quote has no prices.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bid_price.is_none() && self.ask_price.is_none()
    }

    /// Returns the spread if both sides exist.
    #[must_use]
    pub fn spread(&self) -> Option<u128> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) if ask >= bid => Some(ask - bid),
            _ => None,
        }
    }

    /// Returns the mid price if both sides exist.
    #[must_use]
    pub fn mid_price(&self) -> Option<f64> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => Some((bid as f64 + ask as f64) / 2.0),
            _ => None,
        }
    }

    /// Returns the spread in basis points relative to mid price.
    #[must_use]
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.spread(), self.mid_price()) {
            (Some(spread), Some(mid)) if mid > 0.0 => Some(spread as f64 / mid * 10000.0),
            _ => None,
        }
    }
}

impl PartialEq for Quote {
    fn eq(&self, other: &Self) -> bool {
        // Note: timestamp_ms and id are excluded from comparison
        // to detect only market data changes (prices and sizes)
        self.bid_price == other.bid_price
            && self.bid_size == other.bid_size
            && self.ask_price == other.ask_price
            && self.ask_size == other.ask_size
    }
}

impl Eq for Quote {}

impl std::fmt::Display for Quote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => {
                write!(f, "{}@{} / {}@{}", self.bid_size, bid, self.ask_size, ask)
            }
            (Some(bid), None) => write!(f, "{}@{} / -", self.bid_size, bid),
            (None, Some(ask)) => write!(f, "- / {}@{}", self.ask_size, ask),
            (None, None) => write!(f, "- / -"),
        }
    }
}

/// Represents a change in quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteUpdate {
    /// Previous quote.
    pub previous: Quote,
    /// Current quote.
    pub current: Quote,
}

impl QuoteUpdate {
    /// Creates a new quote update.
    #[must_use]
    pub const fn new(previous: Quote, current: Quote) -> Self {
        Self { previous, current }
    }

    /// Returns true if the bid price changed.
    #[must_use]
    pub fn bid_price_changed(&self) -> bool {
        self.previous.bid_price != self.current.bid_price
    }

    /// Returns true if the ask price changed.
    #[must_use]
    pub fn ask_price_changed(&self) -> bool {
        self.previous.ask_price != self.current.ask_price
    }

    /// Returns true if any price changed.
    #[must_use]
    pub fn price_changed(&self) -> bool {
        self.bid_price_changed() || self.ask_price_changed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_creation() {
        let quote = Quote::new(Some(100), 10, Some(105), 5, 1234567890);

        assert_eq!(quote.bid_price(), Some(100));
        assert_eq!(quote.bid_size(), 10);
        assert_eq!(quote.ask_price(), Some(105));
        assert_eq!(quote.ask_size(), 5);
        assert!(quote.is_two_sided());
    }

    #[test]
    fn test_quote_empty() {
        let quote = Quote::empty(0);

        assert!(quote.is_empty());
        assert!(!quote.is_two_sided());
        assert!(quote.spread().is_none());
        assert!(quote.mid_price().is_none());
    }

    #[test]
    fn test_quote_spread() {
        let quote = Quote::new(Some(100), 10, Some(105), 5, 0);

        assert_eq!(quote.spread(), Some(5));
        assert!((quote.mid_price().unwrap() - 102.5).abs() < 0.01);
    }

    #[test]
    fn test_quote_display() {
        let quote = Quote::new(Some(100), 10, Some(105), 5, 0);
        assert_eq!(format!("{}", quote), "10@100 / 5@105");

        let bid_only = Quote::new(Some(100), 10, None, 0, 0);
        assert_eq!(format!("{}", bid_only), "10@100 / -");

        let ask_only = Quote::new(None, 0, Some(105), 5, 0);
        assert_eq!(format!("{}", ask_only), "- / 5@105");

        let empty = Quote::empty(0);
        assert_eq!(format!("{}", empty), "- / -");
    }

    #[test]
    fn test_quote_update() {
        let prev = Quote::new(Some(100), 10, Some(105), 5, 0);
        let curr = Quote::new(Some(101), 10, Some(105), 5, 1);
        let update = QuoteUpdate::new(prev, curr);

        assert!(update.bid_price_changed());
        assert!(!update.ask_price_changed());
        assert!(update.price_changed());
    }

    #[test]
    fn test_quote_timestamp() {
        let quote = Quote::new(Some(100), 10, Some(105), 5, 1234567890);
        assert_eq!(quote.timestamp_ms(), 1234567890);
    }

    #[test]
    fn test_quote_spread_bps() {
        let quote = Quote::new(Some(100), 10, Some(105), 5, 0);
        let spread_bps = quote.spread_bps();
        assert!(spread_bps.is_some());
        // Spread = 5, mid = 102.5, bps = 5/102.5 * 10000 ≈ 487.8
        assert!((spread_bps.unwrap() - 487.8).abs() < 1.0);
    }

    #[test]
    fn test_quote_spread_bps_none() {
        let quote = Quote::new(Some(100), 10, None, 0, 0);
        assert!(quote.spread_bps().is_none());

        let quote2 = Quote::empty(0);
        assert!(quote2.spread_bps().is_none());
    }
}
