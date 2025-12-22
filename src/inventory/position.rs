//! Position tracking for option contracts.
//!
//! This module provides the [`Position`] structure for tracking positions
//! in individual option contracts.

use crate::pricing::Greeks;
use rust_decimal::Decimal;
use rust_decimal::prelude::Signed;
use serde::{Deserialize, Serialize};

/// Represents a position in a single option contract.
///
/// Tracks quantity, average cost, and associated Greeks for a position.
/// Positive quantity indicates a long position, negative indicates short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// Position quantity (positive = long, negative = short).
    quantity: Decimal,
    /// Average entry price per contract.
    average_price: Decimal,
    /// Total cost basis (quantity * average_price * multiplier).
    cost_basis: Decimal,
    /// Contract multiplier.
    multiplier: Decimal,
    /// Current Greeks for this position.
    greeks: Greeks,
    /// Realized P&L from closed portions.
    realized_pnl: Decimal,
    /// Last update timestamp in milliseconds.
    last_update_ms: u64,
}

impl Position {
    /// Creates a new empty position.
    ///
    /// # Arguments
    ///
    /// * `multiplier` - Contract multiplier (e.g., 100 for equity options)
    #[must_use]
    pub const fn new(multiplier: Decimal) -> Self {
        Self {
            quantity: Decimal::ZERO,
            average_price: Decimal::ZERO,
            cost_basis: Decimal::ZERO,
            multiplier,
            greeks: Greeks::zero(),
            realized_pnl: Decimal::ZERO,
            last_update_ms: 0,
        }
    }

    /// Creates a position with initial values.
    ///
    /// # Arguments
    ///
    /// * `quantity` - Initial quantity
    /// * `price` - Entry price
    /// * `multiplier` - Contract multiplier
    /// * `timestamp_ms` - Entry timestamp
    #[must_use]
    pub fn with_entry(
        quantity: Decimal,
        price: Decimal,
        multiplier: Decimal,
        timestamp_ms: u64,
    ) -> Self {
        let cost_basis = quantity * price * multiplier;
        Self {
            quantity,
            average_price: price,
            cost_basis,
            multiplier,
            greeks: Greeks::zero(),
            realized_pnl: Decimal::ZERO,
            last_update_ms: timestamp_ms,
        }
    }

    /// Returns the position quantity.
    #[must_use]
    pub const fn quantity(&self) -> Decimal {
        self.quantity
    }

    /// Returns the average entry price.
    #[must_use]
    pub const fn average_price(&self) -> Decimal {
        self.average_price
    }

    /// Returns the cost basis.
    #[must_use]
    pub const fn cost_basis(&self) -> Decimal {
        self.cost_basis
    }

    /// Returns the contract multiplier.
    #[must_use]
    pub const fn multiplier(&self) -> Decimal {
        self.multiplier
    }

    /// Returns the position Greeks.
    #[must_use]
    pub const fn greeks(&self) -> &Greeks {
        &self.greeks
    }

    /// Returns the realized P&L.
    #[must_use]
    pub const fn realized_pnl(&self) -> Decimal {
        self.realized_pnl
    }

    /// Returns the last update timestamp.
    #[must_use]
    pub const fn last_update_ms(&self) -> u64 {
        self.last_update_ms
    }

    /// Returns true if the position is flat (zero quantity).
    #[must_use]
    pub fn is_flat(&self) -> bool {
        self.quantity.is_zero()
    }

    /// Returns true if the position is long.
    #[must_use]
    pub fn is_long(&self) -> bool {
        self.quantity > Decimal::ZERO
    }

    /// Returns true if the position is short.
    #[must_use]
    pub fn is_short(&self) -> bool {
        self.quantity < Decimal::ZERO
    }

    /// Returns the absolute quantity.
    #[must_use]
    pub fn abs_quantity(&self) -> Decimal {
        self.quantity.abs()
    }

    /// Returns the notional value at current price.
    ///
    /// # Arguments
    ///
    /// * `current_price` - Current market price
    #[must_use]
    pub fn notional_value(&self, current_price: Decimal) -> Decimal {
        self.quantity * current_price * self.multiplier
    }

    /// Returns the unrealized P&L at current price.
    ///
    /// # Arguments
    ///
    /// * `current_price` - Current market price
    #[must_use]
    pub fn unrealized_pnl(&self, current_price: Decimal) -> Decimal {
        let current_value = self.notional_value(current_price);
        current_value - self.cost_basis
    }

