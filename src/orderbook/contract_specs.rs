//! Contract specifications module.
//!
//! This module provides the [`ContractSpecs`] struct for defining instrument-level
//! specifications (tick size, lot size, contract size, settlement currency, exercise
//! style) and attaching them to the option chain hierarchy at the
//! [`UnderlyingOrderBook`](super::underlying::UnderlyingOrderBook) level.
//!
//! It also provides [`SharedContractSpecs`], a thread-safe wrapper used internally
//! by hierarchy managers to propagate specs to newly created children.

use super::validation::ValidationConfig;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

/// Exercise style of the option contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ExerciseStyle {
    /// European-style option: can only be exercised at expiration.
    European = 0,
    /// American-style option: can be exercised at any time before expiration.
    American = 1,
}

impl std::fmt::Display for ExerciseStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::European => write!(f, "European"),
            Self::American => write!(f, "American"),
        }
    }
}

/// Settlement type of the option contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum SettlementType {
    /// Cash settlement: difference between strike and spot paid in currency.
    Cash = 0,
    /// Physical settlement: actual delivery of the underlying asset.
    Physical = 1,
}

impl std::fmt::Display for SettlementType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cash => write!(f, "Cash"),
            Self::Physical => write!(f, "Physical"),
        }
    }
}

/// Instrument-level contract specifications.
///
/// Defines the trading rules and characteristics for all options under a single
/// underlying asset. Attached at the
/// [`UnderlyingOrderBook`](super::underlying::UnderlyingOrderBook) level and
/// propagated down the hierarchy.
///
/// When set on an `UnderlyingOrderBook`, a [`ValidationConfig`] is automatically
/// derived from the tick/lot/min/max fields and applied to all future order books.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::ContractSpecs;
/// use option_chain_orderbook::orderbook::ExerciseStyle;
/// use option_chain_orderbook::orderbook::SettlementType;
///
/// let specs = ContractSpecs::builder()
///     .tick_size(100)
///     .lot_size(1)
///     .contract_size(1)
///     .min_order_size(1)
///     .max_order_size(10_000)
///     .settlement(SettlementType::Cash)
///     .exercise_style(ExerciseStyle::European)
///     .settlement_currency("USDC")
///     .build()
///     .expect("valid contract specs");
///
/// assert_eq!(specs.tick_size(), 100);
/// assert_eq!(specs.exercise_style(), ExerciseStyle::European);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractSpecs {
    /// Price tick size in smallest price units (e.g., 100 = 0.01 USDC with 4 decimals).
    tick_size: u128,
    /// Quantity lot size (minimum order quantity increment).
    lot_size: u64,
    /// Contract multiplier in smallest units (e.g., 1 for standard options).
    contract_size: u64,
    /// Minimum order size in lots.
    min_order_size: u64,
    /// Maximum order size in lots.
    max_order_size: u64,
    /// Settlement type (Cash or Physical).
    settlement: SettlementType,
    /// Exercise style (European or American).
    exercise_style: ExerciseStyle,
    /// Settlement currency symbol (e.g., "USDC").
    settlement_currency: String,
}

impl Default for ContractSpecs {
    /// Creates permissive default specs that impose no validation constraints.
    fn default() -> Self {
        Self {
            tick_size: 1,
            lot_size: 1,
            contract_size: 1,
            min_order_size: 1,
            max_order_size: u64::MAX,
            settlement: SettlementType::Cash,
            exercise_style: ExerciseStyle::European,
            settlement_currency: "USDC".to_string(),
        }
    }
}

impl ContractSpecs {
    /// Creates a new [`ContractSpecsBuilder`] for constructing specs.
    pub fn builder() -> ContractSpecsBuilder {
        ContractSpecsBuilder::default()
    }

    /// Returns the price tick size in smallest price units.
    #[must_use]
    #[inline]
    pub const fn tick_size(&self) -> u128 {
        self.tick_size
    }

    /// Returns the quantity lot size (minimum order quantity increment).
    #[must_use]
    #[inline]
    pub const fn lot_size(&self) -> u64 {
        self.lot_size
    }

