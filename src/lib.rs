//! # Option Chain Order Book - Options Market Making Infrastructure
//!
//! A high-performance Rust library for options market making infrastructure,
//! providing a complete Option Chain Order Book system built on top of
//! [OrderBook-rs](https://crates.io/crates/orderbook-rs),
//! [PriceLevel](https://crates.io/crates/pricelevel), and
//! [OptionStratLib](https://crates.io/crates/optionstratlib).
//!
//! ## Key Features
//!
//! - **Lock-Free Architecture**: Built on OrderBook-rs's lock-free data structures
//!   for maximum throughput in high-frequency trading scenarios.
//!
//! - **Hierarchical Order Book Structure**: Multi-level organization from
//!   underlying assets down to individual option contracts.
//!
//! - **Multi-Expiration Option Chain Management**: Handle hundreds of options
//!   across multiple strikes and expirations simultaneously.
//!
//! - **Real-Time Order Book per Option**: Individual order books for each option
//!   contract with full depth, powered by OrderBook-rs.
//!
//! - **OptionStratLib Integration**: Use Greeks calculation, Options struct,
//!   and pricing models directly from OptionStratLib.
//!
//! ## Architecture
//!
//! The library follows a hierarchical structure for option chain management:
//!
//! ```text
//! UnderlyingOrderBookManager (manages all underlyings: BTC, ETH, SPX, etc.)
//!   └── UnderlyingOrderBook (per underlying, all expirations for one asset)
//!         └── ExpirationOrderBookManager (manages all expirations for underlying)
//!               └── ExpirationOrderBook (per expiry date)
//!                     └── OptionChainOrderBook (per expiration, option chain)
//!                           └── StrikeOrderBookManager (manages call/put pair)
//!                                 └── StrikeOrderBook (per strike price)
//!                                       └── OptionOrderBook (call or put)
//!                                             └── OrderBook<T> (from OrderBook-rs)
//! ```
//!
//! This architecture enables:
//! - Efficient aggregation of Greeks and positions at any level
//! - Fast lookup of specific option contracts
//! - Scalable management of large option chains
//!
//! ## Module Structure
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`orderbook`] | Hierarchical order book structure with all managers |
//! | [`error`] | Error types for the library |
//!
//! ## Core Components
//!
//! ### Order Book Hierarchy ([`orderbook`])
//!
//! - [`orderbook::UnderlyingOrderBookManager`]: Top-level manager for all underlyings
//! - [`orderbook::UnderlyingOrderBook`]: All expirations for a single underlying
//! - [`orderbook::ExpirationOrderBook`]: All strikes for a single expiration
//! - [`orderbook::OptionChainOrderBook`]: Option chain with strike management
//! - [`orderbook::StrikeOrderBook`]: Call/put pair at a strike price
//! - [`orderbook::OptionOrderBook`]: Single option order book
//! - [`orderbook::Quote`]: Two-sided market representation
//!
//! ## Example Usage
//!
//! ### Creating a Hierarchical Order Book
//!
//! ```rust
//! use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
//! use optionstratlib::{pos, ExpirationDate};
//! use orderbook_rs::{OrderId, Side};
//!
//! let manager = UnderlyingOrderBookManager::new();
//! let exp_date = ExpirationDate::Days(pos!(30.0));
//!
//! // Create BTC option chain (use block to drop guards)
//! {
//!     let btc = manager.get_or_create("BTC");
//!     let exp = btc.get_or_create_expiration(exp_date);
//!     let strike = exp.get_or_create_strike(50000);
//!
//!     // Add orders to call
//!     strike.call().add_limit_order(OrderId::new(), Side::Buy, 100, 10).unwrap();
//!     strike.call().add_limit_order(OrderId::new(), Side::Sell, 105, 5).unwrap();
//!
//!     // Get quote
//!     let quote = strike.call().best_quote();
//!     assert!(quote.is_two_sided());
//! }
//!
//! // Get statistics
//! let stats = manager.stats();
//! ```
//!
//! ### Creating a Single Option Order Book
//!
//! ```rust
//! use option_chain_orderbook::orderbook::OptionOrderBook;
//! use optionstratlib::OptionStyle;
//! use orderbook_rs::{OrderId, Side};
//!
//! // Create an order book for a specific option
//! let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
//!
//! // Add limit orders
//! book.add_limit_order(OrderId::new(), Side::Buy, 500, 10).unwrap();
//! book.add_limit_order(OrderId::new(), Side::Sell, 520, 5).unwrap();
//!
//! // Get the best quote
//! let quote = book.best_quote();
//! assert!(quote.is_two_sided());
//! ```
//!
//! ### Using OptionStratLib for Greeks
//!
//! ```rust,ignore
//! use optionstratlib::{Options, ExpirationDate, pos};
//! use optionstratlib::model::types::{OptionStyle, OptionType, Side};
//! use optionstratlib::greeks::{delta, gamma, theta, vega, rho};
//! use rust_decimal_macros::dec;
//!
//! let option = Options {
//!     option_type: OptionType::European,
//!     side: Side::Long,
//!     underlying_symbol: "BTC".to_string(),
//!     strike_price: pos!(50000.0),
//!     expiration_date: ExpirationDate::Days(pos!(30.0)),
//!     implied_volatility: pos!(0.6),
//!     quantity: pos!(1.0),
//!     underlying_price: pos!(48000.0),
//!     risk_free_rate: dec!(0.05),
//!     option_style: OptionStyle::Call,
//!     dividend_yield: pos!(0.0),
//!     exotic_params: None,
//! };
//!
//! let delta_value = delta(&option).unwrap();
//! let gamma_value = gamma(&option).unwrap();
//! ```
//!
//! ## Performance Characteristics
//!
//! Built on OrderBook-rs's lock-free architecture:
//!
//! - **Order Operations**: O(log N) for add/cancel operations
//! - **Best Quote Lookup**: O(1) with caching
//! - **Thread Safety**: Lock-free operations for concurrent access
//!
//! ## Dependencies
//!
//! - **orderbook-rs**: Lock-free order book engine
//! - **pricelevel**: Price level management
//! - **optionstratlib**: Options pricing, Greeks, and strategy analysis
//! - **rust_decimal**: Precise decimal arithmetic

pub mod error;
pub mod orderbook;
pub mod utils;

pub use error::{Error, Result};
