//! P&L calculation module.
//!
//! This module provides profit and loss calculation and attribution
//! for options portfolios.

mod attribution;
mod calculator;

pub use attribution::PnLAttribution;
pub use calculator::PnLCalculator;
