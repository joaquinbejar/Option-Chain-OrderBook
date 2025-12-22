//! Market data handler for processing updates.

use super::types::{MarketDataUpdate, TickData};
use std::collections::HashMap;

/// Handler for processing market data updates.
#[derive(Debug, Default)]
pub struct MarketDataHandler {
    /// Latest tick data by symbol.
    ticks: HashMap<String, TickData>,
    /// Last sequence number by symbol.
    sequences: HashMap<String, u64>,
}

impl MarketDataHandler {
    /// Creates a new market data handler.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ticks: HashMap::new(),
            sequences: HashMap::new(),
        }
    }

    /// Processes a market data update.
    ///
    /// # Returns
    ///
    /// `true` if the update was applied, `false` if it was stale.
    pub fn process_update(&mut self, update: MarketDataUpdate) -> bool {
        let last_seq = self.sequences.get(&update.symbol).copied().unwrap_or(0);

        // Check for stale update
        if update.sequence <= last_seq {
            return false;
        }

        self.ticks.insert(update.symbol.clone(), update.tick);
        self.sequences.insert(update.symbol, update.sequence);
        true
    }

    /// Gets the latest tick data for a symbol.
    #[must_use]
    pub fn get_tick(&self, symbol: &str) -> Option<&TickData> {
        self.ticks.get(symbol)
    }

    /// Gets the latest mid price for a symbol.
    #[must_use]
    pub fn get_mid_price(&self, symbol: &str) -> Option<f64> {
        self.ticks.get(symbol).and_then(TickData::mid_price)
    }

    /// Returns the number of symbols being tracked.
    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.ticks.len()
    }

    /// Clears all market data.
    pub fn clear(&mut self) {
        self.ticks.clear();
        self.sequences.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_data::types::UpdateType;

    #[test]
    fn test_process_update() {
        let mut handler = MarketDataHandler::new();

        let update = MarketDataUpdate {
            symbol: "BTC".to_string(),
            update_type: UpdateType::Snapshot,
            tick: TickData {
                bid_price: Some(50000),
                bid_size: 10,
                ask_price: Some(50010),
                ask_size: 5,
                last_price: None,
                last_size: 0,
                timestamp_ms: 1000,
            },
            sequence: 1,
        };

        assert!(handler.process_update(update));
        assert_eq!(handler.symbol_count(), 1);

        let tick = handler.get_tick("BTC").unwrap();
        assert_eq!(tick.bid_price, Some(50000));
    }

    #[test]
    fn test_stale_update_rejected() {
        let mut handler = MarketDataHandler::new();

        let update1 = MarketDataUpdate {
            symbol: "BTC".to_string(),
            update_type: UpdateType::Snapshot,
            tick: TickData {
                bid_price: Some(50000),
                bid_size: 10,
                ask_price: Some(50010),
                ask_size: 5,
                last_price: None,
                last_size: 0,
                timestamp_ms: 1000,
            },
            sequence: 2,
        };

        let update2 = MarketDataUpdate {
            symbol: "BTC".to_string(),
            update_type: UpdateType::Delta,
            tick: TickData {
                bid_price: Some(49990),
                bid_size: 10,
                ask_price: Some(50000),
                ask_size: 5,
                last_price: None,
                last_size: 0,
                timestamp_ms: 999,
            },
            sequence: 1, // Stale sequence
        };

        assert!(handler.process_update(update1));
        assert!(!handler.process_update(update2)); // Should be rejected

        // Original data should be preserved
        let tick = handler.get_tick("BTC").unwrap();
        assert_eq!(tick.bid_price, Some(50000));
    }
}
