//! Order book integration module.
//!
//! This module provides a hierarchical order book structure for options trading:
//!
//! ## Hierarchy
//!
//! ```text
//! UnderlyingOrderBookManager (manages all underlyings: BTC, ETH, SPX, etc.)
//!   └── UnderlyingOrderBook (per underlying, all expirations for one asset)
//!         └── ExpirationOrderBookManager (manages all expirations for underlying)
//!               └── ExpirationOrderBook (per expiry date)
//!                     └── OptionChainOrderBook (per expiration, option chain of all strikes)
//!                           └── StrikeOrderBookManager (manages call/put pair)
//!                                 └── StrikeOrderBook (per strike price, two OptionOrderBook)
//!                                       └── OptionOrderBook (call or put)
//!                                             └── OrderBook<T> (from OrderBook-rs)
//! ```
//!
//! ## Components
//!
//! - [`UnderlyingOrderBookManager`]: Top-level manager for all underlyings
//! - [`UnderlyingOrderBook`]: All expirations for a single underlying
//! - [`ExpirationOrderBookManager`]: Manages expirations for an underlying
//! - [`ExpirationOrderBook`]: All strikes for a single expiration
//! - [`OptionChainOrderBook`]: Option chain with strike management
//! - [`StrikeOrderBookManager`]: Manages strikes for an expiration
//! - [`StrikeOrderBook`]: Call/put pair at a strike price
//! - [`OptionOrderBook`]: Single option order book (call or put)
//! - [`Quote`]: Represents a two-sided quote (bid and ask)
//!
//! ## Example
//!
//! ```rust
//! use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
//! use option_chain_orderbook::{OrderId, Side};
//! use optionstratlib::ExpirationDate;
//! use optionstratlib::prelude::pos_or_panic;
//!
//! let manager = UnderlyingOrderBookManager::new();
//!
//! // Create a BTC option chain at a 30-day expiry and a 50_000 strike.
//! let btc = manager.get_or_create("BTC");
//! let exp = btc.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(30.0)));
//! let strike = exp.get_or_create_strike(50_000);
//!
//! // Add a two-sided market to the call book.
//! strike.call().add_limit_order(OrderId::new(), Side::Buy, 100, 10)
//!     .expect("add bid should succeed");
//! strike.call().add_limit_order(OrderId::new(), Side::Sell, 105, 5)
//!     .expect("add ask should succeed");
//!
//! // Read the best quote.
//! let quote = strike.call().best_quote();
//! assert!(quote.is_two_sided());
//! ```

mod book;
mod chain;
mod contract_specs;
mod expiration;
mod expiration_key;
mod expiry_cycle;
mod expiry_lifecycle;
mod expiry_scheduler;
mod fees;
pub mod greeks_aggregator;
pub mod greeks_engine;
mod index_feed;
mod index_price_feed;
mod instrument_registry;
mod instrument_status;
mod mark_price;
mod quote;
pub(crate) mod shared;
mod stp;
mod strike;
mod strike_generator;
mod strike_range;
pub mod symbol_index;
mod underlying;
mod validation;

#[cfg(feature = "nats")]
pub mod nats;

#[cfg(feature = "sequencer")]
pub mod sequencer;

// Re-export all public types
pub use book::{OptionOrderBook, TerminalOrderSummary};
pub use chain::{
    ChainEvictExpiredResult, ChainMassCancelResult, OptionChainOrderBook,
    OptionChainOrderBookManager, OptionChainStats,
};
pub use contract_specs::{ContractSpecs, ContractSpecsBuilder, ExerciseStyle, SettlementType};
pub use expiration::{
    ExpirationEvictExpiredResult, ExpirationManagerStats, ExpirationMassCancelResult,
    ExpirationOrderBook, ExpirationOrderBookManager,
};
pub use expiry_cycle::{CycleRule, ExpiryCycleConfig};
pub use expiry_lifecycle::{
    ExpiryLifecycleManager, LifecycleConfig, LifecycleEvent, LifecycleListener, LifecycleResult,
};
pub use expiry_scheduler::{ExpirationCallback, ExpiryScheduler, RefreshResult};
pub use greeks_aggregator::{AggregatedGreeks, GreeksAggregator, Position};
pub use greeks_engine::{
    FlatVolSurface, GreeksEngine, GreeksRecalcTrigger, GreeksUpdate, GreeksUpdateListener,
    VolSurface, calculate_tte_years,
};
// The `IndexPriceFeed` trait and its value types live in the neutral
// `index_feed` module (so the core hierarchy can depend on them without pulling
// in the pricing subsystem); the concrete feeds and the calculator wiring live
// in the pricing module `index_price_feed`. Both are re-exported here so the
// public paths (`orderbook::IndexPriceFeed`, `orderbook::PriceUpdate`, …) are
// unchanged.
pub use index_feed::{IndexPriceFeed, PriceUpdate, PriceUpdateListener, SubscriptionId};
pub use index_price_feed::{MockPriceFeed, StaticPriceFeed, wire_feed_to_calculator};
pub use instrument_registry::{InstrumentInfo, InstrumentRegistry};
pub use instrument_status::InstrumentStatus;
pub use mark_price::{MarkPriceCalculator, MarkPriceConfig, MarkPriceConfigBuilder};
pub use quote::{Quote, QuoteUpdate};
pub use strike::{
    StrikeEvictExpiredResult, StrikeMassCancelResult, StrikeOrderBook, StrikeOrderBookManager,
};
pub use strike_generator::{CleanupResult, StrikeGenerator};
pub use strike_range::{ExpiryType, StrikeRangeConfig, StrikeRangeConfigBuilder};
pub use symbol_index::{SymbolIndex, SymbolRef};
pub use underlying::{
    GlobalEvictExpiredResult, GlobalMassCancelResult, GlobalStats, UnderlyingEvictExpiredResult,
    UnderlyingMassCancelResult, UnderlyingOrderBook, UnderlyingOrderBookManager, UnderlyingStats,
};
pub use validation::ValidationConfig;

#[cfg(feature = "nats")]
pub use nats::{
    NatsPublisherHandles, OptionChainNatsConfig, OptionChainSubjectBuilder,
    build_option_order_book_with_nats, build_underlying_manager_with_nats,
};

#[cfg(feature = "sequencer")]
pub use sequencer::{
    InMemoryOptionChainJournal, MassCancelScope, MassCancelType, OptionChainCommand,
    OptionChainEvent, OptionChainJournal, OptionChainReceipt, OptionChainResult, OrderKind,
    SequencedUnderlyingOrderBook,
};

// Re-export upstream types used in the public API.
//
// The boundary newtypes (`OrderId`, `OrderType`, `Side`, `TimeInForce`, `Hash32`)
// are re-exported here so downstream consumers need no direct `orderbook_rs` /
// `pricelevel` dependency to use this crate's public surface.
pub use orderbook_rs::{
    CancelReason, Clock, FeeSchedule, MassCancelResult, MonotonicClock, OrderId, OrderStateTracker,
    OrderStatus, OrderType, STPMode, Side, StubClock, TimeInForce, TradeResult,
};
pub use pricelevel::{Hash32, Price, Quantity, TimestampMs};
// Re-export `Uuid` so downstream consumers can set trade-ID namespaces without
// taking their own direct `uuid` dependency (boundary-type policy).
pub use uuid::Uuid;
