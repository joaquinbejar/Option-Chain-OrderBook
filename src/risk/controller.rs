//! Risk controller for monitoring and enforcing limits.

use super::limits::RiskLimits;
use crate::pricing::Greeks;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Risk controller for monitoring portfolio risk.
#[derive(Debug, Clone)]
pub struct RiskController {
    /// Risk limits configuration.
    limits: RiskLimits,
    /// Current daily P&L.
    daily_pnl: Decimal,
    /// Peak P&L for drawdown calculation.
    peak_pnl: Decimal,
    /// Current position value.
    position_value: Decimal,
    /// Whether trading is halted.
    is_halted: bool,
    /// Reason for halt if halted.
    halt_reason: Option<String>,
}

impl RiskController {
    /// Creates a new risk controller with the given limits.
    #[must_use]
    pub const fn new(limits: RiskLimits) -> Self {
        Self {
            limits,
            daily_pnl: Decimal::ZERO,
            peak_pnl: Decimal::ZERO,
            position_value: Decimal::ZERO,
            is_halted: false,
            halt_reason: None,
        }
    }

    /// Returns the risk limits.
    #[must_use]
    pub const fn limits(&self) -> &RiskLimits {
        &self.limits
    }

    /// Returns true if trading is halted.
    #[must_use]
    pub const fn is_halted(&self) -> bool {
        self.is_halted
    }

    /// Returns the halt reason if halted.
    #[must_use]
    pub fn halt_reason(&self) -> Option<&str> {
        self.halt_reason.as_deref()
    }

    /// Updates the daily P&L and checks limits.
    pub fn update_pnl(&mut self, pnl: Decimal) {
        self.daily_pnl = pnl;
        if pnl > self.peak_pnl {
            self.peak_pnl = pnl;
        }
        self.check_pnl_limits();
    }

    /// Updates the position value.
    pub fn update_position_value(&mut self, value: Decimal) {
        self.position_value = value;
        self.check_position_limits();
    }

    /// Checks Greek limits and returns any breaches.
    #[must_use]
    pub fn check_greek_limits(&self, greeks: &Greeks) -> Vec<RiskBreach> {
        let mut breaches = Vec::new();

        if greeks.delta().abs() > self.limits.max_delta {
            breaches.push(RiskBreach::Delta {
                current: greeks.delta().abs(),
                limit: self.limits.max_delta,
            });
        }

        if greeks.gamma().abs() > self.limits.max_gamma {
            breaches.push(RiskBreach::Gamma {
                current: greeks.gamma().abs(),
                limit: self.limits.max_gamma,
            });
        }

        if greeks.vega().abs() > self.limits.max_vega {
            breaches.push(RiskBreach::Vega {
                current: greeks.vega().abs(),
                limit: self.limits.max_vega,
            });
        }

        breaches
    }

    /// Halts trading with the given reason.
    pub fn halt(&mut self, reason: impl Into<String>) {
        self.is_halted = true;
        self.halt_reason = Some(reason.into());
    }

    /// Resumes trading.
    pub fn resume(&mut self) {
        self.is_halted = false;
        self.halt_reason = None;
    }

    /// Resets daily P&L tracking (call at start of day).
    pub fn reset_daily(&mut self) {
        self.daily_pnl = Decimal::ZERO;
        self.peak_pnl = Decimal::ZERO;
    }

    fn check_pnl_limits(&mut self) {
        // Check daily loss limit
        if self.daily_pnl < -self.limits.max_daily_loss {
            self.halt(format!(
                "Daily loss limit exceeded: {} < -{}",
                self.daily_pnl, self.limits.max_daily_loss
            ));
        }

        // Check drawdown limit
        let drawdown = self.peak_pnl - self.daily_pnl;
        if drawdown > self.limits.max_drawdown {
            self.halt(format!(
                "Drawdown limit exceeded: {} > {}",
                drawdown, self.limits.max_drawdown
            ));
        }
    }

    fn check_position_limits(&mut self) {
        if self.position_value.abs() > self.limits.max_position_value {
            self.halt(format!(
                "Position value limit exceeded: {} > {}",
                self.position_value.abs(),
                self.limits.max_position_value
            ));
        }
    }
}

impl Default for RiskController {
    fn default() -> Self {
        Self::new(RiskLimits::default())
    }
}

/// Represents a risk limit breach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskBreach {
    /// Delta limit breached.
    Delta {
        /// Current value.
        current: Decimal,
        /// Limit value.
        limit: Decimal,
    },
    /// Gamma limit breached.
    Gamma {
        /// Current value.
        current: Decimal,
        /// Limit value.
        limit: Decimal,
    },
    /// Vega limit breached.
    Vega {
        /// Current value.
        current: Decimal,
        /// Limit value.
        limit: Decimal,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_risk_controller_creation() {
        let controller = RiskController::default();
        assert!(!controller.is_halted());
    }

    #[test]
    fn test_daily_loss_halt() {
        let limits = RiskLimits {
            max_daily_loss: dec!(1000),
            ..Default::default()
        };
        let mut controller = RiskController::new(limits);

        controller.update_pnl(dec!(-1500));

        assert!(controller.is_halted());
        assert!(controller.halt_reason().unwrap().contains("Daily loss"));
    }

    #[test]
    fn test_greek_limits() {
        let controller = RiskController::default();
        let greeks = Greeks::new(dec!(200000), dec!(0), dec!(0), dec!(0), dec!(0));

        let breaches = controller.check_greek_limits(&greeks);

        assert!(!breaches.is_empty());
        assert!(matches!(breaches[0], RiskBreach::Delta { .. }));
    }
}
