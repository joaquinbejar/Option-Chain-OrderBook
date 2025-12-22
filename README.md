[![Dual License](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/option-chain-orderbook.svg)](https://crates.io/crates/option-chain-orderbook)
[![Downloads](https://img.shields.io/crates/d/option-chain-orderbook.svg)](https://crates.io/crates/option-chain-orderbook)
[![Stars](https://img.shields.io/github/stars/joaquinbejar/Option-Chain-OrderBook.svg)](https://github.com/joaquinbejar/Option-Chain-OrderBook/stargazers)
[![Issues](https://img.shields.io/github/issues/joaquinbejar/Option-Chain-OrderBook.svg)](https://github.com/joaquinbejar/Option-Chain-OrderBook/issues)
[![PRs](https://img.shields.io/github/issues-pr/joaquinbejar/Option-Chain-OrderBook.svg)](https://github.com/joaquinbejar/Option-Chain-OrderBook/pulls)

[![Build Status](https://img.shields.io/github/workflow/status/joaquinbejar/Option-Chain-OrderBook/CI)](https://github.com/joaquinbejar/Option-Chain-OrderBook/actions)
[![Coverage](https://img.shields.io/codecov/c/github/joaquinbejar/Option-Chain-OrderBook)](https://codecov.io/gh/joaquinbejar/Option-Chain-OrderBook)
[![Dependencies](https://img.shields.io/librariesio/github/joaquinbejar/Option-Chain-OrderBook)](https://libraries.io/github/joaquinbejar/Option-Chain-OrderBook)
[![Documentation](https://img.shields.io/badge/docs-latest-blue.svg)](https://docs.rs/option-chain-orderbook)



## Option Chain Order Book - Options Market Making Infrastructure

A high-performance Rust library for options market making infrastructure,
providing a complete Option Chain Order Book system built on top of
[OrderBook-rs](https://crates.io/crates/orderbook-rs),
[PriceLevel](https://crates.io/crates/pricelevel), and
[OptionStratLib](https://crates.io/crates/optionstratlib).

### Key Features

- **Lock-Free Architecture**: Built on OrderBook-rs's lock-free data structures
  for maximum throughput in high-frequency trading scenarios.

- **Hierarchical Order Book Structure**: Multi-level organization from
  underlying assets down to individual option contracts.

- **Multi-Expiration Option Chain Management**: Handle hundreds of options
  across multiple strikes and expirations simultaneously.

- **Real-Time Order Book per Option**: Individual order books for each option
  contract with full depth, powered by OrderBook-rs.

- **OptionStratLib Integration**: Use Greeks calculation, Options struct,
  and pricing models directly from OptionStratLib.

### Architecture

The library follows a hierarchical structure for option chain management:

```
UnderlyingOrderBookManager (manages all underlyings: BTC, ETH, SPX, etc.)
  └── UnderlyingOrderBook (per underlying, all expirations for one asset)
        └── ExpirationOrderBookManager (manages all expirations for underlying)
              └── ExpirationOrderBook (per expiry date)
                    └── OptionChainOrderBook (per expiration, option chain)
                          └── StrikeOrderBookManager (manages call/put pair)
                                └── StrikeOrderBook (per strike price)
                                      └── OptionOrderBook (call or put)
                                            └── OrderBook<T> (from OrderBook-rs)
```

This architecture enables:
- Efficient aggregation of Greeks and positions at any level
- Fast lookup of specific option contracts
- Scalable management of large option chains

### Module Structure

| Module | Description |
|--------|-------------|
| [`orderbook`] | Hierarchical order book structure with all managers |
| [`error`] | Error types for the library |

### Core Components

#### Order Book Hierarchy ([`orderbook`])

- [`orderbook::UnderlyingOrderBookManager`]: Top-level manager for all underlyings
- [`orderbook::UnderlyingOrderBook`]: All expirations for a single underlying
- [`orderbook::ExpirationOrderBook`]: All strikes for a single expiration
- [`orderbook::OptionChainOrderBook`]: Option chain with strike management
- [`orderbook::StrikeOrderBook`]: Call/put pair at a strike price
- [`orderbook::OptionOrderBook`]: Single option order book
- [`orderbook::Quote`]: Two-sided market representation

### Example Usage

#### Creating a Hierarchical Order Book

```rust
use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
use optionstratlib::{pos, ExpirationDate};
use orderbook_rs::{OrderId, Side};

let manager = UnderlyingOrderBookManager::new();
let exp_date = ExpirationDate::Days(pos!(30.0));

// Create BTC option chain (use block to drop guards)
{
    let btc = manager.get_or_create("BTC");
    let exp = btc.get_or_create_expiration(exp_date);
    let strike = exp.get_or_create_strike(50000);

    // Add orders to call
    strike.call().add_limit_order(OrderId::new(), Side::Buy, 100, 10).unwrap();
    strike.call().add_limit_order(OrderId::new(), Side::Sell, 105, 5).unwrap();

    // Get quote
    let quote = strike.call().best_quote();
    assert!(quote.is_two_sided());
}

// Get statistics
let stats = manager.stats();
```

#### Creating a Single Option Order Book

```rust
use option_chain_orderbook::orderbook::OptionOrderBook;
use optionstratlib::OptionStyle;
use orderbook_rs::{OrderId, Side};

// Create an order book for a specific option
let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

// Add limit orders
book.add_limit_order(OrderId::new(), Side::Buy, 500, 10).unwrap();
book.add_limit_order(OrderId::new(), Side::Sell, 520, 5).unwrap();

// Get the best quote
let quote = book.best_quote();
assert!(quote.is_two_sided());
```

#### Using OptionStratLib for Greeks

```rust
use optionstratlib::{Options, ExpirationDate, pos};
use optionstratlib::model::types::{OptionStyle, OptionType, Side};
use optionstratlib::greeks::{delta, gamma, theta, vega, rho};
use rust_decimal_macros::dec;

let option = Options {
    option_type: OptionType::European,
    side: Side::Long,
    underlying_symbol: "BTC".to_string(),
    strike_price: pos!(50000.0),
    expiration_date: ExpirationDate::Days(pos!(30.0)),
    implied_volatility: pos!(0.6),
    quantity: pos!(1.0),
    underlying_price: pos!(48000.0),
    risk_free_rate: dec!(0.05),
    option_style: OptionStyle::Call,
    dividend_yield: pos!(0.0),
    exotic_params: None,
};

let delta_value = delta(&option).unwrap();
let gamma_value = gamma(&option).unwrap();
```

### Performance Characteristics

Built on OrderBook-rs's lock-free architecture:

- **Order Operations**: O(log N) for add/cancel operations
- **Best Quote Lookup**: O(1) with caching
- **Thread Safety**: Lock-free operations for concurrent access

### Dependencies

- **orderbook-rs**: Lock-free order book engine
- **pricelevel**: Price level management
- **optionstratlib**: Options pricing, Greeks, and strategy analysis
- **rust_decimal**: Precise decimal arithmetic


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
