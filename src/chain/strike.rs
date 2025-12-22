//! Strike price management for option chains.
//!
//! This module provides the [`StrikeManager`] structure for managing options
//! at a specific strike price, including both call and put contracts.

use super::contract::{OptionContract, OptionType};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Manages options at a specific strike price.
///
/// A strike manager holds both the call and put option at a given strike,
/// along with any associated metadata for that strike level.
#[derive(Debug, Clone)]
pub struct StrikeManager {
    /// The strike price this manager handles.
    strike: Decimal,
    /// Call option at this strike (if exists).
    call: Option<OptionContract>,
    /// Put option at this strike (if exists).
    put: Option<OptionContract>,
    /// Custom metadata associated with this strike.
    metadata: HashMap<String, String>,
}

impl StrikeManager {
    /// Creates a new strike manager for the given strike price.
    ///
    /// # Arguments
    ///
    /// * `strike` - The strike price to manage
    ///
    /// # Returns
    ///
    /// A new `StrikeManager` instance with no contracts.
    #[must_use]
    pub fn new(strike: Decimal) -> Self {
        Self {
            strike,
            call: None,
            put: None,
            metadata: HashMap::new(),
        }
    }

    /// Returns the strike price.
    #[must_use]
    pub const fn strike(&self) -> Decimal {
        self.strike
    }

    /// Returns a reference to the call option if it exists.
    #[must_use]
    pub const fn call(&self) -> Option<&OptionContract> {
        self.call.as_ref()
    }

    /// Returns a reference to the put option if it exists.
    #[must_use]
    pub const fn put(&self) -> Option<&OptionContract> {
        self.put.as_ref()
    }

    /// Returns a mutable reference to the call option if it exists.
    pub fn call_mut(&mut self) -> Option<&mut OptionContract> {
        self.call.as_mut()
    }

    /// Returns a mutable reference to the put option if it exists.
    pub fn put_mut(&mut self) -> Option<&mut OptionContract> {
        self.put.as_mut()
    }

    /// Sets the call option for this strike.
    ///
    /// # Arguments
    ///
    /// * `contract` - The call option contract
    ///
    /// # Panics
    ///
    /// Panics if the contract is not a call option or has a different strike.
    pub fn set_call(&mut self, contract: OptionContract) {
        assert!(
            contract.option_type().is_call(),
            "contract must be a call option"
        );
        assert_eq!(
            contract.strike(),
            self.strike,
            "contract strike must match manager strike"
        );
        self.call = Some(contract);
    }

    /// Sets the put option for this strike.
    ///
    /// # Arguments
    ///
    /// * `contract` - The put option contract
    ///
    /// # Panics
    ///
    /// Panics if the contract is not a put option or has a different strike.
    pub fn set_put(&mut self, contract: OptionContract) {
        assert!(
            contract.option_type().is_put(),
            "contract must be a put option"
        );
        assert_eq!(
            contract.strike(),
            self.strike,
            "contract strike must match manager strike"
        );
        self.put = Some(contract);
    }

    /// Adds a contract to this strike manager.
    ///
    /// The contract is automatically placed in the call or put slot based on
    /// its option type.
    ///
    /// # Arguments
    ///
    /// * `contract` - The option contract to add
    ///
    /// # Panics
    ///
    /// Panics if the contract has a different strike price.
    pub fn add_contract(&mut self, contract: OptionContract) {
        assert_eq!(
            contract.strike(),
            self.strike,
            "contract strike must match manager strike"
        );

        match contract.option_type() {
            OptionType::Call => self.call = Some(contract),
            OptionType::Put => self.put = Some(contract),
        }
    }

    /// Returns the contract for the given option type if it exists.
    #[must_use]
    pub const fn get_contract(&self, option_type: OptionType) -> Option<&OptionContract> {
        match option_type {
            OptionType::Call => self.call.as_ref(),
            OptionType::Put => self.put.as_ref(),
        }
    }

    /// Returns true if both call and put exist at this strike.
    #[must_use]
    pub const fn has_both(&self) -> bool {
        self.call.is_some() && self.put.is_some()
    }

    /// Returns true if at least one contract exists at this strike.
    #[must_use]
    pub const fn has_any(&self) -> bool {
        self.call.is_some() || self.put.is_some()
    }

    /// Returns the number of contracts at this strike (0, 1, or 2).
    #[must_use]
    pub fn contract_count(&self) -> usize {
        self.call.is_some() as usize + self.put.is_some() as usize
    }

    /// Returns an iterator over all contracts at this strike.
    pub fn contracts(&self) -> impl Iterator<Item = &OptionContract> {
        self.call.iter().chain(self.put.iter())
    }

