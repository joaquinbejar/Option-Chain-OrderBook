//! Expiration date management for option chains.
//!
//! This module provides the [`ExpirationManager`] structure for managing all
//! options that share the same expiration date, organized by strike price.

use super::contract::{OptionContract, OptionType};
use super::strike::StrikeManager;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::BTreeMap;

/// Manages all options for a specific expiration date.
///
/// Options are organized by strike price using a `BTreeMap` to maintain
/// sorted order, which is useful for volatility surface calculations and
/// strike selection.
#[derive(Debug, Clone)]
pub struct ExpirationManager {
    /// The expiration date this manager handles.
    expiration: DateTime<Utc>,
    /// Strike managers indexed by strike price (sorted).
    strikes: BTreeMap<Decimal, StrikeManager>,
    /// The underlying asset symbol.
    underlying: String,
}

impl ExpirationManager {
    /// Creates a new expiration manager.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `expiration` - The expiration date and time
    ///
    /// # Returns
    ///
    /// A new `ExpirationManager` instance with no strikes.
    #[must_use]
    pub fn new(underlying: impl Into<String>, expiration: DateTime<Utc>) -> Self {
        Self {
            expiration,
            strikes: BTreeMap::new(),
            underlying: underlying.into(),
        }
    }

    /// Returns the expiration date.
    #[must_use]
    pub const fn expiration(&self) -> DateTime<Utc> {
        self.expiration
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the number of strikes in this expiration.
    #[must_use]
    pub fn strike_count(&self) -> usize {
        self.strikes.len()
    }

    /// Returns the total number of contracts across all strikes.
    #[must_use]
    pub fn contract_count(&self) -> usize {
        self.strikes
            .values()
            .map(StrikeManager::contract_count)
            .sum()
    }

    /// Returns true if this expiration has no strikes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strikes.is_empty()
    }

