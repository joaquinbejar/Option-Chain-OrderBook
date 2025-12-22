//! Hedge order representation.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Represents a hedge order to execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HedgeOrder {
    /// Symbol to hedge (usually the underlying).
    pub symbol: String,
    /// Quantity to trade (positive = buy, negative = sell).
    pub quantity: Decimal,
    /// Limit price (if using limit orders).
    pub limit_price: Option<Decimal>,
    /// Reason for the hedge.
    pub reason: HedgeReason,
    /// Timestamp when hedge was calculated.
    pub timestamp_ms: u64,
}

/// Reason for generating a hedge order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HedgeReason {
    /// Delta exceeded threshold.
    DeltaThreshold,
    /// Scheduled rebalance.
    ScheduledRebalance,
    /// Manual trigger.
    Manual,
    /// Risk limit breach.
    RiskLimitBreach,
}

impl HedgeOrder {
    /// Creates a new hedge order.
    #[must_use]
    pub fn new(
        symbol: impl Into<String>,
        quantity: Decimal,
        limit_price: Option<Decimal>,
        reason: HedgeReason,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            quantity,
            limit_price,
            reason,
            timestamp_ms,
        }
    }

    /// Returns true if this is a buy order.
    #[must_use]
    pub fn is_buy(&self) -> bool {
        self.quantity > Decimal::ZERO
    }

    /// Returns true if this is a sell order.
    #[must_use]
    pub fn is_sell(&self) -> bool {
        self.quantity < Decimal::ZERO
    }

    /// Returns the absolute quantity.
    #[must_use]
    pub fn abs_quantity(&self) -> Decimal {
        self.quantity.abs()
    }
}
