//! Generated quote structures.
//!
//! This module provides the [`GeneratedQuote`] structure that represents
//! the result of quote generation with bid and ask prices and sizes.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Result of quote generation.
///
/// Contains the calculated bid and ask prices and sizes, along with
/// metadata about the quote generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedQuote {
    /// Bid price.
    bid_price: Decimal,
    /// Bid size.
    bid_size: Decimal,
    /// Ask price.
    ask_price: Decimal,
    /// Ask size.
    ask_size: Decimal,
    /// Theoretical mid price used.
    theo_price: Decimal,
    /// Total spread (ask - bid).
    spread: Decimal,
    /// Skew applied (positive = skewed to sell).
    skew: Decimal,
    /// Timestamp in milliseconds since epoch.
    timestamp_ms: u64,
}

impl GeneratedQuote {
    /// Creates a new generated quote.
    ///
    /// # Arguments
    ///
    /// * `bid_price` - Bid price
    /// * `bid_size` - Bid size
    /// * `ask_price` - Ask price
    /// * `ask_size` - Ask size
    /// * `theo_price` - Theoretical mid price
    /// * `skew` - Skew applied
    /// * `timestamp_ms` - Generation timestamp
    #[must_use]
    pub fn new(
        bid_price: Decimal,
        bid_size: Decimal,
        ask_price: Decimal,
        ask_size: Decimal,
        theo_price: Decimal,
        skew: Decimal,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            bid_price,
            bid_size,
            ask_price,
            ask_size,
            theo_price,
            spread: ask_price - bid_price,
            skew,
            timestamp_ms,
        }
    }

    /// Creates a quote with symmetric spread around theo.
    ///
    /// # Arguments
    ///
    /// * `theo_price` - Theoretical mid price
    /// * `half_spread` - Half of the total spread
    /// * `size` - Size for both bid and ask
    /// * `timestamp_ms` - Generation timestamp
    #[must_use]
    pub fn symmetric(
        theo_price: Decimal,
        half_spread: Decimal,
        size: Decimal,
        timestamp_ms: u64,
    ) -> Self {
        let bid_price = (theo_price - half_spread).max(Decimal::ZERO);
        let ask_price = theo_price + half_spread;

        Self {
            bid_price,
            bid_size: size,
            ask_price,
            ask_size: size,
            theo_price,
            spread: ask_price - bid_price,
            skew: Decimal::ZERO,
            timestamp_ms,
        }
    }

    /// Returns the bid price.
    #[must_use]
    pub const fn bid_price(&self) -> Decimal {
        self.bid_price
    }

    /// Returns the bid size.
    #[must_use]
    pub const fn bid_size(&self) -> Decimal {
        self.bid_size
    }

    /// Returns the ask price.
    #[must_use]
    pub const fn ask_price(&self) -> Decimal {
        self.ask_price
    }

    /// Returns the ask size.
    #[must_use]
    pub const fn ask_size(&self) -> Decimal {
        self.ask_size
    }

    /// Returns the theoretical price.
    #[must_use]
    pub const fn theo_price(&self) -> Decimal {
        self.theo_price
    }

    /// Returns the total spread.
    #[must_use]
    pub const fn spread(&self) -> Decimal {
        self.spread
    }

    /// Returns the skew applied.
    #[must_use]
    pub const fn skew(&self) -> Decimal {
        self.skew
    }

    /// Returns the timestamp.
    #[must_use]
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns the mid price of the quote.
    #[must_use]
    pub fn mid_price(&self) -> Decimal {
        (self.bid_price + self.ask_price) / Decimal::TWO
    }

    /// Returns the spread in basis points relative to theo.
    #[must_use]
    pub fn spread_bps(&self) -> Option<Decimal> {
        if self.theo_price.is_zero() {
            return None;
        }
        Some((self.spread / self.theo_price) * Decimal::from(10000))
    }

    /// Returns true if the quote is valid (ask > bid, positive sizes).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.ask_price > self.bid_price
            && self.bid_price >= Decimal::ZERO
            && self.bid_size > Decimal::ZERO
            && self.ask_size > Decimal::ZERO
    }

    /// Returns the edge on the bid side (theo - bid).
    #[must_use]
    pub fn bid_edge(&self) -> Decimal {
        self.theo_price - self.bid_price
    }

    /// Returns the edge on the ask side (ask - theo).
    #[must_use]
    pub fn ask_edge(&self) -> Decimal {
        self.ask_price - self.theo_price
    }

    /// Applies a price adjustment to both sides.
    #[must_use]
    pub fn with_price_adjustment(mut self, adjustment: Decimal) -> Self {
        self.bid_price += adjustment;
        self.ask_price += adjustment;
        self.theo_price += adjustment;
        self
    }

    /// Applies a size multiplier to both sides.
    #[must_use]
    pub fn with_size_multiplier(mut self, multiplier: Decimal) -> Self {
        self.bid_size *= multiplier;
        self.ask_size *= multiplier;
        self
    }

    /// Rounds prices to a tick size.
    #[must_use]
    pub fn round_to_tick(mut self, tick_size: Decimal) -> Self {
        if !tick_size.is_zero() {
            self.bid_price = (self.bid_price / tick_size).floor() * tick_size;
            self.ask_price = (self.ask_price / tick_size).ceil() * tick_size;
            self.spread = self.ask_price - self.bid_price;
        }
        self
    }
}

