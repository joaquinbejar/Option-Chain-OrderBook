//! Greeks calculation and container types.
//!
//! This module provides the [`Greeks`] structure for storing and manipulating
//! option Greeks (sensitivities to various market parameters).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::ops::{Add, Mul, Neg};

/// Container for option Greeks.
///
/// Greeks measure the sensitivity of an option's price to various factors:
/// - **Delta**: Sensitivity to underlying price changes
/// - **Gamma**: Rate of change of delta (second derivative to price)
/// - **Theta**: Time decay (sensitivity to time passage)
/// - **Vega**: Sensitivity to volatility changes
/// - **Rho**: Sensitivity to interest rate changes
///
/// All values are expressed per single contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Greeks {
    /// Delta: ∂V/∂S - sensitivity to underlying price.
    /// Range: -1 to 1 for single options.
    delta: Decimal,

    /// Gamma: ∂²V/∂S² - rate of change of delta.
    /// Always positive for long options.
    gamma: Decimal,

    /// Theta: ∂V/∂t - time decay per day.
    /// Usually negative for long options (value decreases with time).
    theta: Decimal,

    /// Vega: ∂V/∂σ - sensitivity to volatility.
    /// Usually positive for long options.
    vega: Decimal,

    /// Rho: ∂V/∂r - sensitivity to interest rate.
    rho: Decimal,
}

impl Greeks {
    /// Creates a new Greeks container with all values.
    ///
    /// # Arguments
    ///
    /// * `delta` - Delta value
    /// * `gamma` - Gamma value
    /// * `theta` - Theta value (per day)
    /// * `vega` - Vega value
    /// * `rho` - Rho value
    #[must_use]
    pub const fn new(
        delta: Decimal,
        gamma: Decimal,
        theta: Decimal,
        vega: Decimal,
        rho: Decimal,
    ) -> Self {
        Self {
            delta,
            gamma,
            theta,
            vega,
            rho,
        }
    }