    /// Returns the total P&L (realized + unrealized).
    ///
    /// # Arguments
    ///
    /// * `current_price` - Current market price
    #[must_use]
    pub fn total_pnl(&self, current_price: Decimal) -> Decimal {
        self.realized_pnl + self.unrealized_pnl(current_price)
    }

    /// Updates the position Greeks.
    ///
    /// # Arguments
    ///
    /// * `greeks` - New Greeks values
    /// * `timestamp_ms` - Update timestamp
    pub fn update_greeks(&mut self, greeks: Greeks, timestamp_ms: u64) {
        self.greeks = greeks;
        self.last_update_ms = timestamp_ms;
    }

    /// Adds to the position (buy).
    ///
    /// Updates average price using weighted average.
    ///
    /// # Arguments
    ///
    /// * `quantity` - Quantity to add (positive)
    /// * `price` - Execution price
    /// * `timestamp_ms` - Execution timestamp
    pub fn add(&mut self, quantity: Decimal, price: Decimal, timestamp_ms: u64) {
        if quantity.is_zero() {
            return;
        }

        let trade_value = quantity * price * self.multiplier;

        if self.quantity.signum() == quantity.signum() || self.quantity.is_zero() {
            // Adding to existing direction or opening new position
            let new_quantity = self.quantity + quantity;
            if !new_quantity.is_zero() {
                self.average_price =
                    (self.cost_basis + trade_value) / (new_quantity * self.multiplier);
            }
            self.quantity = new_quantity;
            self.cost_basis += trade_value;
        } else {
            // Reducing or flipping position
            self.reduce_or_flip(quantity, price, timestamp_ms);
        }

        self.last_update_ms = timestamp_ms;
    }

    /// Reduces the position (sell for long, buy for short).
    ///
    /// # Arguments
    ///
    /// * `quantity` - Quantity to reduce (sign should be opposite to position)
    /// * `price` - Execution price
    /// * `timestamp_ms` - Execution timestamp
    pub fn reduce(&mut self, quantity: Decimal, price: Decimal, timestamp_ms: u64) {
        self.add(-quantity, price, timestamp_ms);
    }

    /// Internal method to handle position reduction or flip.
    fn reduce_or_flip(&mut self, quantity: Decimal, price: Decimal, timestamp_ms: u64) {
        let close_quantity = quantity.abs().min(self.quantity.abs());
        let close_sign = if self.quantity > Decimal::ZERO {
            Decimal::ONE
        } else {
            -Decimal::ONE
        };

        // Calculate realized P&L on closed portion
        let close_value = close_quantity * price * self.multiplier;
        let close_cost = close_quantity * self.average_price * self.multiplier;
        self.realized_pnl += (close_value - close_cost) * close_sign;

        // Update position
        let remaining = quantity.abs() - close_quantity;
        if remaining.is_zero() {
            // Just reduced, didn't flip
            self.quantity += quantity;
            self.cost_basis = self.quantity * self.average_price * self.multiplier;
        } else {
            // Flipped position
            self.quantity = remaining * quantity.signum();
            self.average_price = price;
            self.cost_basis = self.quantity * price * self.multiplier;
        }

        self.last_update_ms = timestamp_ms;
    }

    /// Closes the entire position.
    ///
    /// # Arguments
    ///
    /// * `price` - Closing price
    /// * `timestamp_ms` - Execution timestamp
    ///
    /// # Returns
    ///
    /// The realized P&L from closing.
    pub fn close(&mut self, price: Decimal, timestamp_ms: u64) -> Decimal {
        if self.is_flat() {
            return Decimal::ZERO;
        }

        let close_pnl = self.unrealized_pnl(price);
        self.realized_pnl += close_pnl;
        self.quantity = Decimal::ZERO;
        self.average_price = Decimal::ZERO;
        self.cost_basis = Decimal::ZERO;
        self.last_update_ms = timestamp_ms;

        close_pnl
    }

    /// Resets the position to flat with zero P&L.
    pub fn reset(&mut self) {
        self.quantity = Decimal::ZERO;
        self.average_price = Decimal::ZERO;
        self.cost_basis = Decimal::ZERO;
        self.greeks = Greeks::zero();
        self.realized_pnl = Decimal::ZERO;
        self.last_update_ms = 0;
    }
}

