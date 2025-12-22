//! Market data handling module.
//!
//! This module provides market data types and handlers for processing
//! real-time market data feeds.

mod handler;
mod types;

pub use handler::MarketDataHandler;
pub use types::{MarketDataUpdate, TickData};
