//! Pricing parameters for option valuation.
//!
//! This module provides the [`PricingParams`] structure that encapsulates
//! all parameters needed for option pricing calculations.

use crate::chain::OptionType;
use rust_decimal::Decimal;
use rust_decimal::prelude::MathematicalOps;
use serde::{Deserialize, Serialize};

/// Parameters required for option pricing.
///
/// Contains all market data and contract specifications needed to calculate
/// theoretical values and Greeks for an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingParams {
    /// Current spot price of the underlying.
    spot: Decimal,
    /// Strike price of the option.
    strike: Decimal,
    /// Time to expiration in years.
    time_to_expiry: Decimal,
    /// Implied volatility (annualized, as decimal e.g., 0.20 for 20%).
    volatility: Decimal,
    /// Risk-free interest rate (annualized, as decimal).
    risk_free_rate: Decimal,
    /// Dividend yield (annualized, as decimal).
    dividend_yield: Decimal,
    /// Option type (Call or Put).
    option_type: OptionType,
}

impl PricingParams {
    /// Creates new pricing parameters.
    ///
    /// # Arguments
    ///
    /// * `spot` - Current spot price
    /// * `strike` - Strike price
    /// * `time_to_expiry` - Time to expiration in years
    /// * `volatility` - Implied volatility (annualized decimal)
    /// * `risk_free_rate` - Risk-free rate (annualized decimal)
    /// * `dividend_yield` - Dividend yield (annualized decimal)
    /// * `option_type` - Call or Put
    #[must_use]
    pub const fn new(
        spot: Decimal,
        strike: Decimal,
        time_to_expiry: Decimal,
        volatility: Decimal,
        risk_free_rate: Decimal,
        dividend_yield: Decimal,
        option_type: OptionType,
    ) -> Self {
        Self {
            spot,
            strike,
            time_to_expiry,
            volatility,
            risk_free_rate,
            dividend_yield,
            option_type,
        }
    }

    /// Creates pricing parameters for a call option with zero dividend yield.
    #[must_use]
    pub const fn call(
        spot: Decimal,
        strike: Decimal,
        time_to_expiry: Decimal,
        volatility: Decimal,
        risk_free_rate: Decimal,
    ) -> Self {
        Self::new(
            spot,
            strike,
            time_to_expiry,
            volatility,
            risk_free_rate,
            Decimal::ZERO,
            OptionType::Call,
        )
    }

    /// Creates pricing parameters for a put option with zero dividend yield.
    #[must_use]
    pub const fn put(
        spot: Decimal,
        strike: Decimal,
        time_to_expiry: Decimal,
        volatility: Decimal,
        risk_free_rate: Decimal,
    ) -> Self {
        Self::new(
            spot,
            strike,
            time_to_expiry,
            volatility,
            risk_free_rate,
            Decimal::ZERO,
            OptionType::Put,
        )
    }

    /// Returns the spot price.
    #[must_use]
    pub const fn spot(&self) -> Decimal {
        self.spot
    }

    /// Returns the strike price.
    #[must_use]
    pub const fn strike(&self) -> Decimal {
        self.strike
    }

    /// Returns the time to expiration in years.
    #[must_use]
    pub const fn time_to_expiry(&self) -> Decimal {
        self.time_to_expiry
    }

    /// Returns the implied volatility.
    #[must_use]
    pub const fn volatility(&self) -> Decimal {
        self.volatility
    }

    /// Returns the risk-free rate.
    #[must_use]
    pub const fn risk_free_rate(&self) -> Decimal {
        self.risk_free_rate
    }

    /// Returns the dividend yield.
    #[must_use]
    pub const fn dividend_yield(&self) -> Decimal {
        self.dividend_yield
    }

    /// Returns the option type.
    #[must_use]
    pub const fn option_type(&self) -> OptionType {
        self.option_type
    }

    /// Returns true if this is a call option.
    #[must_use]
    pub const fn is_call(&self) -> bool {
        self.option_type.is_call()
    }

