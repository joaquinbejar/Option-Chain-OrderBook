//! Position limits configuration.
//!
//! This module provides the [`PositionLimits`] structure for configuring
//! maximum position sizes and Greek exposures.

use crate::pricing::Greeks;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Configuration for position limits.
///
/// Defines maximum allowed positions at various aggregation levels
/// and maximum Greek exposures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionLimits {
    /// Maximum position in a single option contract.
    per_option: Decimal,
    /// Maximum position across call+put at a strike.
    per_strike: Decimal,
    /// Maximum position across all strikes in an expiration.
    per_expiration: Decimal,
    /// Maximum position across all options on the underlying.
    per_underlying: Decimal,
    /// Maximum absolute delta exposure (in dollars).
    max_delta: Decimal,
    /// Maximum absolute gamma exposure (in dollars per 1% move).
    max_gamma: Decimal,
    /// Maximum absolute vega exposure (in dollars).
    max_vega: Decimal,
    /// Maximum absolute theta exposure (in dollars per day).
    max_theta: Decimal,
}

impl PositionLimits {
    /// Creates new position limits with the given values.
    #[must_use]
    pub const fn new(
        per_option: Decimal,
        per_strike: Decimal,
        per_expiration: Decimal,
        per_underlying: Decimal,
    ) -> Self {
        Self {
            per_option,
            per_strike,
            per_expiration,
            per_underlying,
            max_delta: Decimal::MAX,
            max_gamma: Decimal::MAX,
            max_vega: Decimal::MAX,
            max_theta: Decimal::MAX,
        }
    }

    /// Creates default limits suitable for small accounts.
    #[must_use]
    pub fn small() -> Self {
        Self {
            per_option: Decimal::from(100),
            per_strike: Decimal::from(200),
            per_expiration: Decimal::from(500),
            per_underlying: Decimal::from(1000),
            max_delta: Decimal::from(50000),
            max_gamma: Decimal::from(5000),
            max_vega: Decimal::from(10000),
            max_theta: Decimal::from(5000),
        }
    }

    /// Creates default limits suitable for medium accounts.
    #[must_use]
    pub fn medium() -> Self {
        Self {
            per_option: Decimal::from(500),
            per_strike: Decimal::from(1000),
            per_expiration: Decimal::from(2500),
            per_underlying: Decimal::from(5000),
            max_delta: Decimal::from(250000),
            max_gamma: Decimal::from(25000),
            max_vega: Decimal::from(50000),
            max_theta: Decimal::from(25000),
        }
    }

    /// Creates default limits suitable for large accounts.
    #[must_use]
    pub fn large() -> Self {
        Self {
            per_option: Decimal::from(1000),
            per_strike: Decimal::from(2000),
            per_expiration: Decimal::from(5000),
            per_underlying: Decimal::from(10000),
            max_delta: Decimal::from(500000),
            max_gamma: Decimal::from(50000),
            max_vega: Decimal::from(100000),
            max_theta: Decimal::from(50000),
        }
    }

    /// Returns the per-option limit.
    #[must_use]
    pub const fn per_option(&self) -> Decimal {
        self.per_option
    }

    /// Returns the per-strike limit.
    #[must_use]
    pub const fn per_strike(&self) -> Decimal {
        self.per_strike
    }

    /// Returns the per-expiration limit.
    #[must_use]
    pub const fn per_expiration(&self) -> Decimal {
        self.per_expiration
    }

    /// Returns the per-underlying limit.
    #[must_use]
    pub const fn per_underlying(&self) -> Decimal {
        self.per_underlying
    }

    /// Returns the maximum delta limit.
    #[must_use]
    pub const fn max_delta(&self) -> Decimal {
        self.max_delta
    }

    /// Returns the maximum gamma limit.
    #[must_use]
    pub const fn max_gamma(&self) -> Decimal {
        self.max_gamma
    }

    /// Returns the maximum vega limit.
    #[must_use]
    pub const fn max_vega(&self) -> Decimal {
        self.max_vega
    }

    /// Returns the maximum theta limit.
    #[must_use]
    pub const fn max_theta(&self) -> Decimal {
        self.max_theta
    }

    /// Sets the per-option limit.
    #[must_use]
    pub const fn with_per_option(mut self, limit: Decimal) -> Self {
        self.per_option = limit;
        self
    }

    /// Sets the per-strike limit.
    #[must_use]
    pub const fn with_per_strike(mut self, limit: Decimal) -> Self {
        self.per_strike = limit;
        self
    }

    /// Sets the per-expiration limit.
    #[must_use]
    pub const fn with_per_expiration(mut self, limit: Decimal) -> Self {
        self.per_expiration = limit;
        self
    }

