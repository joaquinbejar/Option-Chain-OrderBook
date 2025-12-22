//! Option chain management module.
//!
//! This module provides structures and functionality for managing option chains,
//! including contracts, expirations, and strikes. It serves as the core data
//! structure for organizing options across multiple expiration dates and strike
//! prices.
//!
//! ## Components
//!
//! - [`OptionContract`]: Represents a single option contract with all its properties
//! - [`ExpirationManager`]: Manages options grouped by expiration date
//! - [`StrikeManager`]: Manages options at a specific strike price
//! - [`OptionChainManager`]: Top-level manager for the entire option chain
//!
//! ## Example
//!
//! ```rust,ignore
//! use option_chain_orderbook::chain::{OptionChainManager, OptionType};
//!
//! let mut chain = OptionChainManager::new("BTC");
//! chain.add_contract(contract);
//! ```

mod contract;
mod expiration;
mod manager;
mod strike;

pub use contract::{OptionContract, OptionType};
pub use expiration::ExpirationManager;
pub use manager::OptionChainManager;
pub use strike::StrikeManager;
