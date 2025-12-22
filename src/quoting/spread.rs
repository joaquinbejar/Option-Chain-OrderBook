//! Spread calculation for market making.
//!
//! This module provides the [`SpreadCalculator`] for calculating optimal
//! spreads based on the Avellaneda-Stoikov model and various market conditions.

use super::generated::GeneratedQuote;
use super::params::QuoteParams;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Spread calculator using the Avellaneda-Stoikov model.
///
/// Calculates optimal bid and ask spreads based on:
/// - Risk aversion parameter
/// - Volatility
/// - Time to horizon
/// - Current inventory
/// - Order arrival intensity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpreadCalculator {
    /// Minimum spread in price terms.
    min_spread: Decimal,
    /// Maximum spread in price terms.
    max_spread: Decimal,
    /// Inventory skew factor.
    inventory_skew_factor: Decimal,
    /// Volatility adjustment factor.
    volatility_factor: Decimal,
}

impl SpreadCalculator {
    /// Creates a new spread calculator with default parameters.
    #[must_use]
    pub fn new() -> Self {
        Self {
            min_spread: Decimal::from_str_exact("0.001").unwrap_or(Decimal::ZERO),
            max_spread: Decimal::from_str_exact("0.10").unwrap_or(Decimal::ONE),
            inventory_skew_factor: Decimal::from_str_exact("0.001").unwrap_or(Decimal::ZERO),
            volatility_factor: Decimal::ONE,
        }
    }

    /// Sets the minimum spread.
    #[must_use]
    pub const fn with_min_spread(mut self, min_spread: Decimal) -> Self {
        self.min_spread = min_spread;
        self
    }

    /// Sets the maximum spread.
    #[must_use]
    pub const fn with_max_spread(mut self, max_spread: Decimal) -> Self {
        self.max_spread = max_spread;
        self
    }

    /// Sets the inventory skew factor.
    #[must_use]
    pub const fn with_inventory_skew_factor(mut self, factor: Decimal) -> Self {
        self.inventory_skew_factor = factor;
        self
    }

    /// Sets the volatility adjustment factor.
    #[must_use]
    pub const fn with_volatility_factor(mut self, factor: Decimal) -> Self {
        self.volatility_factor = factor;
        self
    }

    /// Calculates the optimal spread using Avellaneda-Stoikov model.
    ///
    /// The optimal spread is:
    /// ```text
    /// δ* = γ·σ²·(T-t) + (2/γ)·ln(1 + γ/k)
    /// ```
    ///
    /// # Arguments
    ///
    /// * `params` - Quote generation parameters
    ///
    /// # Returns
    ///
    /// The optimal half-spread (to be applied to both bid and ask).
    #[must_use]
    pub fn optimal_spread(&self, params: &QuoteParams) -> Decimal {
        let gamma = params.risk_aversion();
        let sigma = params.volatility();
        let tau = params.time_to_expiry();
        let k = params.arrival_intensity();

        // Variance term: γ·σ²·τ
        let variance_term = gamma * sigma * sigma * tau;

        // Intensity term: (2/γ)·ln(1 + γ/k)
        // Approximate ln(1+x) ≈ x for small x, or use a more accurate approximation
        let gamma_over_k = gamma / k;
        let ln_term = self.approximate_ln_1_plus_x(gamma_over_k);
        let intensity_term = (Decimal::TWO / gamma) * ln_term;

        let base_spread = variance_term + intensity_term;

        // Apply volatility factor
        let adjusted_spread = base_spread * self.volatility_factor;

        // Clamp to min/max
        adjusted_spread.max(self.min_spread).min(self.max_spread)
    }

    /// Calculates the inventory skew.
    ///
    /// The reservation price adjustment is:
    /// ```text
    /// skew = q·γ·σ²·(T-t)
    /// ```
    ///
    /// Positive skew means we want to sell (shift quotes down).
    /// Negative skew means we want to buy (shift quotes up).
    ///
    /// # Arguments
    ///
    /// * `params` - Quote generation parameters
    #[must_use]
    pub fn inventory_skew(&self, params: &QuoteParams) -> Decimal {
        let q = params.inventory();
        let gamma = params.risk_aversion();
        let sigma = params.volatility();
        let tau = params.time_to_expiry();

        q * gamma * sigma * sigma * tau * self.inventory_skew_factor
    }