    /// Returns true if this is a put option.
    #[must_use]
    pub const fn is_put(&self) -> bool {
        self.option_type.is_put()
    }

    /// Returns the moneyness ratio (spot / strike).
    #[must_use]
    pub fn moneyness(&self) -> Decimal {
        if self.strike.is_zero() {
            return Decimal::ZERO;
        }
        self.spot / self.strike
    }

    /// Returns the log moneyness ln(spot / strike).
    #[must_use]
    pub fn log_moneyness(&self) -> Option<Decimal> {
        if self.strike.is_zero() || self.spot.is_zero() {
            return None;
        }
        let ratio = self.spot / self.strike;
        // Approximate ln using rust_decimal
        ratio.checked_ln()
    }

    /// Returns true if the option is in-the-money.
    #[must_use]
    pub fn is_itm(&self) -> bool {
        match self.option_type {
            OptionType::Call => self.spot > self.strike,
            OptionType::Put => self.spot < self.strike,
        }
    }

    /// Returns true if the option is out-of-the-money.
    #[must_use]
    pub fn is_otm(&self) -> bool {
        match self.option_type {
            OptionType::Call => self.spot < self.strike,
            OptionType::Put => self.spot > self.strike,
        }
    }

    /// Returns true if the option is at-the-money (within 1% of strike).
    #[must_use]
    pub fn is_atm(&self) -> bool {
        let diff = (self.spot - self.strike).abs();
        let threshold = self.strike * Decimal::from_str_exact("0.01").unwrap_or(Decimal::ZERO);
        diff <= threshold
    }

    /// Returns the intrinsic value of the option.
    #[must_use]
    pub fn intrinsic_value(&self) -> Decimal {
        match self.option_type {
            OptionType::Call => (self.spot - self.strike).max(Decimal::ZERO),
            OptionType::Put => (self.strike - self.spot).max(Decimal::ZERO),
        }
    }

