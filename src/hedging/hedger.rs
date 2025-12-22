//! Delta hedging engine.

use super::order::{HedgeOrder, HedgeReason};
use super::params::HedgeParams;
use crate::pricing::Greeks;
use rust_decimal::Decimal;

/// Delta hedging engine.
///
/// Calculates and generates hedge orders based on portfolio Greeks
/// and configured parameters.
#[derive(Debug, Clone)]
pub struct DeltaHedger {
    /// Hedging parameters.
    params: HedgeParams,
    /// Current portfolio delta.
    current_delta: Decimal,
    /// Last hedge timestamp.
    last_hedge_ms: u64,
}

impl DeltaHedger {
    /// Creates a new delta hedger with the given parameters.
    #[must_use]
    pub const fn new(params: HedgeParams) -> Self {
        Self {
            params,
            current_delta: Decimal::ZERO,
            last_hedge_ms: 0,
        }
    }

    /// Updates the current portfolio delta.
    pub fn update_delta(&mut self, greeks: &Greeks) {
        self.current_delta = greeks.delta();
    }

    /// Returns the current delta.
    #[must_use]
    pub const fn current_delta(&self) -> Decimal {
        self.current_delta
    }

    /// Returns the delta deviation from target.
    #[must_use]
    pub fn delta_deviation(&self) -> Decimal {
        self.current_delta - self.params.target_delta
    }

    /// Returns true if a hedge is needed based on threshold.
    #[must_use]
    pub fn needs_hedge(&self) -> bool {
        self.delta_deviation().abs() >= self.params.hedge_threshold
    }

    /// Calculates the hedge order if needed.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The underlying symbol to hedge
    /// * `spot_price` - Current spot price for limit order calculation
    /// * `timestamp_ms` - Current timestamp
    ///
    /// # Returns
    ///
    /// `Some(HedgeOrder)` if a hedge is needed, `None` otherwise.
    #[must_use]
    pub fn calculate_hedge(
        &self,
        symbol: impl Into<String>,
        spot_price: Decimal,
        timestamp_ms: u64,
    ) -> Option<HedgeOrder> {
        if !self.needs_hedge() {
            return None;
        }

        let deviation = self.delta_deviation();
        // To neutralize positive delta, we sell; to neutralize negative delta, we buy
        let raw_quantity = -deviation;

        // Clamp to min/max size
        let quantity = if raw_quantity.abs() < self.params.min_hedge_size {
            return None;
        } else if raw_quantity.abs() > self.params.max_hedge_size {
            if raw_quantity > Decimal::ZERO {
                self.params.max_hedge_size
            } else {
                -self.params.max_hedge_size
            }
        } else {
            raw_quantity
        };

        let limit_price = if self.params.use_limit_orders {
            let offset = spot_price * self.params.limit_offset_bps / Decimal::from(10000);
            if quantity > Decimal::ZERO {
                // Buying: bid below mid
                Some(spot_price - offset)
            } else {
                // Selling: ask above mid
                Some(spot_price + offset)
            }
        } else {
            None
        };

        Some(HedgeOrder::new(
            symbol,
            quantity,
            limit_price,
            HedgeReason::DeltaThreshold,
            timestamp_ms,
        ))
    }

    /// Records that a hedge was executed.
    pub fn record_hedge(&mut self, quantity: Decimal, timestamp_ms: u64) {
        self.current_delta += quantity;
        self.last_hedge_ms = timestamp_ms;
    }

    /// Returns the last hedge timestamp.
    #[must_use]
    pub const fn last_hedge_ms(&self) -> u64 {
        self.last_hedge_ms
    }
}

impl Default for DeltaHedger {
    fn default() -> Self {
        Self::new(HedgeParams::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_hedger_creation() {
        let hedger = DeltaHedger::default();
        assert_eq!(hedger.current_delta(), Decimal::ZERO);
        assert!(!hedger.needs_hedge());
    }

    #[test]
    fn test_needs_hedge() {
        let mut hedger = DeltaHedger::default();
        let greeks = Greeks::new(dec!(15), dec!(0), dec!(0), dec!(0), dec!(0));

        hedger.update_delta(&greeks);

        assert!(hedger.needs_hedge()); // 15 > threshold of 10
    }

    #[test]
    fn test_calculate_hedge() {
        let mut hedger = DeltaHedger::default();
        let greeks = Greeks::new(dec!(50), dec!(0), dec!(0), dec!(0), dec!(0));

        hedger.update_delta(&greeks);

        let order = hedger.calculate_hedge("BTC", dec!(50000), 1000);
        assert!(order.is_some());

        let order = order.unwrap();
        assert!(order.is_sell()); // Sell to reduce positive delta
        assert_eq!(order.quantity, dec!(-50));
    }
}