impl Default for Position {
    fn default() -> Self {
        Self::new(Decimal::ONE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_position_creation() {
        let pos = Position::new(dec!(100));

        assert!(pos.is_flat());
        assert_eq!(pos.quantity(), Decimal::ZERO);
        assert_eq!(pos.multiplier(), dec!(100));
    }

    #[test]
    fn test_position_with_entry() {
        let pos = Position::with_entry(dec!(10), dec!(5.50), dec!(100), 1000);

        assert!(pos.is_long());
        assert_eq!(pos.quantity(), dec!(10));
        assert_eq!(pos.average_price(), dec!(5.50));
        assert_eq!(pos.cost_basis(), dec!(5500)); // 10 * 5.50 * 100
    }

    #[test]
    fn test_add_to_long() {
        let mut pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);

        pos.add(dec!(10), dec!(6.00), 2000);

        assert_eq!(pos.quantity(), dec!(20));
        // Average: (5000 + 6000) / 20 = 5.50
        assert_eq!(pos.average_price(), dec!(5.50));
        assert_eq!(pos.cost_basis(), dec!(11000));
    }

    #[test]
    fn test_reduce_long() {
        let mut pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);

        pos.add(dec!(-5), dec!(6.00), 2000);

        assert_eq!(pos.quantity(), dec!(5));
        // Realized P&L: 5 * (6.00 - 5.00) * 100 = 500
        assert_eq!(pos.realized_pnl(), dec!(500));
    }

    #[test]
    fn test_close_position() {
        let mut pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);

        let pnl = pos.close(dec!(6.00), 2000);

        assert!(pos.is_flat());
        // P&L: 10 * (6.00 - 5.00) * 100 = 1000
        assert_eq!(pnl, dec!(1000));
        assert_eq!(pos.realized_pnl(), dec!(1000));
    }

    #[test]
    fn test_short_position() {
        let mut pos = Position::new(dec!(100));

        pos.add(dec!(-10), dec!(5.00), 1000);

        assert!(pos.is_short());
        assert_eq!(pos.quantity(), dec!(-10));
        assert_eq!(pos.cost_basis(), dec!(-5000));
    }

    #[test]
    fn test_unrealized_pnl_long() {
        let pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);

        // Price up: profit
        assert_eq!(pos.unrealized_pnl(dec!(6.00)), dec!(1000));

        // Price down: loss
        assert_eq!(pos.unrealized_pnl(dec!(4.00)), dec!(-1000));
    }

    #[test]
    fn test_unrealized_pnl_short() {
        let mut pos = Position::new(dec!(100));
        pos.add(dec!(-10), dec!(5.00), 1000);

        // Price down: profit for short
        assert_eq!(pos.unrealized_pnl(dec!(4.00)), dec!(1000));

        // Price up: loss for short
        assert_eq!(pos.unrealized_pnl(dec!(6.00)), dec!(-1000));
    }

    #[test]
    fn test_flip_position() {
        let mut pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);

        // Sell 15 to flip from +10 to -5
        pos.add(dec!(-15), dec!(6.00), 2000);

        assert!(pos.is_short());
        assert_eq!(pos.quantity(), dec!(-5));
        // Realized P&L from closing 10 long: 10 * (6.00 - 5.00) * 100 = 1000
        assert_eq!(pos.realized_pnl(), dec!(1000));
        // New average price is the flip price
        assert_eq!(pos.average_price(), dec!(6.00));
    }

    #[test]
    fn test_update_greeks() {
        let mut pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);
        let greeks = Greeks::new(dec!(0.5), dec!(0.02), dec!(-0.05), dec!(0.15), dec!(0.01));

        pos.update_greeks(greeks, 2000);

        assert_eq!(pos.greeks().delta(), dec!(0.5));
        assert_eq!(pos.last_update_ms(), 2000);
    }

    #[test]
    fn test_total_pnl() {
        let mut pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);

        // Partial close with profit
        pos.add(dec!(-5), dec!(6.00), 2000);

        // Realized: 500, Unrealized at 6.50: 5 * (6.50 - 5.00) * 100 = 750
        let total = pos.total_pnl(dec!(6.50));
        assert_eq!(total, dec!(1250));
    }

    #[test]
    fn test_reset() {
        let mut pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);
        pos.close(dec!(6.00), 2000);

        pos.reset();

        assert!(pos.is_flat());
        assert_eq!(pos.realized_pnl(), Decimal::ZERO);
    }

    #[test]
    fn test_serialization() {
        let pos = Position::with_entry(dec!(10), dec!(5.00), dec!(100), 1000);

        let json = serde_json::to_string(&pos).unwrap();
        let deserialized: Position = serde_json::from_str(&json).unwrap();

        assert_eq!(pos, deserialized);
    }
}
