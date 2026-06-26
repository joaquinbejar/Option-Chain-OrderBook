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
//! ```rust,ignore
//! use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
//!
//! let mut manager = UnderlyingOrderBookManager::new();
//!
//! // Create BTC option chain
//! let btc = manager.get_or_create("BTC");
//! let exp = btc.get_or_create_expiration("20240329");
//! let strike = exp.get_or_create_strike(50000);
//!
//! // Add orders to call
//! strike.call().add_limit_order(order_id, Side::Buy, 100, 10)?;
//!
//! // Get quote
//! let quote = strike.call().best_quote();
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
mod index_price_feed;
mod instrument_registry;
mod instrument_status;
mod mark_price;
mod quote;
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
    ChainMassCancelResult, OptionChainOrderBook, OptionChainOrderBookManager, OptionChainStats,
};
pub use contract_specs::{ContractSpecs, ContractSpecsBuilder, ExerciseStyle, SettlementType};
pub use expiration::{
    ExpirationManagerStats, ExpirationMassCancelResult, ExpirationOrderBook,
    ExpirationOrderBookManager,
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
pub use index_price_feed::{
    IndexPriceFeed, MockPriceFeed, PriceUpdate, PriceUpdateListener, StaticPriceFeed,
    SubscriptionId, wire_feed_to_calculator,
};
pub use instrument_registry::{InstrumentInfo, InstrumentRegistry};
pub use instrument_status::InstrumentStatus;
pub use mark_price::{MarkPriceCalculator, MarkPriceConfig, MarkPriceConfigBuilder};
pub use quote::{Quote, QuoteUpdate};
pub use strike::{StrikeMassCancelResult, StrikeOrderBook, StrikeOrderBookManager};
pub use strike_generator::{CleanupResult, StrikeGenerator};
pub use strike_range::{ExpiryType, StrikeRangeConfig, StrikeRangeConfigBuilder};
pub use symbol_index::{SymbolIndex, SymbolRef};
pub use underlying::{
    GlobalMassCancelResult, GlobalStats, UnderlyingMassCancelResult, UnderlyingOrderBook,
    UnderlyingOrderBookManager, UnderlyingStats,
};
pub use validation::ValidationConfig;

#[cfg(feature = "nats")]
pub use nats::{
    NatsPublisherHandles, OptionChainNatsConfig, OptionChainSubjectBuilder,
    build_option_order_book_with_nats,
};

#[cfg(feature = "sequencer")]
pub use sequencer::{
    InMemoryOptionChainJournal, MassCancelScope, MassCancelType, OptionChainCommand,
    OptionChainEvent, OptionChainJournal, OptionChainReceipt, OptionChainResult,
    SequencedUnderlyingOrderBook,
};

// Re-export upstream types used in the public API
pub use orderbook_rs::{
    CancelReason, FeeSchedule, MassCancelResult, OrderStateTracker, OrderStatus, STPMode,
    TradeResult,
};
pub use pricelevel::Hash32;
