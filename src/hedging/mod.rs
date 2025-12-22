//! Delta hedging engine module.
//!
//! This module provides delta hedging functionality for options portfolios,
//! including hedge calculation, execution, and monitoring.
//!
//! ## Components
//!
//! - [`HedgeParams`]: Parameters for hedge calculation
//! - [`HedgeOrder`]: Represents a hedge order to execute
//! - [`DeltaHedger`]: Main hedging engine

mod hedger;
mod order;
mod params;

pub use hedger::DeltaHedger;
pub use order::HedgeOrder;
pub use params::HedgeParams;
