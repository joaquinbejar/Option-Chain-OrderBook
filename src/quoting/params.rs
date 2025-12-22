//! Quote generation parameters.
//!
//! This module provides the [`QuoteParams`] structure that encapsulates
//! all parameters needed for generating market making quotes.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Parameters for quote generation.
///
/// Contains market conditions, inventory state, and risk parameters
/// needed to calculate optimal bid and ask prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteParams {
    /// Theoretical mid price of the option.
    theo_price: Decimal,
    /// Current inventory position (positive = long, negative = short).
    inventory: Decimal,
    /// Implied volatility (annualized decimal).
    volatility: Decimal,
    /// Time to expiration in years.
    time_to_expiry: Decimal,
    /// Risk aversion parameter (gamma in Avellaneda-Stoikov).
    risk_aversion: Decimal,
    /// Order arrival intensity parameter (k in Avellaneda-Stoikov).
    arrival_intensity: Decimal,
    /// Base spread in volatility points.
    base_spread_vol: Decimal,
    /// Maximum inventory limit.
    max_inventory: Decimal,
    /// Minimum quote size.
    min_size: Decimal,
    /// Maximum quote size.
    max_size: Decimal,
}

impl QuoteParams {
    /// Creates new quote parameters.
    ///
    /// # Arguments
    ///
    /// * `theo_price` - Theoretical mid price
    /// * `inventory` - Current inventory position
    /// * `volatility` - Implied volatility
    /// * `time_to_expiry` - Time to expiration in years
    #[must_use]
    pub fn new(
        theo_price: Decimal,
        inventory: Decimal,
        volatility: Decimal,
        time_to_expiry: Decimal,
    ) -> Self {
        Self {
            theo_price,
            inventory,
            volatility,
            time_to_expiry,
            risk_aversion: Decimal::from_str_exact("0.1").unwrap_or(Decimal::ONE),
            arrival_intensity: Decimal::from_str_exact("1.0").unwrap_or(Decimal::ONE),
            base_spread_vol: Decimal::from_str_exact("0.02").unwrap_or(Decimal::ZERO),
            max_inventory: Decimal::from(100),
            min_size: Decimal::ONE,
            max_size: Decimal::from(10),
        }
    }

    /// Returns the theoretical price.
    #[must_use]
    pub const fn theo_price(&self) -> Decimal {
        self.theo_price
    }

    /// Returns the current inventory.
    #[must_use]
    pub const fn inventory(&self) -> Decimal {
        self.inventory
    }

    /// Returns the volatility.
    #[must_use]
    pub const fn volatility(&self) -> Decimal {
        self.volatility
    }

    /// Returns the time to expiry.
    #[must_use]
    pub const fn time_to_expiry(&self) -> Decimal {
        self.time_to_expiry
    }

    /// Returns the risk aversion parameter.
    #[must_use]
    pub const fn risk_aversion(&self) -> Decimal {
        self.risk_aversion
    }

    /// Returns the arrival intensity parameter.
    #[must_use]
    pub const fn arrival_intensity(&self) -> Decimal {
        self.arrival_intensity
    }

    /// Returns the base spread in volatility points.
    #[must_use]
    pub const fn base_spread_vol(&self) -> Decimal {
        self.base_spread_vol
    }

    /// Returns the maximum inventory limit.
    #[must_use]
    pub const fn max_inventory(&self) -> Decimal {
        self.max_inventory
    }

    /// Returns the minimum quote size.
    #[must_use]
    pub const fn min_size(&self) -> Decimal {
        self.min_size
    }

    /// Returns the maximum quote size.
    #[must_use]
    pub const fn max_size(&self) -> Decimal {
        self.max_size
    }

    /// Sets the risk aversion parameter.
    #[must_use]
    pub const fn with_risk_aversion(mut self, risk_aversion: Decimal) -> Self {
        self.risk_aversion = risk_aversion;
        self
    }

    /// Sets the arrival intensity parameter.
    #[must_use]
    pub const fn with_arrival_intensity(mut self, arrival_intensity: Decimal) -> Self {
        self.arrival_intensity = arrival_intensity;
        self
    }

    /// Sets the base spread in volatility points.
    #[must_use]
    pub const fn with_base_spread_vol(mut self, base_spread_vol: Decimal) -> Self {
        self.base_spread_vol = base_spread_vol;
        self
    }

    /// Sets the maximum inventory limit.
    #[must_use]
    pub const fn with_max_inventory(mut self, max_inventory: Decimal) -> Self {
        self.max_inventory = max_inventory;
        self
    }

    /// Sets the quote size limits.
    #[must_use]
    pub const fn with_size_limits(mut self, min_size: Decimal, max_size: Decimal) -> Self {
        self.min_size = min_size;
        self.max_size = max_size;
        self
    }

