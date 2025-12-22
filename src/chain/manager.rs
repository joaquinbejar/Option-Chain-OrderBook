//! Top-level option chain manager.
//!
//! This module provides the [`OptionChainManager`] structure for managing
//! the entire option chain for an underlying asset across all expirations
//! and strikes.

use super::contract::{OptionContract, OptionType};
use super::expiration::ExpirationManager;
use super::strike::StrikeManager;
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::BTreeMap;

/// Manages the complete option chain for an underlying asset.
///
/// The option chain manager organizes all options by expiration date,
/// with each expiration containing multiple strikes. This hierarchical
/// structure allows efficient access patterns for market making operations.
///
/// # Example
///
/// ```rust,ignore
/// use option_chain_orderbook::chain::{OptionChainManager, OptionType};
///
/// let mut chain = OptionChainManager::new("BTC");
/// chain.add_contract(contract);
///
/// // Access by expiration and strike
/// let contract = chain.get_contract(expiration, strike, OptionType::Call);
/// ```
#[derive(Debug, Clone)]
pub struct OptionChainManager {
    /// The underlying asset symbol.
    underlying: String,
    /// Expiration managers indexed by expiration date (sorted).
    expirations: BTreeMap<DateTime<Utc>, ExpirationManager>,
}

impl OptionChainManager {
    /// Creates a new option chain manager for the given underlying.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol (e.g., "BTC", "ETH", "SPX")
    ///
    /// # Returns
    ///
    /// A new `OptionChainManager` instance with no expirations.
    #[must_use]
    pub fn new(underlying: impl Into<String>) -> Self {
        Self {
            underlying: underlying.into(),
            expirations: BTreeMap::new(),
        }
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the number of expirations in the chain.
    #[must_use]
    pub fn expiration_count(&self) -> usize {
        self.expirations.len()
    }

    /// Returns the total number of strikes across all expirations.
    #[must_use]
    pub fn total_strike_count(&self) -> usize {
        self.expirations
            .values()
            .map(ExpirationManager::strike_count)
            .sum()
    }

    /// Returns the total number of contracts in the chain.
    #[must_use]
    pub fn total_contract_count(&self) -> usize {
        self.expirations
            .values()
            .map(ExpirationManager::contract_count)
            .sum()
    }

    /// Returns true if the chain has no expirations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.expirations.is_empty()
    }

    /// Adds a contract to the option chain.
    ///
    /// Creates a new expiration manager if the expiration doesn't exist.
    ///
    /// # Arguments
    ///
    /// * `contract` - The option contract to add
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, or an error if the contract's underlying
    /// doesn't match the chain's underlying.
    pub fn add_contract(&mut self, contract: OptionContract) -> Result<()> {
        if contract.underlying() != self.underlying {
            return Err(Error::validation(format!(
                "contract underlying '{}' does not match chain underlying '{}'",
                contract.underlying(),
                self.underlying
            )));
        }

        let expiration = contract.expiration();
        self.expirations
            .entry(expiration)
            .or_insert_with(|| ExpirationManager::new(&self.underlying, expiration))
            .add_contract(contract);

        Ok(())
    }

    /// Gets a reference to the expiration manager for a given date.
    #[must_use]
    pub fn get_expiration(&self, expiration: DateTime<Utc>) -> Option<&ExpirationManager> {
        self.expirations.get(&expiration)
    }

    /// Gets a mutable reference to the expiration manager for a given date.
    pub fn get_expiration_mut(
        &mut self,
        expiration: DateTime<Utc>,
    ) -> Option<&mut ExpirationManager> {
        self.expirations.get_mut(&expiration)
    }

    /// Gets a contract by expiration, strike, and option type.
    #[must_use]
    pub fn get_contract(
        &self,
        expiration: DateTime<Utc>,
        strike: Decimal,
        option_type: OptionType,
    ) -> Option<&OptionContract> {
        self.expirations
            .get(&expiration)
            .and_then(|em| em.get_contract(strike, option_type))
    }

    /// Gets a contract by its symbol.
    #[must_use]
    pub fn get_contract_by_symbol(&self, symbol: &str) -> Option<&OptionContract> {
        self.contracts().find(|c| c.symbol() == symbol)
    }

    /// Gets a strike manager by expiration and strike.
    #[must_use]
    pub fn get_strike(&self, expiration: DateTime<Utc>, strike: Decimal) -> Option<&StrikeManager> {
        self.expirations
            .get(&expiration)
            .and_then(|em| em.get_strike(strike))
    }

