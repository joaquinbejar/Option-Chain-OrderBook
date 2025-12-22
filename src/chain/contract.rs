//! Option contract definition and types.
//!
//! This module defines the core [`OptionContract`] structure that represents
//! a single option contract with all its properties including strike, expiration,
//! option type, and associated metadata.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Type of option contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OptionType {
    /// Call option - right to buy the underlying.
    Call,
    /// Put option - right to sell the underlying.
    Put,
}

impl fmt::Display for OptionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Call => write!(f, "C"),
            Self::Put => write!(f, "P"),
        }
    }
}

impl OptionType {
    /// Returns true if this is a call option.
    #[must_use]
    pub const fn is_call(&self) -> bool {
        matches!(self, Self::Call)
    }

    /// Returns true if this is a put option.
    #[must_use]
    pub const fn is_put(&self) -> bool {
        matches!(self, Self::Put)
    }
}

/// Represents a single option contract.
///
/// An option contract is uniquely identified by its underlying, expiration date,
/// strike price, and option type (call/put).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OptionContract {
    /// Unique symbol identifier for this contract.
    symbol: String,
    /// Underlying asset symbol (e.g., "BTC", "ETH", "SPX").
    underlying: String,
    /// Expiration date and time in UTC.
    expiration: DateTime<Utc>,
    /// Strike price of the option.
    strike: Decimal,
    /// Type of option (Call or Put).
    option_type: OptionType,
    /// Contract multiplier (e.g., 100 for equity options, 1 for crypto).
    multiplier: Decimal,
    /// Tick size for price increments.
    tick_size: Decimal,
    /// Minimum order size.
    min_order_size: Decimal,
    /// Whether the contract is currently active/tradeable.
    is_active: bool,
}

impl OptionContract {
    /// Creates a new option contract.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `expiration` - The expiration date and time in UTC
    /// * `strike` - The strike price
    /// * `option_type` - Call or Put
    /// * `multiplier` - Contract multiplier
    /// * `tick_size` - Minimum price increment
    /// * `min_order_size` - Minimum order quantity
    ///
    /// # Returns
    ///
    /// A new `OptionContract` instance with an auto-generated symbol.
    #[must_use]
    pub fn new(
        underlying: impl Into<String>,
        expiration: DateTime<Utc>,
        strike: Decimal,
        option_type: OptionType,
        multiplier: Decimal,
        tick_size: Decimal,
        min_order_size: Decimal,
    ) -> Self {
        let underlying = underlying.into();
        let symbol = Self::generate_symbol(&underlying, expiration, strike, option_type);

        Self {
            symbol,
            underlying,
            expiration,
            strike,
            option_type,
            multiplier,
            tick_size,
            min_order_size,
            is_active: true,
        }
    }

    /// Creates a new option contract with a custom symbol.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_symbol(
        symbol: impl Into<String>,
        underlying: impl Into<String>,
        expiration: DateTime<Utc>,
        strike: Decimal,
        option_type: OptionType,
        multiplier: Decimal,
        tick_size: Decimal,
        min_order_size: Decimal,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            underlying: underlying.into(),
            expiration,
            strike,
            option_type,
            multiplier,
            tick_size,
            min_order_size,
            is_active: true,
        }
    }

    /// Generates a standard symbol for the option contract.
    ///
    /// Format: `{UNDERLYING}-{YYYYMMDD}-{STRIKE}-{C|P}`
    fn generate_symbol(
        underlying: &str,
        expiration: DateTime<Utc>,
        strike: Decimal,
        option_type: OptionType,
    ) -> String {
        format!(
            "{}-{}-{}-{}",
            underlying.to_uppercase(),
            expiration.format("%Y%m%d"),
            strike,
            option_type
        )
    }

    /// Returns the contract symbol.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the expiration date and time.
    #[must_use]
    pub const fn expiration(&self) -> DateTime<Utc> {
        self.expiration
    }

    /// Returns the strike price.
    #[must_use]
    pub const fn strike(&self) -> Decimal {
        self.strike
    }

    /// Returns the option type.
    #[must_use]
    pub const fn option_type(&self) -> OptionType {
        self.option_type
    }

    /// Returns the contract multiplier.
    #[must_use]
    pub const fn multiplier(&self) -> Decimal {
        self.multiplier
    }

    /// Returns the tick size.
    #[must_use]
    pub const fn tick_size(&self) -> Decimal {
        self.tick_size
    }

    /// Returns the minimum order size.
    #[must_use]
    pub const fn min_order_size(&self) -> Decimal {
        self.min_order_size
    }

    /// Returns whether the contract is active.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.is_active
    }

    /// Sets the active status of the contract.
    pub fn set_active(&mut self, active: bool) {
        self.is_active = active;
    }

    /// Returns true if the option has expired.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expiration < Utc::now()
    }

    /// Returns the time to expiration in years.
    ///
    /// Returns 0 if the option has expired.
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
    ///
    /// Returns 0 if the option has expired.
    #[must_use]
    pub fn time_to_expiry_days(&self) -> Decimal {
        let now = Utc::now();
        if self.expiration <= now {
            return Decimal::ZERO;
        }

        let duration = self.expiration - now;
        Decimal::from(duration.num_days())
    }

    /// Validates that a price is a valid tick.
    #[must_use]
    pub fn is_valid_price(&self, price: Decimal) -> bool {
        if price <= Decimal::ZERO {
            return false;
        }
        (price % self.tick_size).is_zero()
    }

    /// Validates that a quantity is valid.
    #[must_use]
    pub fn is_valid_quantity(&self, quantity: Decimal) -> bool {
        quantity >= self.min_order_size && quantity > Decimal::ZERO
    }

    /// Rounds a price to the nearest valid tick.
    #[must_use]
    pub fn round_to_tick(&self, price: Decimal) -> Decimal {
        (price / self.tick_size).round() * self.tick_size
    }
}