    /// Returns the inventory ratio (inventory / max_inventory).
    ///
    /// Clamped to [-1, 1].
    #[must_use]
    pub fn inventory_ratio(&self) -> Decimal {
        if self.max_inventory.is_zero() {
            return Decimal::ZERO;
        }
        let ratio = self.inventory / self.max_inventory;
        ratio.max(-Decimal::ONE).min(Decimal::ONE)
    }

    /// Returns true if inventory is at or above the limit.
    #[must_use]
    pub fn is_inventory_full_long(&self) -> bool {
        self.inventory >= self.max_inventory
    }

    /// Returns true if inventory is at or below the negative limit.
    #[must_use]
    pub fn is_inventory_full_short(&self) -> bool {
        self.inventory <= -self.max_inventory
    }

    /// Validates the quote parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.theo_price < Decimal::ZERO {
            return Err("theoretical price cannot be negative".to_string());
        }
        if self.volatility < Decimal::ZERO {
            return Err("volatility cannot be negative".to_string());
        }
        if self.time_to_expiry < Decimal::ZERO {
            return Err("time to expiry cannot be negative".to_string());
        }
        if self.risk_aversion <= Decimal::ZERO {
            return Err("risk aversion must be positive".to_string());
        }
        if self.arrival_intensity <= Decimal::ZERO {
            return Err("arrival intensity must be positive".to_string());
        }
        if self.max_inventory <= Decimal::ZERO {
            return Err("max inventory must be positive".to_string());
        }
        if self.min_size <= Decimal::ZERO {
            return Err("min size must be positive".to_string());
        }
        if self.max_size < self.min_size {
            return Err("max size must be >= min size".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_quote_params_creation() {
        let params = QuoteParams::new(dec!(5.50), dec!(10), dec!(0.20), dec!(0.25));

        assert_eq!(params.theo_price(), dec!(5.50));
        assert_eq!(params.inventory(), dec!(10));
        assert_eq!(params.volatility(), dec!(0.20));
        assert_eq!(params.time_to_expiry(), dec!(0.25));
    }

    #[test]
    fn test_builder_methods() {
        let params = QuoteParams::new(dec!(5.50), dec!(10), dec!(0.20), dec!(0.25))
            .with_risk_aversion(dec!(0.5))
            .with_arrival_intensity(dec!(2.0))
            .with_base_spread_vol(dec!(0.03))
            .with_max_inventory(dec!(50))
            .with_size_limits(dec!(1), dec!(5));

        assert_eq!(params.risk_aversion(), dec!(0.5));
        assert_eq!(params.arrival_intensity(), dec!(2.0));
        assert_eq!(params.base_spread_vol(), dec!(0.03));
        assert_eq!(params.max_inventory(), dec!(50));
        assert_eq!(params.min_size(), dec!(1));
        assert_eq!(params.max_size(), dec!(5));
    }

    #[test]
    fn test_inventory_ratio() {
        let params = QuoteParams::new(dec!(5.50), dec!(50), dec!(0.20), dec!(0.25))
            .with_max_inventory(dec!(100));

        assert_eq!(params.inventory_ratio(), dec!(0.5));

        let full_long = QuoteParams::new(dec!(5.50), dec!(150), dec!(0.20), dec!(0.25))
            .with_max_inventory(dec!(100));
        assert_eq!(full_long.inventory_ratio(), Decimal::ONE);

        let full_short = QuoteParams::new(dec!(5.50), dec!(-150), dec!(0.20), dec!(0.25))
            .with_max_inventory(dec!(100));
        assert_eq!(full_short.inventory_ratio(), -Decimal::ONE);
    }

    #[test]
    fn test_inventory_limits() {
        let long = QuoteParams::new(dec!(5.50), dec!(100), dec!(0.20), dec!(0.25))
            .with_max_inventory(dec!(100));
        assert!(long.is_inventory_full_long());
        assert!(!long.is_inventory_full_short());

        let short = QuoteParams::new(dec!(5.50), dec!(-100), dec!(0.20), dec!(0.25))
            .with_max_inventory(dec!(100));
        assert!(!short.is_inventory_full_long());
        assert!(short.is_inventory_full_short());
    }

    #[test]
    fn test_validation() {
        let valid = QuoteParams::new(dec!(5.50), dec!(10), dec!(0.20), dec!(0.25));
        assert!(valid.validate().is_ok());

        let invalid_vol = QuoteParams::new(dec!(5.50), dec!(10), dec!(-0.20), dec!(0.25));
        assert!(invalid_vol.validate().is_err());

        let invalid_size = QuoteParams::new(dec!(5.50), dec!(10), dec!(0.20), dec!(0.25))
            .with_size_limits(dec!(10), dec!(5));
        assert!(invalid_size.validate().is_err());
    }

    #[test]
    fn test_serialization() {
        let params = QuoteParams::new(dec!(5.50), dec!(10), dec!(0.20), dec!(0.25));

        let json = serde_json::to_string(&params).unwrap();
        let deserialized: QuoteParams = serde_json::from_str(&json).unwrap();

        assert_eq!(params, deserialized);
    }
}