    /// Returns true if the option has expired (time_to_expiry <= 0).
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.time_to_expiry <= Decimal::ZERO
    }

    /// Creates a copy with updated spot price.
    #[must_use]
    pub const fn with_spot(self, spot: Decimal) -> Self {
        Self { spot, ..self }
    }

    /// Creates a copy with updated volatility.
    #[must_use]
    pub const fn with_volatility(self, volatility: Decimal) -> Self {
        Self { volatility, ..self }
    }

    /// Creates a copy with updated time to expiry.
    #[must_use]
    pub const fn with_time_to_expiry(self, time_to_expiry: Decimal) -> Self {
        Self {
            time_to_expiry,
            ..self
        }
    }

    /// Validates the pricing parameters.
    ///
    /// # Returns
    ///
    /// `Ok(())` if valid, or an error message if invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.spot <= Decimal::ZERO {
            return Err("spot price must be positive".to_string());
        }
        if self.strike <= Decimal::ZERO {
            return Err("strike price must be positive".to_string());
        }
        if self.time_to_expiry < Decimal::ZERO {
            return Err("time to expiry cannot be negative".to_string());
        }
        if self.volatility < Decimal::ZERO {
            return Err("volatility cannot be negative".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_pricing_params_creation() {
        let params = PricingParams::new(
            dec!(100),
            dec!(105),
            dec!(0.25),
            dec!(0.20),
            dec!(0.05),
            dec!(0.02),
            OptionType::Call,
        );

        assert_eq!(params.spot(), dec!(100));
        assert_eq!(params.strike(), dec!(105));
        assert_eq!(params.time_to_expiry(), dec!(0.25));
        assert_eq!(params.volatility(), dec!(0.20));
        assert_eq!(params.risk_free_rate(), dec!(0.05));
        assert_eq!(params.dividend_yield(), dec!(0.02));
        assert!(params.is_call());
    }

    #[test]
    fn test_call_shorthand() {
        let params = PricingParams::call(dec!(100), dec!(105), dec!(0.25), dec!(0.20), dec!(0.05));

        assert!(params.is_call());
        assert_eq!(params.dividend_yield(), Decimal::ZERO);
    }

    #[test]
    fn test_put_shorthand() {
        let params = PricingParams::put(dec!(100), dec!(95), dec!(0.25), dec!(0.20), dec!(0.05));

        assert!(params.is_put());
        assert_eq!(params.dividend_yield(), Decimal::ZERO);
    }

    #[test]
    fn test_moneyness() {
        let params = PricingParams::call(dec!(110), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));

        assert_eq!(params.moneyness(), dec!(1.1));
    }

    #[test]
    fn test_itm_otm_atm() {
        // ITM call (spot > strike)
        let itm_call =
            PricingParams::call(dec!(110), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(itm_call.is_itm());
        assert!(!itm_call.is_otm());

        // OTM call (spot < strike)
        let otm_call = PricingParams::call(dec!(90), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(otm_call.is_otm());
        assert!(!otm_call.is_itm());

        // ATM call (spot ≈ strike)
        let atm_call =
            PricingParams::call(dec!(100), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(atm_call.is_atm());

        // ITM put (spot < strike)
        let itm_put = PricingParams::put(dec!(90), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(itm_put.is_itm());

        // OTM put (spot > strike)
        let otm_put = PricingParams::put(dec!(110), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(otm_put.is_otm());
    }

    #[test]
    fn test_intrinsic_value() {
        // ITM call: intrinsic = spot - strike = 110 - 100 = 10
        let itm_call =
            PricingParams::call(dec!(110), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert_eq!(itm_call.intrinsic_value(), dec!(10));

        // OTM call: intrinsic = 0
        let otm_call = PricingParams::call(dec!(90), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert_eq!(otm_call.intrinsic_value(), Decimal::ZERO);

        // ITM put: intrinsic = strike - spot = 100 - 90 = 10
        let itm_put = PricingParams::put(dec!(90), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert_eq!(itm_put.intrinsic_value(), dec!(10));

        // OTM put: intrinsic = 0
        let otm_put = PricingParams::put(dec!(110), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert_eq!(otm_put.intrinsic_value(), Decimal::ZERO);
    }

    #[test]
    fn test_with_methods() {
        let params = PricingParams::call(dec!(100), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));

        let updated = params.with_spot(dec!(110));
        assert_eq!(updated.spot(), dec!(110));
        assert_eq!(updated.strike(), dec!(100)); // unchanged

        let updated = params.with_volatility(dec!(0.30));
        assert_eq!(updated.volatility(), dec!(0.30));
        assert_eq!(updated.spot(), dec!(100)); // unchanged
    }

    #[test]
    fn test_validation() {
        let valid = PricingParams::call(dec!(100), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(valid.validate().is_ok());

        let invalid_spot =
            PricingParams::call(dec!(0), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(invalid_spot.validate().is_err());

        let invalid_strike =
            PricingParams::call(dec!(100), dec!(0), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(invalid_strike.validate().is_err());

        let invalid_vol =
            PricingParams::call(dec!(100), dec!(100), dec!(0.25), dec!(-0.20), dec!(0.05));
        assert!(invalid_vol.validate().is_err());
    }

    #[test]
    fn test_is_expired() {
        let active = PricingParams::call(dec!(100), dec!(100), dec!(0.25), dec!(0.20), dec!(0.05));
        assert!(!active.is_expired());

        let expired = PricingParams::call(dec!(100), dec!(100), dec!(0), dec!(0.20), dec!(0.05));
        assert!(expired.is_expired());

        let negative =
            PricingParams::call(dec!(100), dec!(100), dec!(-0.01), dec!(0.20), dec!(0.05));
        assert!(negative.is_expired());
    }

    #[test]
    fn test_serialization() {
        let params = PricingParams::call(dec!(100), dec!(105), dec!(0.25), dec!(0.20), dec!(0.05));

        let json = serde_json::to_string(&params).unwrap();
        let deserialized: PricingParams = serde_json::from_str(&json).unwrap();

        assert_eq!(params, deserialized);
    }
}
