//! P&L calculator for options portfolios.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// P&L calculator for tracking realized and unrealized P&L.
#[derive(Debug, Clone, Default)]
pub struct PnLCalculator {
    /// Total realized P&L.
    realized_pnl: Decimal,
    /// Total unrealized P&L.
    unrealized_pnl: Decimal,
    /// Total fees paid.
    total_fees: Decimal,
}

impl PnLCalculator {
    /// Creates a new P&L calculator.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            realized_pnl: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            total_fees: Decimal::ZERO,
        }
    }

    /// Returns the realized P&L.
    #[must_use]
    pub const fn realized_pnl(&self) -> Decimal {
        self.realized_pnl
    }

    /// Returns the unrealized P&L.
    #[must_use]
    pub const fn unrealized_pnl(&self) -> Decimal {
        self.unrealized_pnl
    }

    /// Returns the total fees.
    #[must_use]
    pub const fn total_fees(&self) -> Decimal {
        self.total_fees
    }

    /// Returns the total P&L (realized + unrealized - fees).
    #[must_use]
    pub fn total_pnl(&self) -> Decimal {
        self.realized_pnl + self.unrealized_pnl - self.total_fees
    }

    /// Returns the net P&L (realized - fees).
    #[must_use]
    pub fn net_realized_pnl(&self) -> Decimal {
        self.realized_pnl - self.total_fees
    }

    /// Adds realized P&L.
    pub fn add_realized(&mut self, amount: Decimal) {
        self.realized_pnl += amount;
    }

    /// Updates unrealized P&L.
    pub fn update_unrealized(&mut self, amount: Decimal) {
        self.unrealized_pnl = amount;
    }

    /// Adds fees.
    pub fn add_fees(&mut self, amount: Decimal) {
        self.total_fees += amount;
    }

    /// Resets all P&L tracking.
    pub fn reset(&mut self) {
        self.realized_pnl = Decimal::ZERO;
        self.unrealized_pnl = Decimal::ZERO;
        self.total_fees = Decimal::ZERO;
    }
}

/// Snapshot of P&L state at a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PnLSnapshot {
    /// Realized P&L.
    pub realized: Decimal,
    /// Unrealized P&L.
    pub unrealized: Decimal,
    /// Total fees.
    pub fees: Decimal,
    /// Timestamp in milliseconds.
    pub timestamp_ms: u64,
}

impl PnLSnapshot {
    /// Returns the total P&L.
    #[must_use]
    #[allow(dead_code)]
    pub fn total(&self) -> Decimal {
        self.realized + self.unrealized - self.fees
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_pnl_calculator() {
        let mut calc = PnLCalculator::new();

        calc.add_realized(dec!(100));
        calc.update_unrealized(dec!(50));
        calc.add_fees(dec!(10));

        assert_eq!(calc.realized_pnl(), dec!(100));
        assert_eq!(calc.unrealized_pnl(), dec!(50));
        assert_eq!(calc.total_fees(), dec!(10));
        assert_eq!(calc.total_pnl(), dec!(140)); // 100 + 50 - 10
        assert_eq!(calc.net_realized_pnl(), dec!(90)); // 100 - 10
    }
}