impl fmt::Display for OptionContract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    fn create_test_contract() -> OptionContract {
        let expiration = Utc.with_ymd_and_hms(2024, 12, 31, 8, 0, 0).unwrap();
        OptionContract::new(
            "BTC",
            expiration,
            dec!(50000),
            OptionType::Call,
            dec!(1),
            dec!(0.0001),
            dec!(0.1),
        )
    }

    #[test]
    fn test_option_type_display() {
        assert_eq!(format!("{}", OptionType::Call), "C");
        assert_eq!(format!("{}", OptionType::Put), "P");
    }

    #[test]
    fn test_option_type_checks() {
        assert!(OptionType::Call.is_call());
        assert!(!OptionType::Call.is_put());
        assert!(OptionType::Put.is_put());
        assert!(!OptionType::Put.is_call());
    }

    #[test]
    fn test_contract_creation() {
        let contract = create_test_contract();

        assert_eq!(contract.underlying(), "BTC");
        assert_eq!(contract.strike(), dec!(50000));
        assert_eq!(contract.option_type(), OptionType::Call);
        assert!(contract.is_active());
        assert!(contract.symbol().contains("BTC"));
        assert!(contract.symbol().contains("50000"));
        assert!(contract.symbol().contains("C"));
    }

    #[test]
    fn test_contract_symbol_generation() {
        let contract = create_test_contract();
        assert_eq!(contract.symbol(), "BTC-20241231-50000-C");
    }

    #[test]
    fn test_valid_price() {
        let contract = create_test_contract();

        assert!(contract.is_valid_price(dec!(0.0001)));
        assert!(contract.is_valid_price(dec!(0.0002)));
        assert!(contract.is_valid_price(dec!(1.0)));
        assert!(!contract.is_valid_price(dec!(0)));
        assert!(!contract.is_valid_price(dec!(-1)));
    }

    #[test]
    fn test_valid_quantity() {
        let contract = create_test_contract();

        assert!(contract.is_valid_quantity(dec!(0.1)));
        assert!(contract.is_valid_quantity(dec!(1.0)));
        assert!(!contract.is_valid_quantity(dec!(0.05)));
        assert!(!contract.is_valid_quantity(dec!(0)));
    }

    #[test]
    fn test_round_to_tick() {
        let contract = create_test_contract();

        assert_eq!(contract.round_to_tick(dec!(0.00015)), dec!(0.0002));
        assert_eq!(contract.round_to_tick(dec!(0.00014)), dec!(0.0001));
        // 1.00005 / 0.0001 = 10000.5, rounds to 10000, * 0.0001 = 1.0000
        assert_eq!(contract.round_to_tick(dec!(1.00005)), dec!(1.0000));
        // 1.00006 / 0.0001 = 10000.6, rounds to 10001, * 0.0001 = 1.0001
        assert_eq!(contract.round_to_tick(dec!(1.00006)), dec!(1.0001));
    }

    #[test]
    fn test_contract_serialization() {
        let contract = create_test_contract();
        let json = serde_json::to_string(&contract).unwrap();
        let deserialized: OptionContract = serde_json::from_str(&json).unwrap();

        assert_eq!(contract.symbol(), deserialized.symbol());
        assert_eq!(contract.strike(), deserialized.strike());
        assert_eq!(contract.option_type(), deserialized.option_type());
    }
}