    /// Returns the contract multiplier in smallest units.
    #[must_use]
    #[inline]
    pub const fn contract_size(&self) -> u64 {
        self.contract_size
    }

    /// Returns the minimum order size in lots.
    #[must_use]
    #[inline]
    pub const fn min_order_size(&self) -> u64 {
        self.min_order_size
    }

    /// Returns the maximum order size in lots.
    #[must_use]
    #[inline]
    pub const fn max_order_size(&self) -> u64 {
        self.max_order_size
    }

    /// Returns the settlement type.
    #[must_use]
    #[inline]
    pub const fn settlement(&self) -> SettlementType {
        self.settlement
    }

    /// Returns the exercise style.
    #[must_use]
    #[inline]
    pub const fn exercise_style(&self) -> ExerciseStyle {
        self.exercise_style
    }

    /// Returns the settlement currency symbol.
    #[must_use]
    #[inline]
    pub fn settlement_currency(&self) -> &str {
        &self.settlement_currency
    }

    /// Derives a [`ValidationConfig`] from this contract's tick/lot/min/max fields.
    ///
    /// This is used internally to auto-configure order validation when specs are
    /// attached to the hierarchy.
    #[must_use]
    pub fn to_validation_config(&self) -> ValidationConfig {
        ValidationConfig::new()
            .with_tick_size(self.tick_size)
            .with_lot_size(self.lot_size)
            .with_min_order_size(self.min_order_size)
            .with_max_order_size(self.max_order_size)
    }

    /// Validates these specifications, rejecting structurally-broken values.
    ///
    /// A zero tick / lot / contract / minimum size is rejected: the upstream
    /// matching engine treats a zero tick / lot / minimum as "validation
    /// disabled" (silently discarding an intended constraint), and a zero
    /// contract size has no economic meaning. An inverted `[min, max]` window
    /// is rejected because it would reject every order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ConfigurationError`] if:
    /// - `tick_size` is zero
    /// - `lot_size` is zero
    /// - `contract_size` is zero
    /// - `min_order_size` is zero
    /// - `max_order_size < min_order_size`
    pub fn validate(&self) -> Result<()> {
        if self.tick_size == 0 {
            return Err(Error::configuration(format!(
                "tick_size must be at least 1, got {}",
                self.tick_size
            )));
        }
        if self.lot_size == 0 {
            return Err(Error::configuration(format!(
                "lot_size must be at least 1, got {}",
                self.lot_size
            )));
        }
        if self.contract_size == 0 {
            return Err(Error::configuration(format!(
                "contract_size must be at least 1, got {}",
                self.contract_size
            )));
        }
        if self.min_order_size == 0 {
            return Err(Error::configuration(format!(
                "min_order_size must be at least 1, got {}",
                self.min_order_size
            )));
        }
        if self.max_order_size < self.min_order_size {
            return Err(Error::configuration(format!(
                "max_order_size ({}) must be greater than or equal to min_order_size ({})",
                self.max_order_size, self.min_order_size
            )));
        }
        Ok(())
    }
}

impl std::fmt::Display for ContractSpecs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ContractSpecs(tick={}, lot={}, contract={}, min={}, max={}, {}, {}, {})",
            self.tick_size,
            self.lot_size,
            self.contract_size,
            self.min_order_size,
            self.max_order_size,
            self.settlement,
            self.exercise_style,
            self.settlement_currency,
        )
    }
}

/// Builder for [`ContractSpecs`].
///
/// Starts from [`ContractSpecs::default()`] values and allows overriding
/// individual fields.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::{ContractSpecs, SettlementType};
///
/// let specs = ContractSpecs::builder()
///     .tick_size(100)
///     .settlement(SettlementType::Physical)
///     .build()
///     .expect("valid contract specs");
///
/// assert_eq!(specs.tick_size(), 100);
/// assert_eq!(specs.settlement(), SettlementType::Physical);
/// ```
#[derive(Debug, Clone)]
#[must_use = "builders do nothing unless .build() is called"]
#[derive(Default)]
pub struct ContractSpecsBuilder {
    /// The specs being constructed.
    inner: ContractSpecs,
}