impl Default for GeneratedQuote {
    fn default() -> Self {
        Self {
            bid_price: Decimal::ZERO,
            bid_size: Decimal::ZERO,
            ask_price: Decimal::ZERO,
            ask_size: Decimal::ZERO,
            theo_price: Decimal::ZERO,
            spread: Decimal::ZERO,
            skew: Decimal::ZERO,
            timestamp_ms: 0,
        }
    }
}

impl std::fmt::Display for GeneratedQuote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} @ {} / {} @ {} (theo: {}, spread: {})",
            self.bid_size,
            self.bid_price,
            self.ask_size,
            self.ask_price,
            self.theo_price,
            self.spread
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_generated_quote_creation() {
        let quote = GeneratedQuote::new(
            dec!(5.45),
            dec!(10),
            dec!(5.55),
            dec!(10),
            dec!(5.50),
            dec!(0),
            1000,
        );

        assert_eq!(quote.bid_price(), dec!(5.45));
        assert_eq!(quote.bid_size(), dec!(10));
        assert_eq!(quote.ask_price(), dec!(5.55));
        assert_eq!(quote.ask_size(), dec!(10));
        assert_eq!(quote.theo_price(), dec!(5.50));
        assert_eq!(quote.spread(), dec!(0.10));
        assert_eq!(quote.skew(), dec!(0));
    }

    #[test]
    fn test_symmetric_quote() {
        let quote = GeneratedQuote::symmetric(dec!(5.50), dec!(0.05), dec!(10), 1000);

        assert_eq!(quote.bid_price(), dec!(5.45));
        assert_eq!(quote.ask_price(), dec!(5.55));
        assert_eq!(quote.bid_size(), dec!(10));
        assert_eq!(quote.ask_size(), dec!(10));
        assert_eq!(quote.skew(), Decimal::ZERO);
    }

    #[test]
    fn test_mid_price() {
        let quote = GeneratedQuote::new(
            dec!(5.40),
            dec!(10),
            dec!(5.60),
            dec!(10),
            dec!(5.50),
            dec!(0),
            1000,
        );

        assert_eq!(quote.mid_price(), dec!(5.50));
    }

    #[test]
    fn test_spread_bps() {
        let quote = GeneratedQuote::symmetric(dec!(100), dec!(0.50), dec!(10), 1000);

        // Spread = 1.0, theo = 100, bps = (1/100) * 10000 = 100
        assert_eq!(quote.spread_bps(), Some(dec!(100)));
    }

    #[test]
    fn test_is_valid() {
        let valid = GeneratedQuote::symmetric(dec!(5.50), dec!(0.05), dec!(10), 1000);
        assert!(valid.is_valid());

        let invalid_spread = GeneratedQuote::new(
            dec!(5.55),
            dec!(10),
            dec!(5.45),
            dec!(10),
            dec!(5.50),
            dec!(0),
            1000,
        );
        assert!(!invalid_spread.is_valid());

        let invalid_size = GeneratedQuote::new(
            dec!(5.45),
            dec!(0),
            dec!(5.55),
            dec!(10),
            dec!(5.50),
            dec!(0),
            1000,
        );
        assert!(!invalid_size.is_valid());
    }

    #[test]
    fn test_edges() {
        let quote = GeneratedQuote::new(
            dec!(5.45),
            dec!(10),
            dec!(5.58),
            dec!(10),
            dec!(5.50),
            dec!(0.03),
            1000,
        );

        assert_eq!(quote.bid_edge(), dec!(0.05)); // 5.50 - 5.45
        assert_eq!(quote.ask_edge(), dec!(0.08)); // 5.58 - 5.50
    }

    #[test]
    fn test_round_to_tick() {
        let quote = GeneratedQuote::new(
            dec!(5.453),
            dec!(10),
            dec!(5.557),
            dec!(10),
            dec!(5.50),
            dec!(0),
            1000,
        );

        let rounded = quote.round_to_tick(dec!(0.01));

        assert_eq!(rounded.bid_price(), dec!(5.45)); // floor
        assert_eq!(rounded.ask_price(), dec!(5.56)); // ceil
    }

    #[test]
    fn test_with_adjustments() {
        let quote = GeneratedQuote::symmetric(dec!(5.50), dec!(0.05), dec!(10), 1000);

        let adjusted = quote.with_price_adjustment(dec!(0.10));
        assert_eq!(adjusted.bid_price(), dec!(5.55));
        assert_eq!(adjusted.ask_price(), dec!(5.65));

        let sized = quote.with_size_multiplier(dec!(2));
        assert_eq!(sized.bid_size(), dec!(20));
        assert_eq!(sized.ask_size(), dec!(20));
    }

    #[test]
    fn test_display() {
        let quote = GeneratedQuote::symmetric(dec!(5.50), dec!(0.05), dec!(10), 1000);
        let display = format!("{}", quote);

        assert!(display.contains("5.45"));
        assert!(display.contains("5.55"));
        assert!(display.contains("10"));
    }

    #[test]
    fn test_serialization() {
        let quote = GeneratedQuote::symmetric(dec!(5.50), dec!(0.05), dec!(10), 1000);

        let json = serde_json::to_string(&quote).unwrap();
        let deserialized: GeneratedQuote = serde_json::from_str(&json).unwrap();

        assert_eq!(quote, deserialized);
    }
}
