//! # Option Chain Order Book - Professional Options Market Making Infrastructure
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
//! - **Multi-Expiration Option Chain Management**: Handle hundreds of options
//!   across multiple strikes and expirations simultaneously with hierarchical
//!   organization.
//!
//! - **Real-Time Order Book per Option**: Individual order books for each option
//!   contract with full depth, powered by OrderBook-rs.
//!
//! - **Volatility Surface Integration**: Consistent pricing across the entire
//!   option chain using arbitrage-free volatility surfaces.
//!
//! - **Greeks Aggregation**: Portfolio-level risk management with real-time
//!   Greeks calculation (delta, gamma, vega, theta).
//!
//! - **Adaptive Spread Control**: Dynamic spread adjustment based on market
//!   conditions, inventory levels, and the Avellaneda-Stoikov model.
//!
//! - **Inventory Management**: Position tracking with configurable limits per
//!   option, strike, expiration, and underlying asset.
//!
//! - **Delta Hedging Engine**: Automated hedging with configurable thresholds,
//!   bands, and execution strategies.
//!
//! - **P&L Attribution**: Detailed breakdown by Greek (delta, gamma, vega, theta),
//!   realized vs unrealized, and fee tracking.
//!
//! - **Risk Controller**: Real-time risk monitoring with configurable limits
//!   and automatic trading halts.
//!
//! ## Design Goals
//!
//! This library is built with the following design principles:
//!
//! 1. **Performance**: Leverage lock-free data structures for low latency and
//!    high throughput in market making operations.
//! 2. **Correctness**: Ensure accurate Greeks calculation, P&L attribution,
//!    and risk management under all market conditions.
//! 3. **Modularity**: Each component (pricing, quoting, hedging, risk) can be
//!    used independently or composed together.
//! 4. **Extensibility**: Easy integration with different exchanges through
//!    the adapter pattern.
//!
//! ## Architecture
//!
//! The library follows a hierarchical structure for option chain management:
//!
//! ```text
//! OptionChainManager (per underlying)
//!   └── ExpirationManager (per expiry date)
//!         └── StrikeManager (per strike price)
//!               └── OptionOrderBook (per call/put)
//!                     └── OrderBook<T> (from OrderBook-rs)
//! ```
//!
//! This architecture enables:
//! - Efficient aggregation of Greeks and positions at any level
//! - Fast lookup of specific option contracts
//! - Scalable management of large option chains
//!
//! ## Use Cases
//!
//! - **Options Market Making**: Complete infrastructure for quoting options
//!   with dynamic spreads and inventory management
//! - **Delta Hedging Systems**: Automated hedging with configurable strategies
//! - **Risk Management**: Real-time monitoring of Greek exposures and P&L
//! - **Trading Systems**: Core component for building options trading platforms
//! - **Research & Backtesting**: Platform for studying options market dynamics
//!
//! ## Module Structure
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`chain`] | Option chain management: contracts, expirations, strikes |
//! | [`orderbook`] | Order book integration with OrderBook-rs |
//! | [`pricing`] | Pricing engine with Greeks and theoretical values |
//! | [`quoting`] | Quote generation with Avellaneda-Stoikov spread model |
//! | [`inventory`] | Position tracking and portfolio aggregation |
//! | [`hedging`] | Delta hedging engine with configurable parameters |
//! | [`risk`] | Risk controller with limits and stress testing |
//! | [`pnl`] | P&L calculation and Greek attribution |
//! | [`market_data`] | Market data handling and normalization |
//! | [`adapters`] | Exchange adapter traits and types |
//!
//! ## Core Components
//!
//! ### Option Chain Management ([`chain`])
//!
//! The chain module provides hierarchical management of option contracts:
//!
//! - **OptionContract**: Represents a single option with strike, expiry, type
//! - **ExpirationManager**: Manages all strikes for a given expiration
//! - **StrikeManager**: Manages call/put pair at a specific strike
//! - **OptionChainManager**: Top-level manager for an underlying asset
//!
//! ### Order Book ([`orderbook`])
//!
//! Built on OrderBook-rs for high-performance order management:
//!
//! - **OptionOrderBook**: Wrapper around OrderBook-rs with option-specific features
//! - **OptionOrderBookManager**: Manages multiple order books efficiently
//! - **Quote**: Two-sided market representation with bid/ask
//!
//! ### Pricing ([`pricing`])
//!
//! Comprehensive pricing and Greeks calculation:
//!
//! - **Greeks**: Delta, gamma, vega, theta with dollar conversions
//! - **PricingParams**: Option parameters for pricing calculations
//! - **TheoreticalValue**: Theoretical price with bid/ask and Greeks
//!
//! ### Quoting ([`quoting`])
//!
//! Dynamic quote generation using the Avellaneda-Stoikov model:
//!
//! - **SpreadCalculator**: Optimal spread calculation based on volatility and inventory
//! - **QuoteParams**: Parameters for quote generation
//! - **GeneratedQuote**: Output quote with bid/ask prices and sizes
//!
//! ### Inventory Management ([`inventory`])
//!
//! Position tracking and risk limits:
//!
//! - **OptionPosition**: Single option position with Greeks and P&L
//! - **PositionLimits**: Configurable limits per option, strike, expiration
//! - **InventoryManager**: Aggregates positions and checks limits
//!
//! ### Delta Hedging ([`hedging`])
//!
//! Automated hedging engine:
//!
//! - **DeltaHedger**: Tracks delta exposure and generates hedge orders
//! - **HedgeParams**: Configurable thresholds and bands
//! - **HedgeOrder**: Generated hedge order with reason and urgency
//!
//! ### Risk Management ([`risk`])
//!
//! Real-time risk monitoring:
//!
//! - **RiskController**: Monitors exposures and triggers halts
//! - **RiskLimits**: Configurable Greek and loss limits
//! - **RiskBreach**: Enumeration of limit breach types
//!
//! ### P&L Attribution ([`pnl`])
//!
//! Detailed profit and loss tracking:
//!
//! - **PnLCalculator**: Tracks realized and unrealized P&L
//! - **PnLAttribution**: Breaks down P&L by Greek contribution
//!
//! ## Example Usage
//!
//! ### Creating an Option Order Book
//!
//! ```rust
//! use option_chain_orderbook::orderbook::OptionOrderBook;
//! use orderbook_rs::{OrderId, Side};
//!
//! // Create an order book for a specific option
//! let book = OptionOrderBook::new("BTC-20240329-50000-C");
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
//! ### Managing Multiple Order Books
//!
//! ```rust
//! use option_chain_orderbook::orderbook::OptionOrderBookManager;
//! use orderbook_rs::{OrderId, Side};
//!
//! let mut manager = OptionOrderBookManager::new();
//!
//! // Get or create order books for different options
//! let call_book = manager.get_or_create("BTC-20240329-50000-C");
//! call_book.add_limit_order(OrderId::new(), Side::Buy, 500, 10).unwrap();
//!
//! let put_book = manager.get_or_create("BTC-20240329-50000-P");
//! put_book.add_limit_order(OrderId::new(), Side::Sell, 300, 5).unwrap();
//!
//! // Get aggregate statistics
//! let stats = manager.stats();
//! assert_eq!(stats.book_count, 2);
//! ```
//!
//! ### Calculating Spreads with Avellaneda-Stoikov
//!
//! ```rust
//! use option_chain_orderbook::quoting::{SpreadCalculator, QuoteParams};
//! use rust_decimal_macros::dec;
//!
//! let calc = SpreadCalculator::new()
//!     .with_min_spread(dec!(0.001))
//!     .with_max_spread(dec!(0.10));
//!
//! let params = QuoteParams::new(
//!     dec!(5.50),  // mid price
//!     dec!(0),     // inventory
//!     dec!(0.30),  // volatility
//!     dec!(0.25),  // time to expiry (years)
//! );
//!
//! let spread = calc.optimal_spread(&params);
//! let skew = calc.inventory_skew(&params);
//! ```
//!
//! ### Delta Hedging
//!
//! ```rust
//! use option_chain_orderbook::hedging::{DeltaHedger, HedgeParams};
//! use option_chain_orderbook::pricing::Greeks;
//! use rust_decimal_macros::dec;
//!
//! let params = HedgeParams::default();
//! let mut hedger = DeltaHedger::new(params);
//!
//! // Update portfolio delta from Greeks
//! let greeks = Greeks::new(dec!(150), dec!(5), dec!(-20), dec!(100), dec!(10));
//! hedger.update_delta(&greeks);
//!
//! // Check if hedging is needed
//! if let Some(order) = hedger.calculate_hedge("BTC", dec!(50000), 0) {
//!     println!("Hedge needed: {} units", order.quantity);
//! }
//! ```
//!
//! ### Risk Monitoring
//!
//! ```rust
//! use option_chain_orderbook::risk::{RiskController, RiskLimits};
//! use option_chain_orderbook::pricing::Greeks;
//! use rust_decimal_macros::dec;
//!
//! let limits = RiskLimits::default();
//! let mut controller = RiskController::new(limits);
//!
//! let greeks = Greeks::new(dec!(100), dec!(5), dec!(-50), dec!(200), dec!(10));
//!
//! // Check for limit breaches
//! let breaches = controller.check_greek_limits(&greeks);
//! if !breaches.is_empty() {
//!     println!("Risk limits breached!");
//! }
//! ```
//!
//! ## Performance Characteristics
//!
//! Built on OrderBook-rs's lock-free architecture, this library inherits its
//! high-performance characteristics:
//!
//! - **Order Operations**: O(log N) for add/cancel operations
//! - **Best Quote Lookup**: O(1) with caching
//! - **Greeks Aggregation**: O(N) where N is number of positions
//! - **Thread Safety**: Lock-free operations for concurrent access
//!
//! ## Dependencies
//!
//! This library builds on several high-quality Rust crates:
//!
//! - **orderbook-rs**: Lock-free order book engine
//! - **pricelevel**: Price level management with multiple order types
//! - **optionstratlib**: Options pricing and strategy analysis
//! - **rust_decimal**: Precise decimal arithmetic for financial calculations
//!
//! ## Status
//!
//! This project is currently in active development. The API may change as
//! features are added and refined based on real-world usage.

pub mod adapters;
pub mod chain;
pub mod error;
pub mod hedging;
pub mod inventory;
pub mod market_data;
pub mod orderbook;
pub mod pnl;
pub mod pricing;
pub mod quoting;
pub mod risk;

pub use error::Error;

/// Result type alias using the library's error type.
pub type Result<T> = std::result::Result<T, Error>;
