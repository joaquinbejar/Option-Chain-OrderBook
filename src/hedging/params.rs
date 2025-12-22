//! Hedge parameters for delta hedging.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Parameters for hedge calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HedgeParams {
    /// Target delta (usually 0 for delta-neutral).
    pub target_delta: Decimal,
    /// Minimum hedge size to execute.
    pub min_hedge_size: Decimal,
    /// Maximum hedge size per order.
    pub max_hedge_size: Decimal,
    /// Delta threshold to trigger hedge.
    pub hedge_threshold: Decimal,
    /// Whether to use limit orders (vs market).
    pub use_limit_orders: bool,
    /// Limit order offset from mid price in bps.
    pub limit_offset_bps: Decimal,
}

impl Default for HedgeParams {
    fn default() -> Self {
        Self {
            target_delta: Decimal::ZERO,
            min_hedge_size: Decimal::ONE,
            max_hedge_size: Decimal::from(100),
            hedge_threshold: Decimal::from(10),
            use_limit_orders: true,
            limit_offset_bps: Decimal::from(5),
        }
    }
}