    /// Sets the per-underlying limit.
    #[must_use]
    pub const fn with_per_underlying(mut self, limit: Decimal) -> Self {
        self.per_underlying = limit;
        self
    }

    /// Sets the maximum delta limit.
    #[must_use]
    pub const fn with_max_delta(mut self, limit: Decimal) -> Self {
        self.max_delta = limit;
        self
    }

    /// Sets the maximum gamma limit.
    #[must_use]
    pub const fn with_max_gamma(mut self, limit: Decimal) -> Self {
        self.max_gamma = limit;
        self
    }

    /// Sets the maximum vega limit.
    #[must_use]
    pub const fn with_max_vega(mut self, limit: Decimal) -> Self {
        self.max_vega = limit;
        self
    }

    /// Sets the maximum theta limit.
    #[must_use]
    pub const fn with_max_theta(mut self, limit: Decimal) -> Self {
        self.max_theta = limit;
        self
    }

    /// Checks if a position quantity exceeds the per-option limit.
    #[must_use]
    pub fn exceeds_per_option(&self, quantity: Decimal) -> bool {
        quantity.abs() > self.per_option
    }

    /// Checks if a position quantity exceeds the per-strike limit.
    #[must_use]
    pub fn exceeds_per_strike(&self, quantity: Decimal) -> bool {
        quantity.abs() > self.per_strike
    }

    /// Checks if a position quantity exceeds the per-expiration limit.
    #[must_use]
    pub fn exceeds_per_expiration(&self, quantity: Decimal) -> bool {
        quantity.abs() > self.per_expiration
    }

    /// Checks if a position quantity exceeds the per-underlying limit.
    #[must_use]
    pub fn exceeds_per_underlying(&self, quantity: Decimal) -> bool {
        quantity.abs() > self.per_underlying
    }

    /// Checks if Greeks exceed any limits.
    ///
    /// # Arguments
    ///
    /// * `greeks` - The Greeks to check
    /// * `spot` - Current spot price for dollar calculations
    /// * `multiplier` - Contract multiplier
    ///
    /// # Returns
    ///
    /// A vector of limit breaches, empty if none.
    #[must_use]
    pub fn check_greek_limits(
        &self,
        greeks: &Greeks,
        spot: Decimal,
        multiplier: Decimal,
    ) -> Vec<LimitBreach> {
        let mut breaches = Vec::new();

        let dollar_delta = greeks.dollar_delta(spot, multiplier).abs();
        if dollar_delta > self.max_delta {
            breaches.push(LimitBreach::Delta {
                current: dollar_delta,
                limit: self.max_delta,
            });
        }

        let dollar_gamma = greeks.dollar_gamma(spot, multiplier).abs();
        if dollar_gamma > self.max_gamma {
            breaches.push(LimitBreach::Gamma {
                current: dollar_gamma,
                limit: self.max_gamma,
            });
        }

        let dollar_vega = greeks.dollar_vega(multiplier).abs();
        if dollar_vega > self.max_vega {
            breaches.push(LimitBreach::Vega {
                current: dollar_vega,
                limit: self.max_vega,
            });
        }

        let dollar_theta = greeks.dollar_theta(multiplier).abs();
        if dollar_theta > self.max_theta {
            breaches.push(LimitBreach::Theta {
                current: dollar_theta,
                limit: self.max_theta,
            });
        }

        breaches
    }

    /// Returns the utilization ratio for a position quantity against per-option limit.
    #[must_use]
    pub fn option_utilization(&self, quantity: Decimal) -> Decimal {
        if self.per_option.is_zero() {
            return Decimal::ZERO;
        }
        quantity.abs() / self.per_option
    }

    /// Scales all limits by a factor.
    #[must_use]
    pub fn scale(self, factor: Decimal) -> Self {
        Self {
            per_option: self.per_option * factor,
            per_strike: self.per_strike * factor,
            per_expiration: self.per_expiration * factor,
            per_underlying: self.per_underlying * factor,
            max_delta: self.max_delta * factor,
            max_gamma: self.max_gamma * factor,
            max_vega: self.max_vega * factor,
            max_theta: self.max_theta * factor,
        }
    }
}

impl Default for PositionLimits {
    fn default() -> Self {
        Self::medium()
    }
}