impl ContractSpecsBuilder {
    /// Sets the price tick size in smallest price units.
    #[inline]
    pub const fn tick_size(mut self, tick_size: u128) -> Self {
        self.inner.tick_size = tick_size;
        self
    }

    /// Sets the quantity lot size (minimum order quantity increment).
    #[inline]
    pub const fn lot_size(mut self, lot_size: u64) -> Self {
        self.inner.lot_size = lot_size;
        self
    }

    /// Sets the contract multiplier in smallest units.
    #[inline]
    pub const fn contract_size(mut self, contract_size: u64) -> Self {
        self.inner.contract_size = contract_size;
        self
    }

    /// Sets the minimum order size in lots.
    #[inline]
    pub const fn min_order_size(mut self, min_order_size: u64) -> Self {
        self.inner.min_order_size = min_order_size;
        self
    }

    /// Sets the maximum order size in lots.
    #[inline]
    pub const fn max_order_size(mut self, max_order_size: u64) -> Self {
        self.inner.max_order_size = max_order_size;
        self
    }

    /// Sets the settlement type.
    #[inline]
    pub const fn settlement(mut self, settlement: SettlementType) -> Self {
        self.inner.settlement = settlement;
        self
    }

    /// Sets the exercise style.
    #[inline]
    pub const fn exercise_style(mut self, exercise_style: ExerciseStyle) -> Self {
        self.inner.exercise_style = exercise_style;
        self
    }

    /// Sets the settlement currency symbol.
    #[inline]
    pub fn settlement_currency(mut self, currency: impl Into<String>) -> Self {
        self.inner.settlement_currency = currency.into();
        self
    }

    /// Consumes the builder and returns a validated [`ContractSpecs`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ConfigurationError`]
    /// if the assembled specs are structurally invalid (zero tick / lot /
    /// contract / minimum size, or an inverted `[min, max]` order-size window);
    /// see [`ContractSpecs::validate`].
    pub fn build(self) -> Result<ContractSpecs> {
        self.inner.validate()?;
        Ok(self.inner)
    }
}

/// Thread-safe shared contract specifications.
///
/// Wraps a [`ContractSpecs`] in a [`RwLock`] so that hierarchy managers can
/// store and propagate specs to newly created children without requiring
/// `&mut self`.
pub(crate) struct SharedContractSpecs {
    /// The inner contract specs, protected by a read-write lock.
    inner: RwLock<Option<ContractSpecs>>,
}

impl SharedContractSpecs {
    /// Creates a new empty shared contract specs.
    #[inline]
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Sets the contract specs.
    ///
    /// Recovers from a poisoned lock to ensure the specs are always written.
    pub(crate) fn set(&self, specs: ContractSpecs) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(specs);
    }

    /// Returns a clone of the current contract specs, if any.
    ///
    /// Recovers from a poisoned lock to avoid silently dropping stored specs.
    #[must_use]
    pub(crate) fn get(&self) -> Option<ContractSpecs> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    }
}

