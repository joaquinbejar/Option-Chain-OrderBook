//! Quote types for option order books.
//!
//! This module defines quote structures representing two-sided markets
//! with bid and ask prices and sizes. Prices and sizes use `u64` to match
//! the OrderBook-rs interface (representing smallest units).

use serde::{Deserialize, Serialize};

/// Represents a two-sided quote with bid and ask.
///
/// A quote captures the best available prices and sizes on both sides
/// of the market at a given point in time. All prices and sizes are
/// in smallest units (u64) to match OrderBook-rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// Best bid price in smallest units (highest buy price).
    bid_price: Option<u64>,
    /// Size available at the best bid in smallest units.
    bid_size: u64,
    /// Best ask price in smallest units (lowest sell price).
    ask_price: Option<u64>,
    /// Size available at the best ask in smallest units.
    ask_size: u64,
    /// Timestamp of the quote in milliseconds since epoch.
    timestamp_ms: u64,
}

impl Quote {
    /// Creates a new quote with the given bid and ask.
    ///
    /// # Arguments
    ///
    /// * `bid_price` - The best bid price in smallest units (None if no bids)
    /// * `bid_size` - The size at the best bid in smallest units
    /// * `ask_price` - The best ask price in smallest units (None if no asks)
    /// * `ask_size` - The size at the best ask in smallest units
    /// * `timestamp_ms` - Timestamp in milliseconds since epoch
    #[must_use]
    pub const fn new(
        bid_price: Option<u64>,
        bid_size: u64,
        ask_price: Option<u64>,
        ask_size: u64,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            bid_price,
            bid_size,
            ask_price,
            ask_size,
            timestamp_ms,
        }
    }

    /// Creates an empty quote with no prices.
    #[must_use]
    pub const fn empty(timestamp_ms: u64) -> Self {
        Self {
            bid_price: None,
            bid_size: 0,
            ask_price: None,
            ask_size: 0,
            timestamp_ms,
        }
    }

    /// Returns the best bid price in smallest units.
    #[must_use]
    pub const fn bid_price(&self) -> Option<u64> {
        self.bid_price
    }

    /// Returns the size at the best bid in smallest units.
    #[must_use]
    pub const fn bid_size(&self) -> u64 {
        self.bid_size
    }

    /// Returns the best ask price in smallest units.
    #[must_use]
    pub const fn ask_price(&self) -> Option<u64> {
        self.ask_price
    }

    /// Returns the size at the best ask in smallest units.
    #[must_use]
    pub const fn ask_size(&self) -> u64 {
        self.ask_size
    }

    /// Returns the timestamp in milliseconds.
    #[must_use]
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns true if both bid and ask prices exist.
    #[must_use]
    pub const fn is_two_sided(&self) -> bool {
        self.bid_price.is_some() && self.ask_price.is_some()
    }

    /// Returns true if no prices exist.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bid_price.is_none() && self.ask_price.is_none()
    }

    /// Returns the mid price if both sides exist.
    ///
    /// The mid price is calculated as `(bid + ask) / 2`.
    #[must_use]
    pub fn mid_price(&self) -> Option<f64> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => Some((bid as f64 + ask as f64) / 2.0),
            _ => None,
        }
    }

    /// Returns the spread if both sides exist.
    ///
    /// The spread is calculated as `ask - bid`.
    #[must_use]
    pub fn spread(&self) -> Option<u64> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => Some(ask.saturating_sub(bid)),
            _ => None,
        }
    }

    /// Returns the spread in basis points if both sides exist.
    ///
    /// Calculated as `(spread / mid_price) * 10000`.
    #[must_use]
    pub fn spread_bps(&self) -> Option<f64> {
        let mid = self.mid_price()?;
        let spread = self.spread()?;
        if mid == 0.0 {
            return None;
        }
        Some((spread as f64 / mid) * 10000.0)
    }

    /// Returns true if the quote is valid (ask >= bid if both exist).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => ask >= bid,
            _ => true,
        }
    }
}

impl Default for Quote {
    fn default() -> Self {
        Self::empty(0)
    }
}

/// Represents an update to a quote.
///
/// Used to track changes in the order book's best bid/ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteUpdate {
    /// The symbol this update is for.
    symbol_hash: u64,
    /// Previous quote state.
    previous: Quote,
    /// Current quote state.
    current: Quote,
}