    /// Sets metadata for this strike.
    pub fn set_metadata(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.metadata.insert(key.into(), value.into());
    }

    /// Gets metadata for this strike.
    #[must_use]
    pub fn get_metadata(&self, key: &str) -> Option<&String> {
        self.metadata.get(key)
    }

    /// Removes the call option from this strike.
    ///
    /// # Returns
    ///
    /// The removed call option if it existed.
    pub fn remove_call(&mut self) -> Option<OptionContract> {
        self.call.take()
    }

    /// Removes the put option from this strike.
    ///
    /// # Returns
    ///
    /// The removed put option if it existed.
    pub fn remove_put(&mut self) -> Option<OptionContract> {
        self.put.take()
    }

    /// Clears all contracts from this strike.
    pub fn clear(&mut self) {
        self.call = None;
        self.put = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    fn create_call_contract(strike: Decimal) -> OptionContract {
        let expiration = Utc.with_ymd_and_hms(2024, 12, 31, 8, 0, 0).unwrap();
        OptionContract::new(
            "BTC",
            expiration,
            strike,
            OptionType::Call,
            dec!(1),
            dec!(0.0001),
            dec!(0.1),
        )
    }

    fn create_put_contract(strike: Decimal) -> OptionContract {
        let expiration = Utc.with_ymd_and_hms(2024, 12, 31, 8, 0, 0).unwrap();
        OptionContract::new(
            "BTC",
            expiration,
            strike,
            OptionType::Put,
            dec!(1),
            dec!(0.0001),
            dec!(0.1),
        )
    }

    #[test]
    fn test_strike_manager_creation() {
        let manager = StrikeManager::new(dec!(50000));
        assert_eq!(manager.strike(), dec!(50000));
        assert!(manager.call().is_none());
        assert!(manager.put().is_none());
        assert!(!manager.has_any());
        assert!(!manager.has_both());
    }

    #[test]
    fn test_add_call_contract() {
        let mut manager = StrikeManager::new(dec!(50000));
        let call = create_call_contract(dec!(50000));

        manager.set_call(call);

        assert!(manager.call().is_some());
        assert!(manager.put().is_none());
        assert!(manager.has_any());
        assert!(!manager.has_both());
        assert_eq!(manager.contract_count(), 1);
    }

    #[test]
    fn test_add_both_contracts() {
        let mut manager = StrikeManager::new(dec!(50000));
        let call = create_call_contract(dec!(50000));
        let put = create_put_contract(dec!(50000));

        manager.add_contract(call);
        manager.add_contract(put);

        assert!(manager.has_both());
        assert_eq!(manager.contract_count(), 2);
    }

    #[test]
    fn test_get_contract_by_type() {
        let mut manager = StrikeManager::new(dec!(50000));
        let call = create_call_contract(dec!(50000));
        manager.add_contract(call);

        assert!(manager.get_contract(OptionType::Call).is_some());
        assert!(manager.get_contract(OptionType::Put).is_none());
    }

    #[test]
    fn test_contracts_iterator() {
        let mut manager = StrikeManager::new(dec!(50000));
        let call = create_call_contract(dec!(50000));
        let put = create_put_contract(dec!(50000));

        manager.add_contract(call);
        manager.add_contract(put);

        let contracts: Vec<_> = manager.contracts().collect();
        assert_eq!(contracts.len(), 2);
    }

    #[test]
    fn test_metadata() {
        let mut manager = StrikeManager::new(dec!(50000));
        manager.set_metadata("liquidity", "high");

        assert_eq!(manager.get_metadata("liquidity"), Some(&"high".to_string()));
        assert_eq!(manager.get_metadata("nonexistent"), None);
    }

    #[test]
    fn test_remove_contracts() {
        let mut manager = StrikeManager::new(dec!(50000));
        let call = create_call_contract(dec!(50000));
        let put = create_put_contract(dec!(50000));

        manager.add_contract(call);
        manager.add_contract(put);

        let removed_call = manager.remove_call();
        assert!(removed_call.is_some());
        assert!(manager.call().is_none());
        assert!(manager.put().is_some());

        manager.clear();
        assert!(!manager.has_any());
    }

    #[test]
    #[should_panic(expected = "contract must be a call option")]
    fn test_set_call_with_put_panics() {
        let mut manager = StrikeManager::new(dec!(50000));
        let put = create_put_contract(dec!(50000));
        manager.set_call(put);
    }

    #[test]
    #[should_panic(expected = "contract strike must match manager strike")]
    fn test_add_contract_wrong_strike_panics() {
        let mut manager = StrikeManager::new(dec!(50000));
        let call = create_call_contract(dec!(60000));
        manager.add_contract(call);
    }
}