impl Default for SharedContractSpecs {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SharedContractSpecs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let specs = self.get();
        f.debug_struct("SharedContractSpecs")
            .field("inner", &specs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_specs_are_permissive() {
        let specs = ContractSpecs::default();
        assert_eq!(specs.tick_size(), 1);
        assert_eq!(specs.lot_size(), 1);
        assert_eq!(specs.contract_size(), 1);
        assert_eq!(specs.min_order_size(), 1);
        assert_eq!(specs.max_order_size(), u64::MAX);
        assert_eq!(specs.settlement(), SettlementType::Cash);
        assert_eq!(specs.exercise_style(), ExerciseStyle::European);
        assert_eq!(specs.settlement_currency(), "USDC");
    }

    #[test]
    fn test_builder_overrides() {
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .contract_size(5)
            .min_order_size(2)
            .max_order_size(50_000)
            .settlement(SettlementType::Physical)
            .exercise_style(ExerciseStyle::American)
            .settlement_currency("BTC")
            .build()
            .expect("valid specs");

        assert_eq!(specs.tick_size(), 100);
        assert_eq!(specs.lot_size(), 10);
        assert_eq!(specs.contract_size(), 5);
        assert_eq!(specs.min_order_size(), 2);
        assert_eq!(specs.max_order_size(), 50_000);
        assert_eq!(specs.settlement(), SettlementType::Physical);
        assert_eq!(specs.exercise_style(), ExerciseStyle::American);
        assert_eq!(specs.settlement_currency(), "BTC");
    }

    #[test]
    fn test_builder_partial_override() {
        let specs = ContractSpecs::builder()
            .tick_size(500)
            .build()
            .expect("valid specs");

        assert_eq!(specs.tick_size(), 500);
        // Rest should be defaults
        assert_eq!(specs.lot_size(), 1);
        assert_eq!(specs.contract_size(), 1);
        assert_eq!(specs.settlement(), SettlementType::Cash);
        assert_eq!(specs.exercise_style(), ExerciseStyle::European);
    }

    #[test]
    fn test_to_validation_config() {
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .min_order_size(5)
            .max_order_size(1000)
            .build()
            .expect("valid specs");

        let config = specs.to_validation_config();
        assert_eq!(config.tick_size(), Some(100));
        assert_eq!(config.lot_size(), Some(10));
        assert_eq!(config.min_order_size(), Some(5));
        assert_eq!(config.max_order_size(), Some(1000));
    }

    #[test]
    fn test_default_to_validation_config() {
        let specs = ContractSpecs::default();
        let config = specs.to_validation_config();

        // Default specs produce a ValidationConfig that accepts everything
        assert_eq!(config.tick_size(), Some(1));
        assert_eq!(config.lot_size(), Some(1));
        assert_eq!(config.min_order_size(), Some(1));
        assert_eq!(config.max_order_size(), Some(u64::MAX));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .contract_size(5)
            .min_order_size(2)
            .max_order_size(50_000)
            .settlement(SettlementType::Physical)
            .exercise_style(ExerciseStyle::American)
            .settlement_currency("ETH")
            .build()
            .expect("valid specs");

        let json = match serde_json::to_string(&specs) {
            Ok(j) => j,
            Err(err) => panic!("serialization failed: {}", err),
        };
        let deserialized: ContractSpecs = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(err) => panic!("deserialization failed: {}", err),
        };
        assert_eq!(specs, deserialized);
    }