    /// Returns true if this expiration has expired.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expiration < Utc::now()
    }

    /// Returns the time to expiration in years.
    #[must_use]
    pub fn time_to_expiry_years(&self) -> Decimal {
        let now = Utc::now();
        if self.expiration <= now {
            return Decimal::ZERO;
        }

        let duration = self.expiration - now;
        let seconds = Decimal::from(duration.num_seconds());
        let seconds_per_year = Decimal::from(365 * 24 * 60 * 60);

        seconds / seconds_per_year
    }

    /// Returns the time to expiration in days.
    #[must_use]
    pub fn time_to_expiry_days(&self) -> Decimal {
        let now = Utc::now();
        if self.expiration <= now {
            return Decimal::ZERO;
        }

        let duration = self.expiration - now;
        Decimal::from(duration.num_days())
    }

    /// Adds a contract to this expiration.
    ///
    /// Creates a new strike manager if the strike doesn't exist.
    ///
    /// # Arguments
    ///
    /// * `contract` - The option contract to add
    ///
    /// # Panics
    ///
    /// Panics if the contract has a different expiration date.
    pub fn add_contract(&mut self, contract: OptionContract) {
        assert_eq!(
            contract.expiration(),
            self.expiration,
            "contract expiration must match manager expiration"
        );

        let strike = contract.strike();
        self.strikes
            .entry(strike)
            .or_insert_with(|| StrikeManager::new(strike))
            .add_contract(contract);
    }

    /// Gets a reference to the strike manager for a given strike price.
    #[must_use]
    pub fn get_strike(&self, strike: Decimal) -> Option<&StrikeManager> {
        self.strikes.get(&strike)
    }

    /// Gets a mutable reference to the strike manager for a given strike price.
    pub fn get_strike_mut(&mut self, strike: Decimal) -> Option<&mut StrikeManager> {
        self.strikes.get_mut(&strike)
    }

    /// Gets a contract by strike and option type.
    #[must_use]
    pub fn get_contract(
        &self,
        strike: Decimal,
        option_type: OptionType,
    ) -> Option<&OptionContract> {
        self.strikes
            .get(&strike)
            .and_then(|sm| sm.get_contract(option_type))
    }

    /// Returns an iterator over all strike prices (sorted).
    pub fn strike_prices(&self) -> impl Iterator<Item = Decimal> + '_ {
        self.strikes.keys().copied()
    }

    /// Returns an iterator over all strike managers (sorted by strike).
    pub fn strike_managers(&self) -> impl Iterator<Item = &StrikeManager> {
        self.strikes.values()
    }

    /// Returns a mutable iterator over all strike managers.
    pub fn strike_managers_mut(&mut self) -> impl Iterator<Item = &mut StrikeManager> {
        self.strikes.values_mut()
    }

    /// Returns an iterator over all contracts in this expiration.
    pub fn contracts(&self) -> impl Iterator<Item = &OptionContract> {
        self.strikes.values().flat_map(StrikeManager::contracts)
    }

    /// Returns the lowest strike price.
    #[must_use]
    pub fn min_strike(&self) -> Option<Decimal> {
        self.strikes.keys().next().copied()
    }

    /// Returns the highest strike price.
    #[must_use]
    pub fn max_strike(&self) -> Option<Decimal> {
        self.strikes.keys().next_back().copied()
    }

    /// Returns strikes within a range (inclusive).
    pub fn strikes_in_range(
        &self,
        min: Decimal,
        max: Decimal,
    ) -> impl Iterator<Item = (&Decimal, &StrikeManager)> {
        self.strikes.range(min..=max)
    }

    /// Finds the at-the-money strike (closest to spot price).
    #[must_use]
    pub fn atm_strike(&self, spot: Decimal) -> Option<Decimal> {
        self.strikes
            .keys()
            .min_by_key(|&strike| {
                let diff = *strike - spot;
                if diff >= Decimal::ZERO { diff } else { -diff }
            })
            .copied()
    }

    /// Removes a strike from this expiration.
    ///
    /// # Returns
    ///
    /// The removed strike manager if it existed.
    pub fn remove_strike(&mut self, strike: Decimal) -> Option<StrikeManager> {
        self.strikes.remove(&strike)
    }

    /// Clears all strikes from this expiration.
    pub fn clear(&mut self) {
        self.strikes.clear();
    }

    /// Ensures a strike exists, creating it if necessary.
    ///
    /// # Returns
    ///
    /// A mutable reference to the strike manager.
    pub fn ensure_strike(&mut self, strike: Decimal) -> &mut StrikeManager {
        self.strikes
            .entry(strike)
            .or_insert_with(|| StrikeManager::new(strike))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn create_expiration() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 12, 31, 8, 0, 0).unwrap()
    }

    fn create_contract(strike: Decimal, option_type: OptionType) -> OptionContract {
        OptionContract::new(
            "BTC",
            create_expiration(),
            strike,
            option_type,
            dec!(1),
            dec!(0.0001),
            dec!(0.1),
        )
    }

    #[test]
    fn test_expiration_manager_creation() {
        let exp = create_expiration();
        let manager = ExpirationManager::new("BTC", exp);

        assert_eq!(manager.expiration(), exp);
        assert_eq!(manager.underlying(), "BTC");
        assert!(manager.is_empty());
        assert_eq!(manager.strike_count(), 0);
        assert_eq!(manager.contract_count(), 0);
    }

    #[test]
    fn test_add_contracts() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        manager.add_contract(create_contract(dec!(50000), OptionType::Call));
        manager.add_contract(create_contract(dec!(50000), OptionType::Put));
        manager.add_contract(create_contract(dec!(55000), OptionType::Call));

        assert_eq!(manager.strike_count(), 2);
        assert_eq!(manager.contract_count(), 3);
    }

    #[test]
    fn test_get_contract() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        manager.add_contract(create_contract(dec!(50000), OptionType::Call));

        let contract = manager.get_contract(dec!(50000), OptionType::Call);
        assert!(contract.is_some());
        assert_eq!(contract.unwrap().strike(), dec!(50000));

        let missing = manager.get_contract(dec!(50000), OptionType::Put);
        assert!(missing.is_none());
    }

    #[test]
    fn test_strike_prices_sorted() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        manager.add_contract(create_contract(dec!(55000), OptionType::Call));
        manager.add_contract(create_contract(dec!(45000), OptionType::Call));
        manager.add_contract(create_contract(dec!(50000), OptionType::Call));

        let strikes: Vec<_> = manager.strike_prices().collect();
        assert_eq!(strikes, vec![dec!(45000), dec!(50000), dec!(55000)]);
    }

    #[test]
    fn test_min_max_strike() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        manager.add_contract(create_contract(dec!(45000), OptionType::Call));
        manager.add_contract(create_contract(dec!(55000), OptionType::Call));

        assert_eq!(manager.min_strike(), Some(dec!(45000)));
        assert_eq!(manager.max_strike(), Some(dec!(55000)));
    }

    #[test]
    fn test_atm_strike() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        manager.add_contract(create_contract(dec!(45000), OptionType::Call));
        manager.add_contract(create_contract(dec!(50000), OptionType::Call));
        manager.add_contract(create_contract(dec!(55000), OptionType::Call));

        assert_eq!(manager.atm_strike(dec!(49000)), Some(dec!(50000)));
        assert_eq!(manager.atm_strike(dec!(52000)), Some(dec!(50000)));
        assert_eq!(manager.atm_strike(dec!(53000)), Some(dec!(55000)));
    }

    #[test]
    fn test_strikes_in_range() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        manager.add_contract(create_contract(dec!(45000), OptionType::Call));
        manager.add_contract(create_contract(dec!(50000), OptionType::Call));
        manager.add_contract(create_contract(dec!(55000), OptionType::Call));
        manager.add_contract(create_contract(dec!(60000), OptionType::Call));

        let in_range: Vec<_> = manager
            .strikes_in_range(dec!(48000), dec!(56000))
            .map(|(k, _)| *k)
            .collect();
        assert_eq!(in_range, vec![dec!(50000), dec!(55000)]);
    }

    #[test]
    fn test_remove_strike() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        manager.add_contract(create_contract(dec!(50000), OptionType::Call));
        manager.add_contract(create_contract(dec!(55000), OptionType::Call));

        let removed = manager.remove_strike(dec!(50000));
        assert!(removed.is_some());
        assert_eq!(manager.strike_count(), 1);
        assert!(manager.get_strike(dec!(50000)).is_none());
    }

    #[test]
    #[should_panic(expected = "contract expiration must match manager expiration")]
    fn test_add_contract_wrong_expiration_panics() {
        let exp = create_expiration();
        let mut manager = ExpirationManager::new("BTC", exp);

        let wrong_exp = Utc.with_ymd_and_hms(2025, 1, 31, 8, 0, 0).unwrap();
        let contract = OptionContract::new(
            "BTC",
            wrong_exp,
            dec!(50000),
            OptionType::Call,
            dec!(1),
            dec!(0.0001),
            dec!(0.1),
        );

        manager.add_contract(contract);
    }
}
