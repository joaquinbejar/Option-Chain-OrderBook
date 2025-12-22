//! Order book integration module.
//!
//! This module provides integration with OrderBook-rs for managing order books
//! per option contract. It wraps the high-performance order book implementation
//! and provides option-specific functionality.
//!
//! ## Components
//!
//! - [`OptionOrderBook`]: Order book wrapper for a single option contract
//! - [`OptionOrderBookManager`]: Manages order books across the entire option chain
//! - [`Quote`]: Represents a two-sided quote (bid and ask)
//!
//! ## Example
//!
//! ```rust,ignore
//! use option_chain_orderbook::orderbook::{OptionOrderBook, Quote};
//!
//! let mut book = OptionOrderBook::new("BTC-20240329-50000-C");
//! book.add_order(order);
//! let quote = book.best_quote();
//! ```

mod book;
mod manager;
mod quote;

pub use book::OptionOrderBook;
pub use manager::OptionOrderBookManager;
pub use quote::{Quote, QuoteUpdate};