    /// Returns an iterator over all expiration dates (sorted).
    pub fn expiration_dates(&self) -> impl Iterator<Item = DateTime<Utc>> + '_ {
        self.expirations.keys().copied()
    }

    /// Returns an iterator over all expiration managers (sorted by date).
    pub fn expiration_managers(&self) -> impl Iterator<Item = &ExpirationManager> {
        self.expirations.values()
    }

    /// Returns a mutable iterator over all expiration managers.
    pub fn expiration_managers_mut(&mut self) -> impl Iterator<Item = &mut ExpirationManager> {
        self.expirations.values_mut()
    }

    /// Returns an iterator over all contracts in the chain.
    pub fn contracts(&self) -> impl Iterator<Item = &OptionContract> {
        self.expirations
            .values()
            .flat_map(ExpirationManager::contracts)
    }

    /// Returns the nearest expiration date.
    #[must_use]
    pub fn nearest_expiration(&self) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        self.expirations.keys().find(|&&exp| exp > now).copied()
    }

    /// Returns the farthest expiration date.
    #[must_use]
    pub fn farthest_expiration(&self) -> Option<DateTime<Utc>> {
        self.expirations.keys().next_back().copied()
    }

    /// Returns expirations within a time range.
    pub fn expirations_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> impl Iterator<Item = (&DateTime<Utc>, &ExpirationManager)> {
        self.expirations.range(start..=end)
    }

    /// Removes expired expirations from the chain.
    ///
    /// # Returns
    ///
    /// The number of expirations removed.
    pub fn remove_expired(&mut self) -> usize {
        let now = Utc::now();
        let expired: Vec<_> = self
            .expirations
            .keys()
            .filter(|&&exp| exp < now)
            .copied()
            .collect();

        let count = expired.len();
        for exp in expired {
            self.expirations.remove(&exp);
        }
        count
    }

    /// Removes an expiration from the chain.
    ///
    /// # Returns
    ///
    /// The removed expiration manager if it existed.
    pub fn remove_expiration(&mut self, expiration: DateTime<Utc>) -> Option<ExpirationManager> {
        self.expirations.remove(&expiration)
    }

    /// Clears all expirations from the chain.
    pub fn clear(&mut self) {
        self.expirations.clear();
    }

    /// Ensures an expiration exists, creating it if necessary.
    ///
    /// # Returns
    ///
    /// A mutable reference to the expiration manager.
    pub fn ensure_expiration(&mut self, expiration: DateTime<Utc>) -> &mut ExpirationManager {
        let underlying = self.underlying.clone();
        self.expirations
            .entry(expiration)
            .or_insert_with(|| ExpirationManager::new(&underlying, expiration))
    }

    /// Returns all unique strike prices across all expirations (sorted).
    #[must_use]
    pub fn all_strikes(&self) -> Vec<Decimal> {
        let mut strikes: Vec<_> = self
            .expirations
            .values()
            .flat_map(ExpirationManager::strike_prices)
            .collect();
        strikes.sort();
        strikes.dedup();
        strikes
    }

    /// Finds the at-the-money strike for a given expiration.
    #[must_use]
    pub fn atm_strike(&self, expiration: DateTime<Utc>, spot: Decimal) -> Option<Decimal> {
        self.expirations
            .get(&expiration)
            .and_then(|em| em.atm_strike(spot))
    }

    /// Returns statistics about the option chain.
    #[must_use]
    pub fn stats(&self) -> OptionChainStats {
        let expirations = self.expiration_count();
        let strikes = self.total_strike_count();
        let contracts = self.total_contract_count();
        let calls = self
            .contracts()
            .filter(|c| c.option_type().is_call())
            .count();
        let puts = contracts - calls;

        OptionChainStats {
            underlying: self.underlying.clone(),
            expirations,
            strikes,
            contracts,
            calls,
            puts,
        }
    }
}

/// Statistics about an option chain.
#[derive(Debug, Clone)]
pub struct OptionChainStats {
    /// The underlying asset symbol.
    pub underlying: String,
    /// Number of expiration dates.
    pub expirations: usize,
    /// Total number of strikes across all expirations.
    pub strikes: usize,
    /// Total number of contracts.
    pub contracts: usize,
    /// Number of call contracts.
    pub calls: usize,
    /// Number of put contracts.
    pub puts: usize,
}

