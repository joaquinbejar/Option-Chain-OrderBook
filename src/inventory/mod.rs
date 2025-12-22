//! Inventory management module.
//!
//! This module provides position tracking and portfolio aggregation for
//! options market making. It tracks positions at multiple levels:
//! per-option, per-strike, per-expiration, and per-underlying.
//!
//! ## Components
//!
//! - [`Position`]: Represents a position in a single option contract
//! - [`PositionLimits`]: Configurable limits for position sizes
//! - [`InventoryManager`]: Manages positions across the option chain
//!
//! ## Position Limits
//!
//! | Limit Type | Description |
//! |------------|-------------|
//! | Per-Option | Maximum position in single option |
//! | Per-Strike | Maximum across call+put at strike |
//! | Per-Expiration | Maximum across all strikes in expiry |
//! | Per-Underlying | Maximum across all options |
//! | Delta | Maximum delta exposure |
//! | Gamma | Maximum gamma exposure |
//! | Vega | Maximum vega exposure |

mod limits;
mod manager;
mod position;

pub use limits::PositionLimits;
pub use manager::InventoryManager;
pub use position::Position;
