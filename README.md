[![Dual License](https://img.shields.io/badge/license-MIT%20and%20Apache%202.0-blue)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/option-chain-orderbook.svg)](https://crates.io/crates/option-chain-orderbook)
[![Downloads](https://img.shields.io/crates/d/option-chain-orderbook.svg)](https://crates.io/crates/option-chain-orderbook)
[![Stars](https://img.shields.io/github/stars/joaquinbejar/Option-Chain-OrderBook.svg)](https://github.com/joaquinbejar/Option-Chain-OrderBook/stargazers)
[![Issues](https://img.shields.io/github/issues/joaquinbejar/Option-Chain-OrderBook.svg)](https://github.com/joaquinbejar/Option-Chain-OrderBook/issues)
[![PRs](https://img.shields.io/github/issues-pr/joaquinbejar/Option-Chain-OrderBook.svg)](https://github.com/joaquinbejar/Option-Chain-OrderBook/pulls)

[![Build Status](https://img.shields.io/github/workflow/status/joaquinbejar/Option-Chain-OrderBook/CI)](https://github.com/joaquinbejar/Option-Chain-OrderBook/actions)
[![Coverage](https://img.shields.io/codecov/c/github/joaquinbejar/Option-Chain-OrderBook)](https://codecov.io/gh/joaquinbejar/Option-Chain-OrderBook)
[![Dependencies](https://img.shields.io/librariesio/github/joaquinbejar/Option-Chain-OrderBook)](https://libraries.io/github/joaquinbejar/Option-Chain-OrderBook)
[![Documentation](https://img.shields.io/badge/docs-latest-blue.svg)](https://docs.rs/option-chain-orderbook)



## Option Chain Order Book - Professional Options Market Making Infrastructure

A high-performance Rust library for options market making infrastructure,
providing a complete Option Chain Order Book system built on top of
[OrderBook-rs](https://crates.io/crates/orderbook-rs),
[PriceLevel](https://crates.io/crates/pricelevel), and
[OptionStratLib](https://crates.io/crates/optionstratlib).

### Key Features

- **Lock-Free Architecture**: Built on OrderBook-rs's lock-free data structures
  for maximum throughput in high-frequency trading scenarios.

- **Multi-Expiration Option Chain Management**: Handle hundreds of options
  across multiple strikes and expirations simultaneously with hierarchical
  organization.

- **Real-Time Order Book per Option**: Individual order books for each option
  contract with full depth, powered by OrderBook-rs.

- **Volatility Surface Integration**: Consistent pricing across the entire
  option chain using arbitrage-free volatility surfaces.

- **Greeks Aggregation**: Portfolio-level risk management with real-time
  Greeks calculation (delta, gamma, vega, theta).

- **Adaptive Spread Control**: Dynamic spread adjustment based on market
  conditions, inventory levels, and the Avellaneda-Stoikov model.

- **Inventory Management**: Position tracking with configurable limits per
  option, strike, expiration, and underlying asset.

- **Delta Hedging Engine**: Automated hedging with configurable thresholds,
  bands, and execution strategies.

- **P&L Attribution**: Detailed breakdown by Greek (delta, gamma, vega, theta),
  realized vs unrealized, and fee tracking.

- **Risk Controller**: Real-time risk monitoring with configurable limits
  and automatic trading halts.

### Design Goals

This library is built with the following design principles:

1. **Performance**: Leverage lock-free data structures for low latency and
   high throughput in market making operations.
2. **Correctness**: Ensure accurate Greeks calculation, P&L attribution,
   and risk management under all market conditions.
3. **Modularity**: Each component (pricing, quoting, hedging, risk) can be
   used independently or composed together.
4. **Extensibility**: Easy integration with different exchanges through
   the adapter pattern.

### Architecture

The library follows a hierarchical structure for option chain management:

```
OptionChainManager (per underlying)
  └── ExpirationManager (per expiry date)
        └── StrikeManager (per strike price)
              └── OptionOrderBook (per call/put)
                    └── OrderBook<T> (from OrderBook-rs)
```

This architecture enables:
- Efficient aggregation of Greeks and positions at any level
- Fast lookup of specific option contracts
- Scalable management of large option chains

### Use Cases

- **Options Market Making**: Complete infrastructure for quoting options
  with dynamic spreads and inventory management
- **Delta Hedging Systems**: Automated hedging with configurable strategies
- **Risk Management**: Real-time monitoring of Greek exposures and P&L
- **Trading Systems**: Core component for building options trading platforms
- **Research & Backtesting**: Platform for studying options market dynamics

### Module Structure

| Module | Description |
|--------|-------------|
| [`chain`] | Option chain management: contracts, expirations, strikes |
| [`orderbook`] | Order book integration with OrderBook-rs |
| [`pricing`] | Pricing engine with Greeks and theoretical values |
| [`quoting`] | Quote generation with Avellaneda-Stoikov spread model |
| [`inventory`] | Position tracking and portfolio aggregation |
| [`hedging`] | Delta hedging engine with configurable parameters |
| [`risk`] | Risk controller with limits and stress testing |
| [`pnl`] | P&L calculation and Greek attribution |
| [`market_data`] | Market data handling and normalization |
| [`adapters`] | Exchange adapter traits and types |

### Core Components

#### Option Chain Management ([`chain`])

The chain module provides hierarchical management of option contracts:

- **OptionContract**: Represents a single option with strike, expiry, type
- **ExpirationManager**: Manages all strikes for a given expiration
- **StrikeManager**: Manages call/put pair at a specific strike
- **OptionChainManager**: Top-level manager for an underlying asset

#### Order Book ([`orderbook`])

Built on OrderBook-rs for high-performance order management:

- **OptionOrderBook**: Wrapper around OrderBook-rs with option-specific features
- **OptionOrderBookManager**: Manages multiple order books efficiently
- **Quote**: Two-sided market representation with bid/ask

#### Pricing ([`pricing`])

Comprehensive pricing and Greeks calculation:

- **Greeks**: Delta, gamma, vega, theta with dollar conversions
- **PricingParams**: Option parameters for pricing calculations
- **TheoreticalValue**: Theoretical price with bid/ask and Greeks

#### Quoting ([`quoting`])

Dynamic quote generation using the Avellaneda-Stoikov model:

- **SpreadCalculator**: Optimal spread calculation based on volatility and inventory
- **QuoteParams**: Parameters for quote generation
- **GeneratedQuote**: Output quote with bid/ask prices and sizes

#### Inventory Management ([`inventory`])

Position tracking and risk limits:

- **OptionPosition**: Single option position with Greeks and P&L
- **PositionLimits**: Configurable limits per option, strike, expiration
- **InventoryManager**: Aggregates positions and checks limits

#### Delta Hedging ([`hedging`])

Automated hedging engine:

- **DeltaHedger**: Tracks delta exposure and generates hedge orders
- **HedgeParams**: Configurable thresholds and bands
- **HedgeOrder**: Generated hedge order with reason and urgency

#### Risk Management ([`risk`])

Real-time risk monitoring:

- **RiskController**: Monitors exposures and triggers halts
- **RiskLimits**: Configurable Greek and loss limits
- **RiskBreach**: Enumeration of limit breach types

#### P&L Attribution ([`pnl`])

Detailed profit and loss tracking:

- **PnLCalculator**: Tracks realized and unrealized P&L
- **PnLAttribution**: Breaks down P&L by Greek contribution

### Example Usage

#### Creating an Option Order Book

```rust
use option_chain_orderbook::orderbook::OptionOrderBook;
use orderbook_rs::{OrderId, Side};

// Create an order book for a specific option
let book = OptionOrderBook::new("BTC-20240329-50000-C");

// Add limit orders
book.add_limit_order(OrderId::new(), Side::Buy, 500, 10).unwrap();
book.add_limit_order(OrderId::new(), Side::Sell, 520, 5).unwrap();

// Get the best quote
let quote = book.best_quote();
assert!(quote.is_two_sided());
```

#### Managing Multiple Order Books

```rust
use option_chain_orderbook::orderbook::OptionOrderBookManager;
use orderbook_rs::{OrderId, Side};

let mut manager = OptionOrderBookManager::new();

// Get or create order books for different options
let call_book = manager.get_or_create("BTC-20240329-50000-C");
call_book.add_limit_order(OrderId::new(), Side::Buy, 500, 10).unwrap();

let put_book = manager.get_or_create("BTC-20240329-50000-P");
put_book.add_limit_order(OrderId::new(), Side::Sell, 300, 5).unwrap();

// Get aggregate statistics
let stats = manager.stats();
assert_eq!(stats.book_count, 2);
```

#### Calculating Spreads with Avellaneda-Stoikov

```rust
use option_chain_orderbook::quoting::{SpreadCalculator, QuoteParams};
use rust_decimal_macros::dec;

let calc = SpreadCalculator::new()
    .with_min_spread(dec!(0.001))
    .with_max_spread(dec!(0.10));

let params = QuoteParams::new(
    dec!(5.50),  // mid price
    dec!(0),     // inventory
    dec!(0.30),  // volatility
    dec!(0.25),  // time to expiry (years)
);

let spread = calc.optimal_spread(&params);
let skew = calc.inventory_skew(&params);
```

#### Delta Hedging

```rust
use option_chain_orderbook::hedging::{DeltaHedger, HedgeParams};
use option_chain_orderbook::pricing::Greeks;
use rust_decimal_macros::dec;

let params = HedgeParams::default();
let mut hedger = DeltaHedger::new(params);

// Update portfolio delta from Greeks
let greeks = Greeks::new(dec!(150), dec!(5), dec!(-20), dec!(100), dec!(10));
hedger.update_delta(&greeks);

// Check if hedging is needed
if let Some(order) = hedger.calculate_hedge("BTC", dec!(50000), 0) {
    println!("Hedge needed: {} units", order.quantity);
}
```

#### Risk Monitoring

```rust
use option_chain_orderbook::risk::{RiskController, RiskLimits};
use option_chain_orderbook::pricing::Greeks;
use rust_decimal_macros::dec;

let limits = RiskLimits::default();
let mut controller = RiskController::new(limits);

let greeks = Greeks::new(dec!(100), dec!(5), dec!(-50), dec!(200), dec!(10));

// Check for limit breaches
let breaches = controller.check_greek_limits(&greeks);
if !breaches.is_empty() {
    println!("Risk limits breached!");
}
```

### Performance Characteristics

Built on OrderBook-rs's lock-free architecture, this library inherits its
high-performance characteristics:

- **Order Operations**: O(log N) for add/cancel operations
- **Best Quote Lookup**: O(1) with caching
- **Greeks Aggregation**: O(N) where N is number of positions
- **Thread Safety**: Lock-free operations for concurrent access

### Dependencies

This library builds on several high-quality Rust crates:

- **orderbook-rs**: Lock-free order book engine
- **pricelevel**: Price level management with multiple order types
- **optionstratlib**: Options pricing and strategy analysis
- **rust_decimal**: Precise decimal arithmetic for financial calculations

### Status

This project is currently in active development. The API may change as
features are added and refined based on real-world usage.


## 🛠 Makefile Commands

This project includes a `Makefile` with common tasks to simplify development. Here's a list of useful commands:

### 🔧 Build & Run

```sh
make build         # Compile the project
make release       # Build in release mode
make run           # Run the main binary
```

### 🧪 Test & Quality

```sh
make test          # Run all tests
make fmt           # Format code
make fmt-check     # Check formatting without applying
make lint          # Run clippy with warnings as errors
make lint-fix      # Auto-fix lint issues
make fix           # Auto-fix Rust compiler suggestions
make check         # Run fmt-check + lint + test
```

### 📦 Packaging & Docs

```sh
make doc           # Check for missing docs via clippy
make doc-open      # Build and open Rust documentation
make create-doc    # Generate internal docs
make readme        # Regenerate README using cargo-readme
make publish       # Prepare and publish crate to crates.io
```

### 📈 Coverage & Benchmarks

```sh
make coverage            # Generate code coverage report (XML)
make coverage-html       # Generate HTML coverage report
make open-coverage       # Open HTML report
make bench               # Run benchmarks using Criterion
make bench-show          # Open benchmark report
make bench-save          # Save benchmark history snapshot
make bench-compare       # Compare benchmark runs
make bench-json          # Output benchmarks in JSON
make bench-clean         # Remove benchmark data
```

### 🧪 Git & Workflow Helpers

```sh
make git-log             # Show commits on current branch vs main
make check-spanish       # Check for Spanish words in code
make zip                 # Create zip without target/ and temp files
make tree                # Visualize project tree (excludes common clutter)
```

### 🤖 GitHub Actions (via act)

```sh
make workflow-build      # Simulate build workflow
make workflow-lint       # Simulate lint workflow
make workflow-test       # Simulate test workflow
make workflow-coverage   # Simulate coverage workflow
make workflow            # Run all workflows
```

ℹ️ Requires act for local workflow simulation and cargo-tarpaulin for coverage.

## Contribution and Contact

We welcome contributions to this project! If you would like to contribute, please follow these steps:

1. Fork the repository.
2. Create a new branch for your feature or bug fix.
3. Make your changes and ensure that the project still builds and all tests pass.
4. Commit your changes and push your branch to your forked repository.
5. Submit a pull request to the main repository.

If you have any questions, issues, or would like to provide feedback, please feel free to contact the project
maintainer:

### **Contact Information**
- **Author**: Joaquín Béjar García
- **Email**: jb@taunais.com
- **Telegram**: [@joaquin_bejar](https://t.me/joaquin_bejar)
- **Repository**: <https://github.com/joaquinbejar/Option-Chain-OrderBook>
- **Documentation**: <https://docs.rs/option-chain-orderbook>


We appreciate your interest and look forward to your contributions!

**License**: MIT