impl QuoteUpdate {
    /// Creates a new quote update.
    ///
    /// # Arguments
    ///
    /// * `symbol_hash` - Hash of the symbol for efficient comparison
    /// * `previous` - The previous quote state
    /// * `current` - The current quote state
    #[must_use]
    pub const fn new(symbol_hash: u64, previous: Quote, current: Quote) -> Self {
        Self {
            symbol_hash,
            previous,
            current,
        }
    }

    /// Returns the symbol hash.
    #[must_use]
    pub const fn symbol_hash(&self) -> u64 {
        self.symbol_hash
    }

    /// Returns the previous quote.
    #[must_use]
    pub const fn previous(&self) -> &Quote {
        &self.previous
    }

    /// Returns the current quote.
    #[must_use]
    pub const fn current(&self) -> &Quote {
        &self.current
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

    /// Returns true if the bid size changed.
    #[must_use]
    pub fn bid_size_changed(&self) -> bool {
        self.previous.bid_size != self.current.bid_size
    }

    /// Returns true if the ask size changed.
    #[must_use]
    pub fn ask_size_changed(&self) -> bool {
        self.previous.ask_size != self.current.ask_size
    }

    /// Returns the change in mid price if calculable.
    #[must_use]
    pub fn mid_price_change(&self) -> Option<f64> {
        let prev_mid = self.previous.mid_price()?;
        let curr_mid = self.current.mid_price()?;
        Some(curr_mid - prev_mid)
    }

    /// Returns the change in spread if calculable.
    #[must_use]
    pub fn spread_change(&self) -> Option<i64> {
        let prev_spread = self.previous.spread()? as i64;
        let curr_spread = self.current.spread()? as i64;
        Some(curr_spread - prev_spread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_creation() {
        let quote = Quote::new(Some(100), 10, Some(101), 5, 1000);

        assert_eq!(quote.bid_price(), Some(100));
        assert_eq!(quote.bid_size(), 10);
        assert_eq!(quote.ask_price(), Some(101));
        assert_eq!(quote.ask_size(), 5);
        assert_eq!(quote.timestamp_ms(), 1000);
    }

    #[test]
    fn test_empty_quote() {
        let quote = Quote::empty(1000);

        assert!(quote.bid_price().is_none());
        assert!(quote.ask_price().is_none());
        assert!(quote.is_empty());
        assert!(!quote.is_two_sided());
    }

    #[test]
    fn test_mid_price() {
        let quote = Quote::new(Some(100), 10, Some(102), 5, 1000);

        assert_eq!(quote.mid_price(), Some(101.0));
    }

    #[test]
    fn test_spread() {
        let quote = Quote::new(Some(100), 10, Some(102), 5, 1000);

        assert_eq!(quote.spread(), Some(2));
    }

    #[test]
    fn test_spread_bps() {
        let quote = Quote::new(Some(100), 10, Some(101), 5, 1000);

        // spread = 1, mid = 100.5, bps = (1/100.5) * 10000 ≈ 99.50
        let bps = quote.spread_bps().unwrap();
        assert!(bps > 99.0 && bps < 100.0);
    }

    #[test]
    fn test_quote_validity() {
        let valid = Quote::new(Some(100), 10, Some(101), 5, 1000);
        assert!(valid.is_valid());

        let invalid = Quote::new(Some(101), 10, Some(100), 5, 1000);
        assert!(!invalid.is_valid());

        let one_sided = Quote::new(Some(100), 10, None, 0, 1000);
        assert!(one_sided.is_valid());
    }

    #[test]
    fn test_quote_update() {
        let prev = Quote::new(Some(100), 10, Some(101), 5, 1000);
        let curr = Quote::new(Some(100), 15, Some(102), 5, 1001);

        let update = QuoteUpdate::new(12345, prev, curr);

        assert!(!update.bid_price_changed());
        assert!(update.ask_price_changed());
        assert!(update.bid_size_changed());
        assert!(!update.ask_size_changed());
        assert!(update.price_changed());
    }

    #[test]
    fn test_mid_price_change() {
        let prev = Quote::new(Some(100), 10, Some(102), 5, 1000);
        let curr = Quote::new(Some(101), 10, Some(103), 5, 1001);

        let update = QuoteUpdate::new(12345, prev, curr);

        // prev mid = 101, curr mid = 102
        assert_eq!(update.mid_price_change(), Some(1.0));
    }

    #[test]
    fn test_quote_serialization() {
        let quote = Quote::new(Some(100), 10, Some(101), 5, 1000);

        let json = serde_json::to_string(&quote).unwrap();
        let deserialized: Quote = serde_json::from_str(&json).unwrap();

        assert_eq!(quote, deserialized);
    }
}