impl std::fmt::Display for OptionChainStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} expirations, {} strikes, {} contracts ({} calls, {} puts)",
            self.underlying, self.expirations, self.strikes, self.contracts, self.calls, self.puts
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn create_expiration(month: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, month, 28, 8, 0, 0).unwrap()
    }

    fn create_contract(
        expiration: DateTime<Utc>,
        strike: Decimal,
        option_type: OptionType,
    ) -> OptionContract {
        OptionContract::new(
            "BTC",
            expiration,
            strike,
            option_type,
            dec!(1),
            dec!(0.0001),
            dec!(0.1),
        )
    }

    #[test]
    fn test_chain_manager_creation() {
        let chain = OptionChainManager::new("BTC");

        assert_eq!(chain.underlying(), "BTC");
        assert!(chain.is_empty());
        assert_eq!(chain.expiration_count(), 0);
        assert_eq!(chain.total_contract_count(), 0);
    }

    #[test]
    fn test_add_contracts() {
        let mut chain = OptionChainManager::new("BTC");
        let exp1 = create_expiration(3);
        let exp2 = create_expiration(6);

        chain
            .add_contract(create_contract(exp1, dec!(50000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp1, dec!(50000), OptionType::Put))
            .unwrap();
        chain
            .add_contract(create_contract(exp2, dec!(55000), OptionType::Call))
            .unwrap();

        assert_eq!(chain.expiration_count(), 2);
        assert_eq!(chain.total_contract_count(), 3);
        assert_eq!(chain.total_strike_count(), 2);
    }

    #[test]
    fn test_add_contract_wrong_underlying() {
        let mut chain = OptionChainManager::new("BTC");
        let exp = create_expiration(3);

        let contract = OptionContract::new(
            "ETH",
            exp,
            dec!(3000),
            OptionType::Call,
            dec!(1),
            dec!(0.0001),
            dec!(0.1),
        );

        let result = chain.add_contract(contract);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_contract() {
        let mut chain = OptionChainManager::new("BTC");
        let exp = create_expiration(3);

        chain
            .add_contract(create_contract(exp, dec!(50000), OptionType::Call))
            .unwrap();

        let contract = chain.get_contract(exp, dec!(50000), OptionType::Call);
        assert!(contract.is_some());
        assert_eq!(contract.unwrap().strike(), dec!(50000));

        let missing = chain.get_contract(exp, dec!(50000), OptionType::Put);
        assert!(missing.is_none());
    }

    #[test]
    fn test_get_contract_by_symbol() {
        let mut chain = OptionChainManager::new("BTC");
        let exp = create_expiration(3);

        chain
            .add_contract(create_contract(exp, dec!(50000), OptionType::Call))
            .unwrap();

        let contract = chain.get_contract_by_symbol("BTC-20240328-50000-C");
        assert!(contract.is_some());
    }

    #[test]
    fn test_expiration_dates_sorted() {
        let mut chain = OptionChainManager::new("BTC");
        let exp1 = create_expiration(6);
        let exp2 = create_expiration(3);
        let exp3 = create_expiration(9);

        chain
            .add_contract(create_contract(exp1, dec!(50000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp2, dec!(50000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp3, dec!(50000), OptionType::Call))
            .unwrap();

        let dates: Vec<_> = chain.expiration_dates().collect();
        assert_eq!(dates.len(), 3);
        assert!(dates[0] < dates[1]);
        assert!(dates[1] < dates[2]);
    }

    #[test]
    fn test_all_strikes() {
        let mut chain = OptionChainManager::new("BTC");
        let exp1 = create_expiration(3);
        let exp2 = create_expiration(6);

        chain
            .add_contract(create_contract(exp1, dec!(50000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp1, dec!(55000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp2, dec!(50000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp2, dec!(60000), OptionType::Call))
            .unwrap();

        let strikes = chain.all_strikes();
        assert_eq!(strikes, vec![dec!(50000), dec!(55000), dec!(60000)]);
    }

    #[test]
    fn test_stats() {
        let mut chain = OptionChainManager::new("BTC");
        let exp = create_expiration(3);

        chain
            .add_contract(create_contract(exp, dec!(50000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp, dec!(50000), OptionType::Put))
            .unwrap();
        chain
            .add_contract(create_contract(exp, dec!(55000), OptionType::Call))
            .unwrap();

        let stats = chain.stats();
        assert_eq!(stats.underlying, "BTC");
        assert_eq!(stats.expirations, 1);
        assert_eq!(stats.strikes, 2);
        assert_eq!(stats.contracts, 3);
        assert_eq!(stats.calls, 2);
        assert_eq!(stats.puts, 1);
    }

    #[test]
    fn test_remove_expiration() {
        let mut chain = OptionChainManager::new("BTC");
        let exp1 = create_expiration(3);
        let exp2 = create_expiration(6);

        chain
            .add_contract(create_contract(exp1, dec!(50000), OptionType::Call))
            .unwrap();
        chain
            .add_contract(create_contract(exp2, dec!(50000), OptionType::Call))
            .unwrap();

        let removed = chain.remove_expiration(exp1);
        assert!(removed.is_some());
        assert_eq!(chain.expiration_count(), 1);
        assert!(chain.get_expiration(exp1).is_none());
    }
}
