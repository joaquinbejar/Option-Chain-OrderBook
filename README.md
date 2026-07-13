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

- **Thread-Safe Concurrent Access**: Uses `SkipMap` for lock-free concurrent
  access to order books across multiple threads.

- **OptionStratLib Integration**: Use Greeks calculation, `ExpirationDate`,
  `OptionStyle`, and pricing models directly from OptionStratLib.

- **Result-Based Error Handling**: All fallible operations return `Result<T, Error>`
  with descriptive error types.

### Architecture

The library follows a hierarchical structure for option chain management:

```
UnderlyingOrderBookManager (manages all underlyings: BTC, ETH, SPX, etc.)
  └── UnderlyingOrderBook (per underlying, all expirations for one asset)
        └── ExpirationOrderBookManager (manages all expirations for underlying)
              └── ExpirationOrderBook (per expiry date)
                    └── OptionChainOrderBook (per expiration, option chain)
                          └── StrikeOrderBookManager (manages all strikes)
                                └── StrikeOrderBook (per strike price, call/put pair)
                                      └── OptionOrderBook (call or put)
                                            └── OrderBook<T> (from OrderBook-rs)
```

This architecture enables:
- Efficient aggregation of Greeks and positions at any level
- Fast lookup of specific option contracts
- Scalable management of large option chains
- ATM strike lookup at any level
- Statistics aggregation across the hierarchy

### Module Structure

| Module | Description |
|--------|-------------|
| [`orderbook`] | Hierarchical order book structure with all managers |
| [`error`] | Error types and `Result` type alias |
| [`utils`] | Utility functions (e.g., date formatting) |

### Re-export Convention

Every public item is available at the crate root **and** under
[`orderbook`]: `option_chain_orderbook::OptionOrderBook` and
`option_chain_orderbook::orderbook::OptionOrderBook` resolve to the same
type. Prefer the shorter crate-root path; the `orderbook::` path remains
valid.

The boundary newtypes — `OrderId`, `OrderType`, `Side`, `TimeInForce`,
`Hash32`, `Price`, `Quantity`, and `TimestampMs` — are re-exported here from
`orderbook_rs` / `pricelevel`, so consumers need **no direct `orderbook_rs`
/ `pricelevel` dependency** to use the hierarchy. `Quote` exposes prices,
sizes, and timestamps through `Price` / `Quantity` / `TimestampMs`; the leaf
`add_limit_order*` submission path still takes plain `u128` / `u64` (that is
what `orderbook_rs` accepts there).

### Core Components

#### Order Book Hierarchy ([`orderbook`])

- [`orderbook::UnderlyingOrderBookManager`]: Top-level manager for all underlyings
- [`orderbook::UnderlyingOrderBook`]: All expirations for a single underlying
- [`orderbook::ExpirationOrderBookManager`]: Manages expirations for an underlying
- [`orderbook::ExpirationOrderBook`]: All strikes for a single expiration
- [`orderbook::OptionChainOrderBook`]: Option chain with strike management
- [`orderbook::StrikeOrderBookManager`]: Manages strikes for an expiration
- [`orderbook::StrikeOrderBook`]: Call/put pair at a strike price
- [`orderbook::OptionOrderBook`]: Single option order book
- [`orderbook::Quote`]: Two-sided market representation
- [`orderbook::QuoteUpdate`]: Quote change tracking

### Microstructure Coverage

This crate is the option-chain *organization and aggregation* layer; the
matching engine itself is `orderbook-rs`. On top of that engine it provides:

- **Hierarchical option chain**: underlying → expiration → chain → strike →
  contract, where each leaf [`orderbook::OptionOrderBook`] wraps one
  `orderbook_rs::OrderBook<T>`. `get_or_create_*` traversal is idempotent and
  returns shared handles.
- **Two-sided quotes**: [`orderbook::Quote`] / [`orderbook::QuoteUpdate`]
  expose top-of-book per side; a one-sided book yields a one-sided quote.
- **Mark price**: [`orderbook::MarkPriceCalculator`] computes a configurable
  weighted average of index / mid / last-trade prices with `Decimal`
  dampening to bound per-update movement.
- **Greeks**: [`orderbook::GreeksEngine`] prices each contract through
  `optionstratlib` from a supplied implied volatility (read from a
  [`orderbook::VolSurface`] by the integrator and passed in — the engine
  takes the IV directly, it does not query the surface itself), and
  [`orderbook::GreeksAggregator`] sums per-position Greeks across the
  hierarchy into [`orderbook::AggregatedGreeks`] using `Decimal`.