    #[test]
    fn test_default_serialization_roundtrip() {
        let specs = ContractSpecs::default();
        let json = match serde_json::to_string(&specs) {
            Ok(j) => j,
            Err(err) => panic!("serialization failed: {}", err),
        };
        let deserialized: ContractSpecs = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(err) => panic!("deserialization failed: {}", err),
        };
        assert_eq!(specs, deserialized);
    }

    #[test]
    fn test_display_contract_specs() {
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(10)
            .settlement(SettlementType::Cash)
            .exercise_style(ExerciseStyle::European)
            .settlement_currency("USDC")
            .build()
            .expect("valid specs");

        let display = format!("{specs}");
        assert!(display.contains("tick=100"));
        assert!(display.contains("lot=10"));
        assert!(display.contains("Cash"));
        assert!(display.contains("European"));
        assert!(display.contains("USDC"));
    }

    #[test]
    fn test_display_exercise_style() {
        assert_eq!(format!("{}", ExerciseStyle::European), "European");
        assert_eq!(format!("{}", ExerciseStyle::American), "American");
    }

    #[test]
    fn test_display_settlement_type() {
        assert_eq!(format!("{}", SettlementType::Cash), "Cash");
        assert_eq!(format!("{}", SettlementType::Physical), "Physical");
    }

    #[test]
    fn test_exercise_style_equality() {
        assert_eq!(ExerciseStyle::European, ExerciseStyle::European);
        assert_ne!(ExerciseStyle::European, ExerciseStyle::American);
    }

    #[test]
    fn test_settlement_type_equality() {
        assert_eq!(SettlementType::Cash, SettlementType::Cash);
        assert_ne!(SettlementType::Cash, SettlementType::Physical);
    }

    #[test]
    fn test_exercise_style_serialization() {
        let style = ExerciseStyle::American;
        let json = match serde_json::to_string(&style) {
            Ok(j) => j,
            Err(err) => panic!("serialization failed: {}", err),
        };
        let deserialized: ExerciseStyle = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(err) => panic!("deserialization failed: {}", err),
        };
        assert_eq!(style, deserialized);
    }

    #[test]
    fn test_settlement_type_serialization() {
        let stype = SettlementType::Physical;
        let json = match serde_json::to_string(&stype) {
            Ok(j) => j,
            Err(err) => panic!("serialization failed: {}", err),
        };
        let deserialized: SettlementType = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(err) => panic!("deserialization failed: {}", err),
        };
        assert_eq!(stype, deserialized);
    }

    #[test]
    fn test_contract_specs_clone() {
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .settlement_currency("BTC")
            .build()
            .expect("valid specs");
        let cloned = specs.clone();
        assert_eq!(specs, cloned);
    }

    #[test]
    fn test_shared_contract_specs_default() {
        let shared = SharedContractSpecs::new();
        assert!(shared.get().is_none());
    }

    #[test]
    fn test_shared_contract_specs_set_get() {
        let shared = SharedContractSpecs::new();
        let specs = ContractSpecs::builder()
            .tick_size(100)
            .build()
            .expect("valid specs");
        shared.set(specs.clone());
        assert_eq!(shared.get(), Some(specs));
    }

    #[test]
    fn test_shared_contract_specs_overwrite() {
        let shared = SharedContractSpecs::new();
        shared.set(
            ContractSpecs::builder()
                .tick_size(100)
                .build()
                .expect("valid specs"),
        );
        shared.set(
            ContractSpecs::builder()
                .tick_size(200)
                .build()
                .expect("valid specs"),
        );
        assert_eq!(shared.get().map(|s| s.tick_size()), Some(200));
    }

    #[test]
    fn test_shared_contract_specs_debug() {
        let shared = SharedContractSpecs::new();
        let debug = format!("{shared:?}");
        assert!(debug.contains("SharedContractSpecs"));
    }

    #[test]
    fn test_builder_debug() {
        let builder = ContractSpecs::builder().tick_size(100);
        let debug = format!("{builder:?}");
        assert!(debug.contains("ContractSpecsBuilder"));
    }

    // ========== ContractSpecsBuilder validation tests ==========

    #[test]
    fn test_contract_specs_builder_default_builds_ok() {
        let result = ContractSpecs::builder().build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_contract_specs_builder_valid_builds_ok() {
        let result = ContractSpecs::builder()
            .tick_size(100)
            .lot_size(1)
            .contract_size(1)
            .min_order_size(1)
            .max_order_size(10_000)
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_contract_specs_builder_zero_tick_rejected() {
        let result = ContractSpecs::builder().tick_size(0).build();
        let err = match result {
            Ok(_) => panic!("expected zero tick_size to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("tick_size"));
        assert!(err.to_string().contains('0'));
    }

    #[test]
    fn test_contract_specs_builder_zero_lot_rejected() {
        let result = ContractSpecs::builder().lot_size(0).build();
        let err = match result {
            Ok(_) => panic!("expected zero lot_size to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("lot_size"));
    }

    #[test]
    fn test_contract_specs_builder_zero_contract_size_rejected() {
        let result = ContractSpecs::builder().contract_size(0).build();
        let err = match result {
            Ok(_) => panic!("expected zero contract_size to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("contract_size"));
    }

    #[test]
    fn test_contract_specs_builder_zero_min_order_size_rejected() {
        let result = ContractSpecs::builder().min_order_size(0).build();
        let err = match result {
            Ok(_) => panic!("expected zero min_order_size to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("min_order_size"));
    }

    #[test]
    fn test_contract_specs_builder_inverted_order_size_window_rejected() {
        let result = ContractSpecs::builder()
            .min_order_size(100)
            .max_order_size(10)
            .build();
        let err = match result {
            Ok(_) => panic!("expected max < min to be rejected"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("max_order_size"));
        assert!(msg.contains("10"));
        assert!(msg.contains("100"));
    }

    #[test]
    fn test_contract_specs_validate_default_ok() {
        let specs = ContractSpecs::default();
        assert!(specs.validate().is_ok());
    }
}
