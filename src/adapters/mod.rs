//! Exchange adapters module.
//!
//! This module provides adapters for connecting to various exchanges
//! for both traditional and cryptocurrency options markets.

mod traits;

pub use traits::{ExchangeAdapter, OrderRequest, OrderResponse};
