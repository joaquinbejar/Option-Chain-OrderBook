//! Market data types.

use serde::{Deserialize, Serialize};

/// Tick data for an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickData {
    /// Best bid price in smallest units.
    pub bid_price: Option<u64>,
    /// Best bid size.
    pub bid_size: u64,
    /// Best ask price in smallest units.
    pub ask_price: Option<u64>,
    /// Best ask size.
    pub ask_size: u64,
    /// Last trade price.
    pub last_price: Option<u64>,
    /// Last trade size.
    pub last_size: u64,
    /// Timestamp in milliseconds.
    pub timestamp_ms: u64,
}

impl TickData {
    /// Returns the mid price if both bid and ask exist.
    #[must_use]
    pub fn mid_price(&self) -> Option<f64> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => Some((bid as f64 + ask as f64) / 2.0),
            _ => None,
        }
    }

    /// Returns the spread if both bid and ask exist.
    #[must_use]
    pub fn spread(&self) -> Option<u64> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => Some(ask.saturating_sub(bid)),
            _ => None,
        }
    }
}

/// Market data update event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDataUpdate {
    /// Symbol this update is for.
    pub symbol: String,
    /// Update type.
    pub update_type: UpdateType,
    /// Tick data.
    pub tick: TickData,
    /// Sequence number for ordering.
    pub sequence: u64,
}

/// Type of market data update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateType {
    /// Snapshot of current state.
    Snapshot,
    /// Incremental update.
    Delta,
    /// Trade occurred.
    Trade,
}