- **Expiry lifecycle**: [`orderbook::ExpiryCycleConfig`] /
  [`orderbook::CycleRule`], [`orderbook::ExpiryLifecycleManager`], and
  [`orderbook::ExpiryScheduler`] drive roll/expiry transitions with listeners.
- **Scoped mass-cancel**: contract / strike / chain / expiration /
  underlying / global, each returning a typed result counting what it
  cancelled, iterated deterministically from the ordered `SkipMap`.
- **Instrument & symbol services**: [`orderbook::SymbolIndex`],
  [`orderbook::InstrumentRegistry`], [`orderbook::InstrumentStatus`],
  [`orderbook::ContractSpecs`], [`orderbook::StrikeGenerator`], and
  [`orderbook::StrikeRangeConfig`] for fast lookup and strike management.
- **Order policy hooks**: a crate-local [`orderbook::ValidationConfig`]
  (order/qty limits and an inclusive `[min_price, max_price]` price band)
  plus the upstream [`orderbook::FeeSchedule`] and self-trade prevention
  [`orderbook::STPMode`]. Tick/lot/size limits, fees, and STP are applied by
  `orderbook-rs` at the leaf engine; the price band is enforced crate-side
  (the engine has no price-bound hook) and, when both a
  [`orderbook::ContractSpecs`] band and a validation band apply to the same
  contract, they are merged tightest-wins.
- **Optional eventing**: NATS publishing (`nats` feature) and a
  command/event/journal/replay sequencer (`sequencer` feature). The
  sequenced add path carries order-kind variety — limit, post-only, and
  iceberg ([`orderbook::OrderKind`]) — through the journal so replay
  reconstructs the exact order shape.

### Limitations

- **Not a matching engine.** Order placement, matching, fills, fees, and STP
  at the leaf are `orderbook-rs` behavior. This crate organizes and
  aggregates many `OrderBook<T>` instances; it does not reimplement matching,
  and options math is delegated to `optionstratlib` (no hand-rolled
  Black-Scholes here).
- **Async is opt-in.** `tokio` is pulled in only by the `nats` and
  `sequencer` features. The default build, the hierarchy traversal, and the
  order-submission / quote path are fully synchronous — there is no `.await`
  on the hot path. The matching engine underneath (`orderbook-rs`) is
  lock-free, and the hierarchy itself is lock-free skip-maps + atomics; the
  only mutexes are around rarely-contended state (e.g. opt-in trade capture,
  config holders), not the matching path.
- **`ExpirationDate::Days` is wall-clock-relative.** A `Days(n)` expiry is a
  moving relative day-count: it is resolved against the current clock when
  materialized into a contract date or time-to-expiry, so the same `Days`
  value maps to different calendar dates as time passes. Use
  [`ExpirationDate::DateTime`](optionstratlib::ExpirationDate) for an
  absolute, replay-stable expiry; lifecycle transitions operate only on the
  `DateTime` form.
- **Mark price is a derived, non-journaled value.** It is computed from
  current inputs and is not part of the `sequencer` journal; replay
  reconstructs order-book state, not historical mark prices.
