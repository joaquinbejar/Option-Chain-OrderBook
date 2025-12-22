//! Risk limits configuration.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Risk limits for market making operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskLimits {
    /// Maximum loss per day.
    pub max_daily_loss: Decimal,
    /// Maximum drawdown from peak.
    pub max_drawdown: Decimal,
    /// Maximum position value.
    pub max_position_value: Decimal,
    /// Maximum delta exposure.
    pub max_delta: Decimal,
    /// Maximum gamma exposure.
    pub max_gamma: Decimal,
    /// Maximum vega exposure.
    pub max_vega: Decimal,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_daily_loss: Decimal::from(10000),
            max_drawdown: Decimal::from(50000),
            max_position_value: Decimal::from(1000000),
            max_delta: Decimal::from(100000),
            max_gamma: Decimal::from(10000),
            max_vega: Decimal::from(50000),
        }
    }
}