/// Represents a limit breach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitBreach {
    /// Delta limit breached.
    Delta {
        /// Current delta exposure.
        current: Decimal,
        /// Delta limit.
        limit: Decimal,
    },
    /// Gamma limit breached.
    Gamma {
        /// Current gamma exposure.
        current: Decimal,
        /// Gamma limit.
        limit: Decimal,
    },
    /// Vega limit breached.
    Vega {
        /// Current vega exposure.
        current: Decimal,
        /// Vega limit.
        limit: Decimal,
    },
    /// Theta limit breached.
    Theta {
        /// Current theta exposure.
        current: Decimal,
        /// Theta limit.
        limit: Decimal,
    },
}

impl std::fmt::Display for LimitBreach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Delta { current, limit } => {
                write!(f, "Delta limit breached: {} > {}", current, limit)
            }
            Self::Gamma { current, limit } => {
                write!(f, "Gamma limit breached: {} > {}", current, limit)
            }
            Self::Vega { current, limit } => {
                write!(f, "Vega limit breached: {} > {}", current, limit)
            }
            Self::Theta { current, limit } => {
                write!(f, "Theta limit breached: {} > {}", current, limit)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_limits_creation() {
        let limits = PositionLimits::new(dec!(100), dec!(200), dec!(500), dec!(1000));

        assert_eq!(limits.per_option(), dec!(100));
        assert_eq!(limits.per_strike(), dec!(200));
        assert_eq!(limits.per_expiration(), dec!(500));
        assert_eq!(limits.per_underlying(), dec!(1000));
    }

    #[test]
    fn test_preset_limits() {
        let small = PositionLimits::small();
        let medium = PositionLimits::medium();
        let large = PositionLimits::large();

        assert!(small.per_option() < medium.per_option());
        assert!(medium.per_option() < large.per_option());
    }

    #[test]
    fn test_builder_methods() {
        let limits = PositionLimits::small()
            .with_per_option(dec!(150))
            .with_max_delta(dec!(75000));

        assert_eq!(limits.per_option(), dec!(150));
        assert_eq!(limits.max_delta(), dec!(75000));
    }

    #[test]
    fn test_exceeds_checks() {
        let limits = PositionLimits::new(dec!(100), dec!(200), dec!(500), dec!(1000));

        assert!(!limits.exceeds_per_option(dec!(50)));
        assert!(!limits.exceeds_per_option(dec!(100)));
        assert!(limits.exceeds_per_option(dec!(101)));

        // Negative quantities should also be checked
        assert!(limits.exceeds_per_option(dec!(-101)));
    }

    #[test]
    fn test_greek_limits() {
        let limits = PositionLimits::small();
        let greeks = Greeks::new(dec!(100), dec!(10), dec!(-50), dec!(200), dec!(5));

        // With spot=100, multiplier=100:
        // Dollar delta = 100 * 100 * 100 = 1,000,000 (exceeds 50,000)
        let breaches = limits.check_greek_limits(&greeks, dec!(100), dec!(100));

        assert!(!breaches.is_empty());
        assert!(
            breaches
                .iter()
                .any(|b| matches!(b, LimitBreach::Delta { .. }))
        );
    }

    #[test]
    fn test_no_greek_breaches() {
        let limits = PositionLimits::large();
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));

        let breaches = limits.check_greek_limits(&greeks, dec!(100), dec!(100));

        assert!(breaches.is_empty());
    }

    #[test]
    fn test_utilization() {
        let limits = PositionLimits::new(dec!(100), dec!(200), dec!(500), dec!(1000));

        assert_eq!(limits.option_utilization(dec!(50)), dec!(0.5));
        assert_eq!(limits.option_utilization(dec!(100)), dec!(1));
        assert_eq!(limits.option_utilization(dec!(-75)), dec!(0.75));
    }

    #[test]
    fn test_scale() {
        let limits = PositionLimits::small();
        let scaled = limits.scale(dec!(2));

        assert_eq!(scaled.per_option(), dec!(200));
        assert_eq!(scaled.per_strike(), dec!(400));
        assert_eq!(scaled.per_expiration(), dec!(1000));
        assert_eq!(scaled.per_underlying(), dec!(2000));
        assert_eq!(scaled.max_delta(), dec!(100000));
    }

    #[test]
    fn test_limit_breach_display() {
        let breach = LimitBreach::Delta {
            current: dec!(75000),
            limit: dec!(50000),
        };

        let display = format!("{}", breach);
        assert!(display.contains("Delta"));
        assert!(display.contains("75000"));
        assert!(display.contains("50000"));
    }

    #[test]
    fn test_serialization() {
        let limits = PositionLimits::medium();

        let json = serde_json::to_string(&limits).unwrap();
        let deserialized: PositionLimits = serde_json::from_str(&json).unwrap();

        assert_eq!(limits, deserialized);
    }
}