    /// Creates a Greeks container with all zeros.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            delta: Decimal::ZERO,
            gamma: Decimal::ZERO,
            theta: Decimal::ZERO,
            vega: Decimal::ZERO,
            rho: Decimal::ZERO,
        }
    }

    /// Returns the delta value.
    #[must_use]
    pub const fn delta(&self) -> Decimal {
        self.delta
    }

    /// Returns the gamma value.
    #[must_use]
    pub const fn gamma(&self) -> Decimal {
        self.gamma
    }

    /// Returns the theta value (per day).
    #[must_use]
    pub const fn theta(&self) -> Decimal {
        self.theta
    }

    /// Returns the vega value.
    #[must_use]
    pub const fn vega(&self) -> Decimal {
        self.vega
    }

    /// Returns the rho value.
    #[must_use]
    pub const fn rho(&self) -> Decimal {
        self.rho
    }

    /// Returns the absolute delta.
    #[must_use]
    pub fn abs_delta(&self) -> Decimal {
        self.delta.abs()
    }

    /// Returns true if delta is positive (long exposure).
    #[must_use]
    pub fn is_long_delta(&self) -> bool {
        self.delta > Decimal::ZERO
    }

    /// Returns true if delta is negative (short exposure).
    #[must_use]
    pub fn is_short_delta(&self) -> bool {
        self.delta < Decimal::ZERO
    }

    /// Returns true if all Greeks are zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.delta.is_zero()
            && self.gamma.is_zero()
            && self.theta.is_zero()
            && self.vega.is_zero()
            && self.rho.is_zero()
    }

    /// Scales all Greeks by a multiplier (e.g., for position sizing).
    ///
    /// # Arguments
    ///
    /// * `multiplier` - The scaling factor
    #[must_use]
    pub fn scale(&self, multiplier: Decimal) -> Self {
        Self {
            delta: self.delta * multiplier,
            gamma: self.gamma * multiplier,
            theta: self.theta * multiplier,
            vega: self.vega * multiplier,
            rho: self.rho * multiplier,
        }
    }

    /// Calculates the dollar delta (delta * spot * multiplier).
    ///
    /// # Arguments
    ///
    /// * `spot` - Current spot price
    /// * `multiplier` - Contract multiplier
    #[must_use]
    pub fn dollar_delta(&self, spot: Decimal, multiplier: Decimal) -> Decimal {
        self.delta * spot * multiplier
    }

    /// Calculates the dollar gamma (gamma * spot² * multiplier / 100).
    ///
    /// Represents P&L change for a 1% move in the underlying.
    ///
    /// # Arguments
    ///
    /// * `spot` - Current spot price
    /// * `multiplier` - Contract multiplier
    #[must_use]
    pub fn dollar_gamma(&self, spot: Decimal, multiplier: Decimal) -> Decimal {
        let one_percent = spot / Decimal::from(100);
        self.gamma * one_percent * one_percent * multiplier / Decimal::TWO
    }

    /// Calculates the dollar theta (theta * multiplier).
    ///
    /// # Arguments
    ///
    /// * `multiplier` - Contract multiplier
    #[must_use]
    pub fn dollar_theta(&self, multiplier: Decimal) -> Decimal {
        self.theta * multiplier
    }

    /// Calculates the dollar vega (vega * multiplier).
    ///
    /// # Arguments
    ///
    /// * `multiplier` - Contract multiplier
    #[must_use]
    pub fn dollar_vega(&self, multiplier: Decimal) -> Decimal {
        self.vega * multiplier
    }

    /// Estimates P&L for given market moves.
    ///
    /// Uses Taylor expansion: ΔV ≈ Δ·ΔS + ½Γ·(ΔS)² + Θ·Δt + ν·Δσ
    ///
    /// # Arguments
    ///
    /// * `spot_change` - Change in underlying price
    /// * `vol_change` - Change in volatility (in decimal, e.g., 0.01 for 1%)
    /// * `days_passed` - Number of days passed
    #[must_use]
    pub fn estimate_pnl(
        &self,
        spot_change: Decimal,
        vol_change: Decimal,
        days_passed: Decimal,
    ) -> Decimal {
        let delta_pnl = self.delta * spot_change;
        let gamma_pnl = self.gamma * spot_change * spot_change / Decimal::TWO;
        let theta_pnl = self.theta * days_passed;
        let vega_pnl = self.vega * vol_change;

        delta_pnl + gamma_pnl + theta_pnl + vega_pnl
    }
}

impl Add for Greeks {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            delta: self.delta + other.delta,
            gamma: self.gamma + other.gamma,
            theta: self.theta + other.theta,
            vega: self.vega + other.vega,
            rho: self.rho + other.rho,
        }
    }
}

impl Neg for Greeks {
    type Output = Self;

    fn neg(self) -> Self {
        Self {
            delta: -self.delta,
            gamma: -self.gamma,
            theta: -self.theta,
            vega: -self.vega,
            rho: -self.rho,
        }
    }
}

impl Mul<Decimal> for Greeks {
    type Output = Self;

    fn mul(self, rhs: Decimal) -> Self {
        self.scale(rhs)
    }
}

impl std::iter::Sum for Greeks {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::zero(), |acc, g| acc + g)
    }
}

