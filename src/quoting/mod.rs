//! Quote generation module.
//!
//! This module provides quote generation functionality for market making,
//! including spread calculation, skewing based on inventory, and the
//! Avellaneda-Stoikov optimal market making model.
//!
//! ## Components
//!
//! - [`QuoteParams`]: Parameters for quote generation
//! - [`GeneratedQuote`]: Result of quote generation with bid/ask prices
//! - [`SpreadCalculator`]: Calculates optimal spreads based on market conditions
//!
//! ## Quoting Algorithms
//!
//! The primary quoting algorithm is based on the Avellaneda-Stoikov model:
//!
//! **Reservation Price:**
//! ```text
//! r(s,q,t) = s - q·γ·σ²·(T-t)
//! ```
//!
//! **Optimal Spread:**
//! ```text
//! δ* = γ·σ²·(T-t) + (2/γ)·ln(1 + γ/k)
//! ```

mod generated;
mod params;
mod spread;

pub use generated::GeneratedQuote;
pub use params::QuoteParams;
pub use spread::SpreadCalculator;