    /// Generates a complete quote with spread and skew.
    ///
    /// # Arguments
    ///
    /// * `params` - Quote generation parameters
    /// * `timestamp_ms` - Current timestamp in milliseconds
    #[must_use]
    pub fn generate_quote(&self, params: &QuoteParams, timestamp_ms: u64) -> GeneratedQuote {
        let half_spread = self.optimal_spread(params) / Decimal::TWO;
        let skew = self.inventory_skew(params);
        let theo = params.theo_price();

        // Apply skew to reservation price
        let reservation_price = theo - skew;

        // Calculate bid and ask around reservation price
        let mut bid_price = reservation_price - half_spread;
        let ask_price = reservation_price + half_spread;

        // Ensure bid is non-negative
        bid_price = bid_price.max(Decimal::ZERO);

        // Calculate sizes based on inventory
        let (bid_size, ask_size) = self.calculate_sizes(params);

        GeneratedQuote::new(
            bid_price,
            bid_size,
            ask_price,
            ask_size,
            theo,
            skew,
            timestamp_ms,
        )
    }

    /// Calculates bid and ask sizes based on inventory.
    ///
    /// When long, reduce bid size and increase ask size.
    /// When short, increase bid size and reduce ask size.
    fn calculate_sizes(&self, params: &QuoteParams) -> (Decimal, Decimal) {
        let base_size = (params.min_size() + params.max_size()) / Decimal::TWO;
        let inventory_ratio = params.inventory_ratio();

        // Adjust sizes based on inventory
        // Long inventory -> smaller bid, larger ask
        // Short inventory -> larger bid, smaller ask
        let size_adjustment = base_size * inventory_ratio.abs() / Decimal::TWO;

        let (bid_size, ask_size) = if inventory_ratio > Decimal::ZERO {
            // Long: reduce bid, increase ask
            (base_size - size_adjustment, base_size + size_adjustment)
        } else if inventory_ratio < Decimal::ZERO {
            // Short: increase bid, reduce ask
            (base_size + size_adjustment, base_size - size_adjustment)
        } else {
            (base_size, base_size)
        };

        // Clamp to limits
        let bid_size = bid_size.max(params.min_size()).min(params.max_size());
        let ask_size = ask_size.max(params.min_size()).min(params.max_size());

        // If at inventory limit, set size to zero on that side
        let bid_size = if params.is_inventory_full_long() {
            Decimal::ZERO
        } else {
            bid_size
        };
        let ask_size = if params.is_inventory_full_short() {
            Decimal::ZERO
        } else {
            ask_size
        };

        (bid_size, ask_size)
    }

    /// Approximates ln(1+x) using Taylor series.
    ///
    /// For small x: ln(1+x) ≈ x - x²/2 + x³/3 - ...
    fn approximate_ln_1_plus_x(&self, x: Decimal) -> Decimal {
        if x.abs() < Decimal::from_str_exact("0.5").unwrap_or(Decimal::ONE) {
            // Use Taylor series for small x
            let x2 = x * x;
            let x3 = x2 * x;
            x - x2 / Decimal::TWO + x3 / Decimal::from(3)
        } else {
            // For larger x, use a rougher approximation
            // ln(1+x) ≈ x / (1 + x/2) for moderate x
            x / (Decimal::ONE + x / Decimal::TWO)
        }
    }
}

