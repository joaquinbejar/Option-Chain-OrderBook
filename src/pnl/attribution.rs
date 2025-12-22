//! P&L attribution for options portfolios.

use crate::pricing::Greeks;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// P&L attribution breakdown by source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PnLAttribution {
    /// P&L from delta (underlying price movement).
    pub delta_pnl: Decimal,
    /// P&L from gamma (convexity).
    pub gamma_pnl: Decimal,
    /// P&L from theta (time decay).
    pub theta_pnl: Decimal,
    /// P&L from vega (volatility changes).
    pub vega_pnl: Decimal,
    /// P&L from rho (interest rate changes).
    pub rho_pnl: Decimal,
    /// Unexplained P&L (residual).
    pub unexplained_pnl: Decimal,
}

impl PnLAttribution {
    /// Creates a new P&L attribution with all zeros.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            delta_pnl: Decimal::ZERO,
            gamma_pnl: Decimal::ZERO,
            theta_pnl: Decimal::ZERO,
            vega_pnl: Decimal::ZERO,
            rho_pnl: Decimal::ZERO,
            unexplained_pnl: Decimal::ZERO,
        }
    }

    /// Calculates P&L attribution from Greeks and market changes.
    ///
    /// # Arguments
    ///
    /// * `greeks` - Portfolio Greeks
    /// * `spot_change` - Change in underlying price
    /// * `vol_change` - Change in volatility
    /// * `days_passed` - Number of days passed
    /// * `rate_change` - Change in interest rate
    /// * `actual_pnl` - Actual observed P&L
    #[must_use]
    pub fn calculate(
        greeks: &Greeks,
        spot_change: Decimal,
        vol_change: Decimal,
        days_passed: Decimal,
        rate_change: Decimal,
        actual_pnl: Decimal,
    ) -> Self {
        let delta_pnl = greeks.delta() * spot_change;
        let gamma_pnl = greeks.gamma() * spot_change * spot_change / Decimal::TWO;
        let theta_pnl = greeks.theta() * days_passed;
        let vega_pnl = greeks.vega() * vol_change;
        let rho_pnl = greeks.rho() * rate_change;

        let explained = delta_pnl + gamma_pnl + theta_pnl + vega_pnl + rho_pnl;
        let unexplained_pnl = actual_pnl - explained;

        Self {
            delta_pnl,
            gamma_pnl,
            theta_pnl,
            vega_pnl,
            rho_pnl,
            unexplained_pnl,
        }
    }

    /// Returns the total explained P&L.
    #[must_use]
    pub fn explained_pnl(&self) -> Decimal {
        self.delta_pnl + self.gamma_pnl + self.theta_pnl + self.vega_pnl + self.rho_pnl
    }

    /// Returns the total P&L (explained + unexplained).
    #[must_use]
    pub fn total_pnl(&self) -> Decimal {
        self.explained_pnl() + self.unexplained_pnl
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_pnl_attribution() {
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));

        let attribution = PnLAttribution::calculate(
            &greeks,
            dec!(10),   // spot up $10
            dec!(0.01), // vol up 1%
            dec!(1),    // 1 day passed
            dec!(0),    // no rate change
            dec!(6),    // actual P&L
        );

        // Delta P&L: 0.5 * 10 = 5
        assert_eq!(attribution.delta_pnl, dec!(5));

        // Gamma P&L: 0.02 * 100 / 2 = 1
        assert_eq!(attribution.gamma_pnl, dec!(1));

        // Theta P&L: -0.05 * 1 = -0.05
        assert_eq!(attribution.theta_pnl, dec!(-0.05));

        // Vega P&L: 0.15 * 0.01 = 0.0015
        assert_eq!(attribution.vega_pnl, dec!(0.0015));
    }
}
