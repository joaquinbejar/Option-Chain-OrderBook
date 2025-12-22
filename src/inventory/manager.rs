//! Inventory manager for tracking positions across the option chain.
//!
//! This module provides the [`InventoryManager`] for managing positions
//! across all options in a chain with aggregation at multiple levels.

use super::limits::{LimitBreach, PositionLimits};
use super::position::Position;
use crate::pricing::Greeks;
use crate::{Error, Result};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Manages positions across the entire option chain.
///
/// Tracks positions at multiple aggregation levels:
/// - Per-option contract
/// - Per-strike (call + put combined)
/// - Per-expiration
/// - Per-underlying (total portfolio)
#[derive(Debug, Clone)]
pub struct InventoryManager {
    /// The underlying asset symbol.
    underlying: String,
    /// Positions indexed by option symbol.
    positions: HashMap<String, Position>,
    /// Position limits configuration.
    limits: PositionLimits,
    /// Default contract multiplier for new positions.
    default_multiplier: Decimal,
}

impl InventoryManager {
    /// Creates a new inventory manager.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `limits` - Position limits configuration
    /// * `default_multiplier` - Default contract multiplier
    #[must_use]
    pub fn new(
        underlying: impl Into<String>,
        limits: PositionLimits,
        default_multiplier: Decimal,
    ) -> Self {
        Self {
            underlying: underlying.into(),
            positions: HashMap::new(),
            limits,
            default_multiplier,
        }
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the position limits.
    #[must_use]
    pub const fn limits(&self) -> &PositionLimits {
        &self.limits
    }

    /// Returns the number of positions.
    #[must_use]
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    /// Returns true if there are no positions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Gets a position by symbol.
    #[must_use]
    pub fn get_position(&self, symbol: &str) -> Option<&Position> {
        self.positions.get(symbol)
    }

    /// Gets a mutable position by symbol.
    pub fn get_position_mut(&mut self, symbol: &str) -> Option<&mut Position> {
        self.positions.get_mut(symbol)
    }

    /// Gets or creates a position for the given symbol.
    pub fn get_or_create_position(&mut self, symbol: impl Into<String>) -> &mut Position {
        let symbol = symbol.into();
        self.positions
            .entry(symbol)
            .or_insert_with(|| Position::new(self.default_multiplier))
    }

    /// Records a trade for a position.
    ///
    /// # Arguments
    ///
    /// * `symbol` - Option contract symbol
    /// * `quantity` - Trade quantity (positive = buy, negative = sell)
    /// * `price` - Execution price
    /// * `timestamp_ms` - Execution timestamp
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, or an error if limits would be exceeded.
    pub fn record_trade(
        &mut self,
        symbol: impl Into<String>,
        quantity: Decimal,
        price: Decimal,
        timestamp_ms: u64,
    ) -> Result<()> {
        let symbol = symbol.into();

        // Check if trade would exceed limits
        let current_qty = self
            .positions
            .get(&symbol)
            .map(Position::quantity)
            .unwrap_or(Decimal::ZERO);
        let new_qty = current_qty + quantity;

        if self.limits.exceeds_per_option(new_qty) {
            return Err(Error::inventory_limit_exceeded(
                "per_option",
                self.limits.per_option(),
                new_qty.abs(),
            ));
        }

        // Record the trade
        let position = self.get_or_create_position(symbol);
        position.add(quantity, price, timestamp_ms);

        Ok(())
    }

    /// Updates Greeks for a position.
    ///
    /// # Arguments
    ///
    /// * `symbol` - Option contract symbol
    /// * `greeks` - New Greeks values
    /// * `timestamp_ms` - Update timestamp
    pub fn update_greeks(&mut self, symbol: &str, greeks: Greeks, timestamp_ms: u64) {
        if let Some(position) = self.positions.get_mut(symbol) {
            position.update_greeks(greeks, timestamp_ms);
        }
    }

    /// Returns the total Greeks across all positions.
    #[must_use]
    pub fn total_greeks(&self) -> Greeks {
        self.positions.values().map(|p| *p.greeks()).sum()
    }

    /// Returns the total realized P&L across all positions.
    #[must_use]
    pub fn total_realized_pnl(&self) -> Decimal {
        self.positions.values().map(Position::realized_pnl).sum()
    }

    /// Checks for any Greek limit breaches.
    ///
    /// # Arguments
    ///
    /// * `spot` - Current spot price
    /// * `multiplier` - Contract multiplier
    #[must_use]
    pub fn check_greek_limits(&self, spot: Decimal, multiplier: Decimal) -> Vec<LimitBreach> {
        let total_greeks = self.total_greeks();
        self.limits
            .check_greek_limits(&total_greeks, spot, multiplier)
    }

    /// Returns an iterator over all positions.
    pub fn positions(&self) -> impl Iterator<Item = (&str, &Position)> {
        self.positions.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Clears all positions.
    pub fn clear(&mut self) {
        self.positions.clear();
    }

    /// Removes a position by symbol.
    pub fn remove_position(&mut self, symbol: &str) -> Option<Position> {
        self.positions.remove(symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_inventory_manager_creation() {
        let manager = InventoryManager::new("BTC", PositionLimits::small(), dec!(1));

        assert_eq!(manager.underlying(), "BTC");
        assert!(manager.is_empty());
    }

    #[test]
    fn test_record_trade() {
        let mut manager = InventoryManager::new("BTC", PositionLimits::small(), dec!(100));

        manager
            .record_trade("BTC-20240329-50000-C", dec!(10), dec!(5.50), 1000)
            .unwrap();

        let position = manager.get_position("BTC-20240329-50000-C").unwrap();
        assert_eq!(position.quantity(), dec!(10));
        assert_eq!(position.average_price(), dec!(5.50));
    }

    #[test]
    fn test_limit_exceeded() {
        let limits = PositionLimits::new(dec!(10), dec!(20), dec!(50), dec!(100));
        let mut manager = InventoryManager::new("BTC", limits, dec!(100));

        // This should succeed
        manager
            .record_trade("BTC-20240329-50000-C", dec!(10), dec!(5.50), 1000)
            .unwrap();

        // This should fail (would exceed per-option limit of 10)
        let result = manager.record_trade("BTC-20240329-50000-C", dec!(5), dec!(5.50), 2000);
        assert!(result.is_err());
    }

    #[test]
    fn test_total_greeks() {
        let mut manager = InventoryManager::new("BTC", PositionLimits::small(), dec!(100));

        manager
            .record_trade("BTC-20240329-50000-C", dec!(10), dec!(5.50), 1000)
            .unwrap();
        manager
            .record_trade("BTC-20240329-50000-P", dec!(5), dec!(3.00), 1000)
            .unwrap();

        let greeks1 = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));
        let greeks2 = Greeks::new(dec!(-0.3), dec!(0.01), dec!(-0.03), dec!(0.10), dec!(0.02));

        manager.update_greeks("BTC-20240329-50000-C", greeks1, 2000);
        manager.update_greeks("BTC-20240329-50000-P", greeks2, 2000);

        let total = manager.total_greeks();
        assert_eq!(total.delta(), dec!(0.2)); // 0.5 - 0.3
    }
}
