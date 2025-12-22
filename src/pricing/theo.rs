//! Theoretical value calculation results.
//!
//! This module provides the [`TheoreticalValue`] structure that contains
//! the result of option pricing calculations including price and Greeks.

use super::greeks::Greeks;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Result of a theoretical value calculation.
///
/// Contains the calculated option price along with all Greeks and
/// metadata about the calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TheoreticalValue {
    /// Theoretical option price.
    price: Decimal,
    /// Option Greeks.
    greeks: Greeks,
    /// Implied volatility used in calculation.
    implied_volatility: Decimal,
    /// Timestamp of calculation in milliseconds since epoch.
    timestamp_ms: u64,
}

impl TheoreticalValue {
    /// Creates a new theoretical value.
    ///
    /// # Arguments
    ///
    /// * `price` - Calculated option price
    /// * `greeks` - Calculated Greeks
    /// * `implied_volatility` - IV used in calculation
    /// * `timestamp_ms` - Calculation timestamp in milliseconds
    #[must_use]
    pub const fn new(
        price: Decimal,
        greeks: Greeks,
        implied_volatility: Decimal,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            price,
            greeks,
            implied_volatility,
            timestamp_ms,
        }
    }

    /// Creates a theoretical value with only price (no Greeks).
    #[must_use]
    pub const fn price_only(
        price: Decimal,
        implied_volatility: Decimal,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            price,
            greeks: Greeks::zero(),
            implied_volatility,
            timestamp_ms,
        }
    }

    /// Returns the theoretical price.
    #[must_use]
    pub const fn price(&self) -> Decimal {
        self.price
    }

    /// Returns the Greeks.
    #[must_use]
    pub const fn greeks(&self) -> &Greeks {
        &self.greeks
    }

    /// Returns the implied volatility.
    #[must_use]
    pub const fn implied_volatility(&self) -> Decimal {
        self.implied_volatility
    }

    /// Returns the calculation timestamp.
    #[must_use]
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns the delta.
    #[must_use]
    pub const fn delta(&self) -> Decimal {
        self.greeks.delta()
    }

    /// Returns the gamma.
    #[must_use]
    pub const fn gamma(&self) -> Decimal {
        self.greeks.gamma()
    }

    /// Returns the theta.
    #[must_use]
    pub const fn theta(&self) -> Decimal {
        self.greeks.theta()
    }

    /// Returns the vega.
    #[must_use]
    pub const fn vega(&self) -> Decimal {
        self.greeks.vega()
    }

    /// Returns the rho.
    #[must_use]
    pub const fn rho(&self) -> Decimal {
        self.greeks.rho()
    }

    /// Calculates the bid price given a spread.
    ///
    /// # Arguments
    ///
    /// * `half_spread` - Half of the bid-ask spread
    #[must_use]
    pub fn bid_price(&self, half_spread: Decimal) -> Decimal {
        (self.price - half_spread).max(Decimal::ZERO)
    }

    /// Calculates the ask price given a spread.
    ///
    /// # Arguments
    ///
    /// * `half_spread` - Half of the bid-ask spread
    #[must_use]
    pub fn ask_price(&self, half_spread: Decimal) -> Decimal {
        self.price + half_spread
    }

    /// Calculates bid and ask prices given a spread.
    ///
    /// # Arguments
    ///
    /// * `spread` - Total bid-ask spread
    ///
    /// # Returns
    ///
    /// Tuple of (bid_price, ask_price).
    #[must_use]
    pub fn bid_ask(&self, spread: Decimal) -> (Decimal, Decimal) {
        let half = spread / Decimal::TWO;
        (self.bid_price(half), self.ask_price(half))
    }

    /// Scales the theoretical value by a quantity.
    ///
    /// Useful for calculating position-level values.
    ///
    /// # Arguments
    ///
    /// * `quantity` - Number of contracts
    /// * `multiplier` - Contract multiplier
    #[must_use]
    pub fn scale(&self, quantity: Decimal, multiplier: Decimal) -> ScaledTheoreticalValue {
        let scale_factor = quantity * multiplier;
        ScaledTheoreticalValue {
            notional_value: self.price * scale_factor,
            greeks: self.greeks.scale(quantity),
            quantity,
            multiplier,
        }
    }

    /// Checks if the theoretical value is stale.
    ///
    /// # Arguments
    ///
    /// * `max_age_ms` - Maximum age in milliseconds
    /// * `current_time_ms` - Current time in milliseconds
    #[must_use]
    pub const fn is_stale(&self, max_age_ms: u64, current_time_ms: u64) -> bool {
        current_time_ms.saturating_sub(self.timestamp_ms) > max_age_ms
    }
}

