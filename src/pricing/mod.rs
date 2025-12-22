//! Pricing engine module.
//!
//! This module provides pricing functionality for options including theoretical
//! value calculation, Greeks computation, and volatility surface management.
//!
//! ## Components
//!
//! - [`Greeks`]: Container for option Greeks (delta, gamma, theta, vega, rho)
//! - [`PricingParams`]: Parameters required for option pricing
//! - [`TheoreticalValue`]: Result of pricing calculation with Greeks
//!
//! ## Example
//!
//! ```rust,ignore
//! use option_chain_orderbook::pricing::{PricingParams, Greeks};
//!
//! let params = PricingParams::new(spot, strike, expiry, vol, rate);
//! let theo = pricing_engine.calculate(params);
//! ```

mod greeks;
mod params;
mod theo;

pub use greeks::Greeks;
pub use params::PricingParams;
pub use theo::TheoreticalValue;