impl std::fmt::Display for Greeks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Δ={:.4} Γ={:.6} Θ={:.4} ν={:.4} ρ={:.4}",
            self.delta, self.gamma, self.theta, self.vega, self.rho
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_greeks_creation() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));

        assert_eq!(greeks.delta(), dec!(0.5));
        assert_eq!(greeks.gamma(), dec!(0.02));
        assert_eq!(greeks.theta(), dec!(-0.05));
        assert_eq!(greeks.vega(), dec!(0.15));
        assert_eq!(greeks.rho(), dec!(0.01));
    }

    #[test]
    fn test_greeks_zero() {
        let greeks = Greeks::zero();

        assert!(greeks.is_zero());
        assert_eq!(greeks.delta(), Decimal::ZERO);
    }

    #[test]
    fn test_greeks_addition() {
        let g1 = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        let g2 = Greeks::new(dec!(0.3), dec!(0.01), dec!(-0.03), dec!(0.10), dec!(0.02));

        let sum = g1 + g2;

        assert_eq!(sum.delta(), dec!(0.8));
        assert_eq!(sum.gamma(), dec!(0.03));
        assert_eq!(sum.theta(), dec!(-0.08));
        assert_eq!(sum.vega(), dec!(0.25));
        assert_eq!(sum.rho(), dec!(0.03));
    }

    #[test]
    fn test_greeks_negation() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        let neg = -greeks;

        assert_eq!(neg.delta(), dec!(-0.5));
        assert_eq!(neg.gamma(), dec!(-0.02));
        assert_eq!(neg.theta(), dec!(0.05));
        assert_eq!(neg.vega(), dec!(-0.15));
        assert_eq!(neg.rho(), dec!(-0.01));
    }

    #[test]
    fn test_greeks_scale() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        let scaled = greeks.scale(dec!(10));

        assert_eq!(scaled.delta(), dec!(5));
        assert_eq!(scaled.gamma(), dec!(0.2));
        assert_eq!(scaled.theta(), dec!(-0.5));
        assert_eq!(scaled.vega(), dec!(1.5));
        assert_eq!(scaled.rho(), dec!(0.1));
    }

    #[test]
    fn test_greeks_mul() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        let scaled = greeks * dec!(2);

        assert_eq!(scaled.delta(), dec!(1));
    }

    #[test]
    fn test_greeks_sum() {
        let greeks_list = vec![
            Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01)),
            Greeks::new(dec!(0.3), dec!(0.01), dec!(-0.03), dec!(0.10), dec!(0.02)),
            Greeks::new(dec!(0.2), dec!(0.01), dec!(-0.02), dec!(0.05), dec!(0.01)),
        ];

        let total: Greeks = greeks_list.into_iter().sum();

        assert_eq!(total.delta(), dec!(1.0));
        assert_eq!(total.gamma(), dec!(0.04));
        assert_eq!(total.theta(), dec!(-0.10));
        assert_eq!(total.vega(), dec!(0.30));
        assert_eq!(total.rho(), dec!(0.04));
    }

    #[test]
    fn test_dollar_delta() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        let dollar_delta = greeks.dollar_delta(dec!(100), dec!(100));

        // 0.5 * 100 * 100 = 5000
        assert_eq!(dollar_delta, dec!(5000));
    }

    #[test]
    fn test_estimate_pnl() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));

        // Spot up $10, vol up 1%, 1 day passed
        let pnl = greeks.estimate_pnl(dec!(10), dec!(0.01), dec!(1));

        // Delta P&L: 0.5 * 10 = 5
        // Gamma P&L: 0.02 * 100 / 2 = 1
        // Theta P&L: -0.05 * 1 = -0.05
        // Vega P&L: 0.15 * 0.01 = 0.0015
        // Total ≈ 5.9515
        assert!(pnl > dec!(5.9) && pnl < dec!(6.0));
    }

    #[test]
    fn test_long_short_delta() {
        let long = Greeks::new(dec!(0.5), dec!(0), dec!(0), dec!(0), dec!(0));
        let short = Greeks::new(dec!(-0.5), dec!(0), dec!(0), dec!(0), dec!(0));
        let neutral = Greeks::new(dec!(0), dec!(0), dec!(0), dec!(0), dec!(0));

        assert!(long.is_long_delta());
        assert!(!long.is_short_delta());

        assert!(short.is_short_delta());
        assert!(!short.is_long_delta());

        assert!(!neutral.is_long_delta());
        assert!(!neutral.is_short_delta());
    }

    #[test]
    fn test_greeks_display() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        let display = format!("{}", greeks);

        assert!(display.contains("Δ="));
        assert!(display.contains("Γ="));
        assert!(display.contains("Θ="));
        assert!(display.contains("ν="));
        assert!(display.contains("ρ="));
    }

    #[test]
    fn test_greeks_serialization() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));

        let json = serde_json::to_string(&greeks).unwrap();
        let deserialized: Greeks = serde_json::from_str(&json).unwrap();

        assert_eq!(greeks, deserialized);
    }
}