impl Default for SpreadCalculator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn create_test_params() -> QuoteParams {
        QuoteParams::new(dec!(5.50), dec!(0), dec!(0.20), dec!(0.25))
            .with_risk_aversion(dec!(0.1))
            .with_arrival_intensity(dec!(1.0))
            .with_max_inventory(dec!(100))
            .with_size_limits(dec!(1), dec!(10))
    }

    #[test]
    fn test_spread_calculator_creation() {
        let calc = SpreadCalculator::new();
        assert!(calc.min_spread > Decimal::ZERO);
        assert!(calc.max_spread > calc.min_spread);
    }

    #[test]
    fn test_optimal_spread() {
        let calc = SpreadCalculator::new();
        let params = create_test_params();

        let spread = calc.optimal_spread(&params);

        assert!(spread >= calc.min_spread);
        assert!(spread <= calc.max_spread);
    }

    #[test]
    fn test_inventory_skew_neutral() {
        let calc = SpreadCalculator::new();
        let params = create_test_params(); // inventory = 0

        let skew = calc.inventory_skew(&params);
        assert_eq!(skew, Decimal::ZERO);
    }

    #[test]
    fn test_inventory_skew_long() {
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(50), dec!(0.20), dec!(0.25))
            .with_risk_aversion(dec!(0.1))
            .with_max_inventory(dec!(100));

        let skew = calc.inventory_skew(&params);
        assert!(skew > Decimal::ZERO); // Positive skew when long
    }

    #[test]
    fn test_inventory_skew_short() {
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(-50), dec!(0.20), dec!(0.25))
            .with_risk_aversion(dec!(0.1))
            .with_max_inventory(dec!(100));

        let skew = calc.inventory_skew(&params);
        assert!(skew < Decimal::ZERO); // Negative skew when short
    }

    #[test]
    fn test_generate_quote_neutral() {
        let calc = SpreadCalculator::new();
        let params = create_test_params();

        let quote = calc.generate_quote(&params, 1000);

        assert!(quote.is_valid());
        assert!(quote.bid_price() < quote.theo_price());
        assert!(quote.ask_price() > quote.theo_price());
        assert_eq!(quote.skew(), Decimal::ZERO);
    }

    #[test]
    fn test_generate_quote_long_inventory() {
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(50), dec!(0.20), dec!(0.25))
            .with_risk_aversion(dec!(0.1))
            .with_max_inventory(dec!(100))
            .with_size_limits(dec!(1), dec!(10));

        let quote = calc.generate_quote(&params, 1000);

        // When long, skew should be positive (want to sell)
        assert!(quote.skew() > Decimal::ZERO);
        // Ask size should be larger than bid size
        assert!(quote.ask_size() >= quote.bid_size());
    }

    #[test]
    fn test_generate_quote_at_inventory_limit() {
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(100), dec!(0.20), dec!(0.25))
            .with_risk_aversion(dec!(0.1))
            .with_max_inventory(dec!(100))
            .with_size_limits(dec!(1), dec!(10));

        let quote = calc.generate_quote(&params, 1000);

        // When at max long inventory, bid size should be zero
        assert_eq!(quote.bid_size(), Decimal::ZERO);
        assert!(quote.ask_size() > Decimal::ZERO);
    }

    #[test]
    fn test_spread_increases_with_volatility() {
        // Use higher risk aversion to make variance term more significant
        // and higher max_spread to avoid clamping
        let calc = SpreadCalculator::new().with_max_spread(dec!(10.0));

        // Higher risk aversion makes γ·σ²·τ term dominate over intensity term
        let low_vol = QuoteParams::new(dec!(5.50), dec!(0), dec!(0.20), dec!(1.0))
            .with_risk_aversion(dec!(1.0))
            .with_arrival_intensity(dec!(10.0));
        let high_vol = QuoteParams::new(dec!(5.50), dec!(0), dec!(0.80), dec!(1.0))
            .with_risk_aversion(dec!(1.0))
            .with_arrival_intensity(dec!(10.0));

        let low_spread = calc.optimal_spread(&low_vol);
        let high_spread = calc.optimal_spread(&high_vol);

        // With σ_low=0.20 and σ_high=0.80, variance term ratio is 16x
        assert!(
            high_spread > low_spread,
            "high_spread ({}) should be > low_spread ({})",
            high_spread,
            low_spread
        );
    }

    #[test]
    fn test_builder_methods() {
        let calc = SpreadCalculator::new()
            .with_min_spread(dec!(0.01))
            .with_max_spread(dec!(0.50))
            .with_inventory_skew_factor(dec!(0.002))
            .with_volatility_factor(dec!(1.5));

        assert_eq!(calc.min_spread, dec!(0.01));
        assert_eq!(calc.max_spread, dec!(0.50));
        assert_eq!(calc.inventory_skew_factor, dec!(0.002));
        assert_eq!(calc.volatility_factor, dec!(1.5));
    }

    #[test]
    fn test_serialization() {
        let calc = SpreadCalculator::new()
            .with_min_spread(dec!(0.01))
            .with_max_spread(dec!(0.50));

        let json = serde_json::to_string(&calc).unwrap();
        let deserialized: SpreadCalculator = serde_json::from_str(&json).unwrap();

        assert_eq!(calc, deserialized);
    }
}