impl Default for TheoreticalValue {
    fn default() -> Self {
        Self {
            price: Decimal::ZERO,
            greeks: Greeks::zero(),
            implied_volatility: Decimal::ZERO,
            timestamp_ms: 0,
        }
    }
}

/// Scaled theoretical value for a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaledTheoreticalValue {
    /// Total notional value (price * quantity * multiplier).
    pub notional_value: Decimal,
    /// Scaled Greeks for the position.
    pub greeks: Greeks,
    /// Position quantity.
    pub quantity: Decimal,
    /// Contract multiplier.
    pub multiplier: Decimal,
}

impl ScaledTheoreticalValue {
    /// Returns the dollar delta for the position.
    #[must_use]
    pub fn dollar_delta(&self, spot: Decimal) -> Decimal {
        self.greeks.dollar_delta(spot, self.multiplier)
    }

    /// Returns the dollar gamma for the position.
    #[must_use]
    pub fn dollar_gamma(&self, spot: Decimal) -> Decimal {
        self.greeks.dollar_gamma(spot, self.multiplier)
    }

    /// Returns the dollar theta for the position.
    #[must_use]
    pub fn dollar_theta(&self) -> Decimal {
        self.greeks.dollar_theta(self.multiplier)
    }

    /// Returns the dollar vega for the position.
    #[must_use]
    pub fn dollar_vega(&self) -> Decimal {
        self.greeks.dollar_vega(self.multiplier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn create_test_theo() -> TheoreticalValue {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        TheoreticalValue::new(dec!(5.50), greeks, dec!(0.20), 1000)
    }

    #[test]
    fn test_theo_creation() {
        let theo = create_test_theo();

        assert_eq!(theo.price(), dec!(5.50));
        assert_eq!(theo.delta(), dec!(0.5));
        assert_eq!(theo.gamma(), dec!(0.02));
        assert_eq!(theo.theta(), dec!(-0.05));
        assert_eq!(theo.vega(), dec!(0.15));
        assert_eq!(theo.rho(), dec!(0.01));
        assert_eq!(theo.implied_volatility(), dec!(0.20));
        assert_eq!(theo.timestamp_ms(), 1000);
    }

    #[test]
    fn test_price_only() {
        let theo = TheoreticalValue::price_only(dec!(5.50), dec!(0.20), 1000);

        assert_eq!(theo.price(), dec!(5.50));
        assert!(theo.greeks().is_zero());
    }

    #[test]
    fn test_bid_ask() {
        let theo = create_test_theo();

        // With spread of 0.10
        let (bid, ask) = theo.bid_ask(dec!(0.10));
        assert_eq!(bid, dec!(5.45));
        assert_eq!(ask, dec!(5.55));
    }

    #[test]
    fn test_bid_price_floor() {
        let theo = TheoreticalValue::price_only(dec!(0.05), dec!(0.20), 1000);

        // Bid should not go negative
        let bid = theo.bid_price(dec!(0.10));
        assert_eq!(bid, Decimal::ZERO);
    }

    #[test]
    fn test_scale() {
        let theo = create_test_theo();

        // 10 contracts with multiplier of 100
        let scaled = theo.scale(dec!(10), dec!(100));

        // Notional = 5.50 * 10 * 100 = 5500
        assert_eq!(scaled.notional_value, dec!(5500));

        // Greeks scaled by quantity (not multiplier)
        assert_eq!(scaled.greeks.delta(), dec!(5)); // 0.5 * 10
    }

    #[test]
    fn test_scaled_dollar_greeks() {
        let theo = create_test_theo();
        let scaled = theo.scale(dec!(10), dec!(100));

        // Dollar delta = delta * spot * multiplier
        // = 5 * 100 * 100 = 50000
        let dollar_delta = scaled.dollar_delta(dec!(100));
        assert_eq!(dollar_delta, dec!(50000));
    }

    #[test]
    fn test_is_stale() {
        let theo = create_test_theo(); // timestamp = 1000

        // Not stale if within max age
        assert!(!theo.is_stale(500, 1400));

        // Stale if beyond max age
        assert!(theo.is_stale(500, 1600));
    }

    #[test]
    fn test_default() {
        let theo = TheoreticalValue::default();

        assert_eq!(theo.price(), Decimal::ZERO);
        assert!(theo.greeks().is_zero());
        assert_eq!(theo.implied_volatility(), Decimal::ZERO);
    }

    #[test]
    fn test_serialization() {
        let theo = create_test_theo();

        let json = serde_json::to_string(&theo).unwrap();
        let deserialized: TheoreticalValue = serde_json::from_str(&json).unwrap();

        assert_eq!(theo, deserialized);
    }
}