- **Trade IDs are not replay-stable.** The upstream engine mints each
  book's trade-ID namespace with a random `Uuid::new_v4()` and exposes no
  injection seam, so trade IDs differ between a live run and its replay
  even on identical command streams; the replay oracle compares book
  state and excludes trade IDs (tracked upstream as OrderBook-rs#199).
- **Time-in-force replay determinism requires an injected clock.** `GTD` /
  `Day` admission is decided by each leaf's engine clock. Inject a
  deterministic clock (e.g. [`orderbook::StubClock`]) via the hierarchy's
  `set_clock` before the first order, and configure the replaying instance
  with an identically-behaving clock; otherwise leaves stamp and admit
  against the wall clock.
- **Pricing inputs are supplied by the integrator.** The crate ships only a
  trivial [`orderbook::FlatVolSurface`] and mock / static index feeds
  ([`orderbook::MockPriceFeed`], [`orderbook::StaticPriceFeed`]); a
  production volatility surface and a live index price feed are the caller's
  responsibility.

### Example Usage

#### Creating a Hierarchical Order Book

```rust
use option_chain_orderbook::{OrderId, Side, UnderlyingOrderBookManager};
use optionstratlib::prelude::pos_or_panic;
use optionstratlib::ExpirationDate;

let manager = UnderlyingOrderBookManager::new();
let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

// Create BTC option chain (use block to drop guards)
{
    let btc = manager.get_or_create("BTC");
    let exp = btc.get_or_create_expiration(exp_date);
    let strike = exp.get_or_create_strike(50000);

    // Add orders to call
    strike.call().add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        .expect("add order should succeed");
    strike.call().add_limit_order(OrderId::new(), Side::Sell, 105, 5)
        .expect("add order should succeed");

    // Get quote
    let quote = strike.call().best_quote();
    assert!(quote.is_two_sided());
}

// Get statistics
let stats = manager.stats();
```

#### Creating a Single Option Order Book

```rust
use option_chain_orderbook::{OptionOrderBook, OrderId, Side};
use optionstratlib::OptionStyle;

// Create an order book for a specific option
let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

// Add limit orders
book.add_limit_order(OrderId::new(), Side::Buy, 500, 10)
    .expect("add order should succeed");
book.add_limit_order(OrderId::new(), Side::Sell, 520, 5)
    .expect("add order should succeed");

// Get the best quote
let quote = book.best_quote();
assert!(quote.is_two_sided());
```

#### Using OptionStratLib for Greeks

```rust
use optionstratlib::prelude::pos_or_panic;
use optionstratlib::{Options, ExpirationDate};
use optionstratlib::model::types::{OptionStyle, OptionType, Side};
use optionstratlib::greeks::{delta, gamma, theta, vega, rho};
use rust_decimal_macros::dec;

let option = Options {
    option_type: OptionType::European,
    side: Side::Long,
    underlying_symbol: "BTC".to_string(),
    strike_price: pos_or_panic!(50000.0),
    expiration_date: ExpirationDate::Days(pos_or_panic!(30.0)),
    implied_volatility: pos_or_panic!(0.6),
    quantity: pos_or_panic!(1.0),
    underlying_price: pos_or_panic!(48000.0),
    risk_free_rate: dec!(0.05),
    option_style: OptionStyle::Call,
    dividend_yield: pos_or_panic!(0.0),
    exotic_params: None,
};

let delta_value = delta(&option).expect("delta calculation should succeed");
let gamma_value = gamma(&option).expect("gamma calculation should succeed");
```

### Examples

The library includes comprehensive examples demonstrating each level of the hierarchy:

| Example | Description |
|---------|-------------|
| `01_option_orderbook` | Single option order book operations |
| `02_strike_orderbook` | Strike level with call/put pairs |
| `03_chain_orderbook` | Option chain (all strikes for one expiration) |
| `04_expiration_orderbook` | Expiration level with term structure |
| `05_underlying_orderbook` | Underlying level (all expirations) |
| `06_full_hierarchy` | Complete hierarchy with trading scenarios |
| `07_mass_cancel` | Hierarchical mass cancel operations |
| `08_order_lifecycle` | Order state tracking and lifecycle queries |

Run examples with:
```bash
cargo run --example 01_option_orderbook
cargo run --example 06_full_hierarchy
```

### Benchmarks

Comprehensive benchmarks are available for all components:

- **orderbook_bench**: Single option order book operations
- **strike_bench**: Strike order book and manager operations
- **chain_bench**: Option chain order book operations
- **expiration_bench**: Expiration order book operations
- **underlying_bench**: Underlying order book operations
- **hierarchy_bench**: Full hierarchy traversal and trading scenarios

Run benchmarks with:
```bash
cargo bench
cargo bench -- orderbook_benches
cargo bench -- hierarchy_benches
```

### Performance Characteristics

Built on OrderBook-rs's lock-free architecture:

- **Order Operations**: O(log N) for add/cancel operations
- **Best Quote Lookup**: small bounded top-of-book read (best price per side
  plus the aggregate size at that single best level); no caching and no
  full-book scan or heap allocation
- **Thread Safety**: Lock-free operations for concurrent access
- **Hierarchy Traversal**: O(log N) access via `SkipMap`

### Dependencies

See `Cargo.toml` for the exact pinned versions (kept there so this list
cannot go stale). The core dependencies are:

- **orderbook-rs**: lock-free matching engine and price levels — the actual
  order book this crate organizes (`special_orders` feature on)
- **pricelevel**: per-level engine and boundary newtypes (`OrderId`,
  `Price`, `Quantity`, `Side`, `OrderType`, `TimeInForce`, `Hash32`)
- **optionstratlib**: options pricing, Greeks, `ExpirationDate`,
  `OptionStyle`, and `Positive`
- **crossbeam-skiplist**: ordered lock-free skip list (manager children)
- **dashmap**: lock-free concurrent hash map (secondary indexes)
- **rust_decimal**: exact decimal arithmetic for mark price and Greeks
- **thiserror**: typed error handling
- **serde** / **serde_json**: serialization for events and config DTOs
- **tracing**: structured logging (no global subscriber installed by the
  library)
- **tokio** *(optional)*: async runtime, pulled in only by the `nats` and
  `sequencer` features


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
