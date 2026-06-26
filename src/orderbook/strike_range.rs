//! Strike range configuration module.
//!
//! This module provides the [`StrikeRangeConfig`] struct and [`ExpiryType`] enum
//! for configuring automatic strike generation ranges per underlying and per
//! expiration type. These configurations determine:
//! - The ATM ± percentage range for strike generation
//! - The interval between consecutive strikes
//! - Minimum and maximum number of strikes to generate
//!
//! Configurations are stored at the
//! [`UnderlyingOrderBook`](super::underlying::UnderlyingOrderBook) level and
//! keyed by [`ExpiryType`].

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// Expiration type classification for strike range configuration.
///
/// Different expiration types typically have different strike ranges:
/// - Daily options have tighter ranges (less time for large moves)
/// - Quarterly options have wider ranges (more time value)
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::ExpiryType;
///
/// let expiry = ExpiryType::Weekly;
/// let config = expiry.default_config();
/// assert!((config.range_pct() - 0.10).abs() < f64::EPSILON);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum ExpiryType {
    /// Daily expiration (0-1 DTE).
    Daily = 0,
    /// Weekly expiration (1-7 DTE).
    Weekly = 1,
    /// Monthly expiration (typically third Friday).
    #[default]
    Monthly = 2,
    /// Quarterly expiration (end of quarter).
    Quarterly = 3,
}

impl ExpiryType {
    /// Returns the default strike range configuration for this expiry type.
    ///
    /// Default ranges follow architecture specification:
    /// - Daily: ±5%
    /// - Weekly: ±10%
    /// - Monthly: ±20%
    /// - Quarterly: ±30%
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::ExpiryType;
    ///
    /// let config = ExpiryType::Monthly.default_config();
    /// assert!((config.range_pct() - 0.20).abs() < f64::EPSILON);
    /// ```
    #[must_use]
    pub fn default_config(self) -> StrikeRangeConfig {
        match self {
            Self::Daily => StrikeRangeConfig {
                range_pct: 0.05,
                strike_interval: 1000,
                min_strikes: 3,
                max_strikes: 50,
            },
            Self::Weekly => StrikeRangeConfig {
                range_pct: 0.10,
                strike_interval: 1000,
                min_strikes: 5,
                max_strikes: 100,
            },
            Self::Monthly => StrikeRangeConfig {
                range_pct: 0.20,
                strike_interval: 1000,
                min_strikes: 5,
                max_strikes: 100,
            },
            Self::Quarterly => StrikeRangeConfig {
                range_pct: 0.30,
                strike_interval: 1000,
                min_strikes: 5,
                max_strikes: 150,
            },
        }
    }

    /// Returns all expiry type variants.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Daily, Self::Weekly, Self::Monthly, Self::Quarterly]
    }
}

impl std::fmt::Display for ExpiryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Daily => write!(f, "Daily"),
            Self::Weekly => write!(f, "Weekly"),
            Self::Monthly => write!(f, "Monthly"),
            Self::Quarterly => write!(f, "Quarterly"),
        }
    }
}

/// Strike range configuration for automatic strike generation.
///
/// Defines the parameters for generating strikes around the ATM price
/// for a specific underlying and expiry type combination.
///
/// # Fields
///
/// - `range_pct`: Percentage range around ATM (e.g., 0.10 = ±10%)
/// - `strike_interval`: Price units between consecutive strikes (e.g., 1000 for BTC)
/// - `min_strikes`: Minimum number of strikes to generate
/// - `max_strikes`: Maximum number of strikes to generate
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::StrikeRangeConfig;
///
/// let config = StrikeRangeConfig::builder()
///     .range_pct(0.15)
///     .strike_interval(500)
///     .min_strikes(10)
///     .max_strikes(200)
///     .build()
///     .expect("valid config");
///
/// assert!((config.range_pct() - 0.15).abs() < f64::EPSILON);
/// assert_eq!(config.strike_interval(), 500);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrikeRangeConfig {
    /// Percentage range around ATM (e.g., 0.10 = ±10%).
    range_pct: f64,
    /// Strike interval in price units (e.g., 1000 for BTC = $1000).
    strike_interval: u64,
    /// Minimum number of strikes to generate.
    min_strikes: usize,
    /// Maximum number of strikes to generate.
    max_strikes: usize,
}

impl Default for StrikeRangeConfig {
    /// Creates a default configuration suitable for most use cases.
    ///
    /// Defaults:
    /// - range_pct: 0.20 (±20%)
    /// - strike_interval: 1000
    /// - min_strikes: 5
    /// - max_strikes: 100
    fn default() -> Self {
        Self {
            range_pct: 0.20,
            strike_interval: 1000,
            min_strikes: 5,
            max_strikes: 100,
        }
    }
}

impl StrikeRangeConfig {
    /// Creates a new [`StrikeRangeConfigBuilder`] for constructing a config.
    pub fn builder() -> StrikeRangeConfigBuilder {
        StrikeRangeConfigBuilder::default()
    }

    /// Returns the percentage range around ATM.
    ///
    /// Value of 0.10 means ±10% around ATM.
    #[must_use]
    #[inline]
    pub const fn range_pct(&self) -> f64 {
        self.range_pct
    }

    /// Returns the strike interval in price units.
    ///
    /// For example, 1000 for BTC means $1000 between consecutive strikes.
    #[must_use]
    #[inline]
    pub const fn strike_interval(&self) -> u64 {
        self.strike_interval
    }

    /// Returns the minimum number of strikes to generate.
    #[must_use]
    #[inline]
    pub const fn min_strikes(&self) -> usize {
        self.min_strikes
    }

    /// Returns the maximum number of strikes to generate.
    #[must_use]
    #[inline]
    pub const fn max_strikes(&self) -> usize {
        self.max_strikes
    }

    /// Validates the configuration.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - `range_pct` is not finite, negative, or greater than 1.0
    /// - `strike_interval` is zero
    /// - `min_strikes` is greater than `max_strikes`
    /// - `min_strikes` is zero
    pub fn validate(&self) -> Result<()> {
        if !self.range_pct.is_finite() {
            return Err(Error::configuration("range_pct must be finite"));
        }
        if self.range_pct <= 0.0 {
            return Err(Error::configuration("range_pct must be positive"));
        }
        if self.range_pct > 1.0 {
            return Err(Error::configuration("range_pct must not exceed 1.0 (100%)"));
        }
        if self.strike_interval == 0 {
            return Err(Error::configuration("strike_interval must be positive"));
        }
        if self.min_strikes == 0 {
            return Err(Error::configuration("min_strikes must be at least 1"));
        }
        if self.min_strikes > self.max_strikes {
            return Err(Error::configuration(
                "min_strikes must not exceed max_strikes",
            ));
        }
        Ok(())
    }
}

impl std::fmt::Display for StrikeRangeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "StrikeRangeConfig(±{:.0}%, interval={}, strikes={}-{})",
            self.range_pct * 100.0,
            self.strike_interval,
            self.min_strikes,
            self.max_strikes
        )
    }
}

/// Builder for [`StrikeRangeConfig`].
///
/// Starts from [`StrikeRangeConfig::default()`] values and allows overriding
/// individual fields. Call [`build()`](Self::build) to validate and construct
/// the final configuration.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::StrikeRangeConfig;
///
/// let config = StrikeRangeConfig::builder()
///     .range_pct(0.25)
///     .strike_interval(50)  // ETH-style $50 intervals
///     .build()
///     .expect("valid config");
///
/// assert_eq!(config.strike_interval(), 50);
/// ```
#[derive(Debug, Clone)]
#[must_use = "builders do nothing unless .build() is called"]
#[derive(Default)]
pub struct StrikeRangeConfigBuilder {
    /// The config being constructed.
    inner: StrikeRangeConfig,
}

impl StrikeRangeConfigBuilder {
    /// Sets the percentage range around ATM.
    ///
    /// # Arguments
    ///
    /// * `pct` - Range percentage (e.g., 0.10 for ±10%)
    #[inline]
    pub const fn range_pct(mut self, pct: f64) -> Self {
        self.inner.range_pct = pct;
        self
    }

    /// Sets the strike interval in price units.
    ///
    /// # Arguments
    ///
    /// * `interval` - Price units between strikes (e.g., 1000 for BTC)
    #[inline]
    pub const fn strike_interval(mut self, interval: u64) -> Self {
        self.inner.strike_interval = interval;
        self
    }

    /// Sets the minimum number of strikes to generate.
    ///
    /// # Arguments
    ///
    /// * `min` - Minimum strike count
    #[inline]
    pub const fn min_strikes(mut self, min: usize) -> Self {
        self.inner.min_strikes = min;
        self
    }

    /// Sets the maximum number of strikes to generate.
    ///
    /// # Arguments
    ///
    /// * `max` - Maximum strike count
    #[inline]
    pub const fn max_strikes(mut self, max: usize) -> Self {
        self.inner.max_strikes = max;
        self
    }

    /// Consumes the builder and returns a validated [`StrikeRangeConfig`].
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if validation fails.
    pub fn build(self) -> Result<StrikeRangeConfig> {
        self.inner.validate()?;
        Ok(self.inner)
    }

    /// Consumes the builder and returns the config without validation.
    ///
    /// Use this only when you are certain the values are valid.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn build_unchecked(self) -> StrikeRangeConfig {
        self.inner
    }
}

/// Thread-safe shared strike range configurations.
///
/// Wraps a [`HashMap<ExpiryType, StrikeRangeConfig>`] in a [`RwLock`] so that
/// hierarchy managers can store and update configs without requiring `&mut self`.
///
/// Unlike the single-value [`Shared`](super::shared::Shared)`<T>` holder, this is
/// a keyed map with map-specific accessors (`set(key, value)` insert, keyed
/// `get`, `remove`, `get_all`, `len`, `is_empty`), so it stays a bespoke wrapper.
pub(crate) struct SharedStrikeRangeConfigs {
    /// The inner configs, protected by a read-write lock.
    inner: RwLock<HashMap<ExpiryType, StrikeRangeConfig>>,
}

impl SharedStrikeRangeConfigs {
    /// Creates a new empty shared strike range configs.
    #[inline]
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Sets the strike range config for a specific expiry type.
    ///
    /// Recovers from a poisoned lock to ensure the config is always written.
    pub(crate) fn set(&self, expiry_type: ExpiryType, config: StrikeRangeConfig) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(expiry_type, config);
    }

    /// Returns the strike range config for a specific expiry type, if any.
    ///
    /// Recovers from a poisoned lock to avoid silently dropping stored configs.
    #[must_use]
    pub(crate) fn get(&self, expiry_type: ExpiryType) -> Option<StrikeRangeConfig> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.get(&expiry_type).cloned()
    }

    /// Returns a clone of all strike range configs.
    ///
    /// Recovers from a poisoned lock to avoid silently dropping stored configs.
    #[must_use]
    pub(crate) fn get_all(&self) -> HashMap<ExpiryType, StrikeRangeConfig> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    }

    /// Removes the strike range config for a specific expiry type.
    ///
    /// Returns the removed config if it existed.
    pub(crate) fn remove(&self, expiry_type: ExpiryType) -> Option<StrikeRangeConfig> {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(&expiry_type)
    }

    /// Clears all strike range configs.
    pub(crate) fn clear(&self) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clear();
    }

    /// Returns the number of configured expiry types.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.len()
    }

    /// Returns true if no configs are set.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SharedStrikeRangeConfigs {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SharedStrikeRangeConfigs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let configs = self.get_all();
        f.debug_struct("SharedStrikeRangeConfigs")
            .field("inner", &configs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== ExpiryType tests ==========

    #[test]
    fn test_expiry_type_display() {
        assert_eq!(format!("{}", ExpiryType::Daily), "Daily");
        assert_eq!(format!("{}", ExpiryType::Weekly), "Weekly");
        assert_eq!(format!("{}", ExpiryType::Monthly), "Monthly");
        assert_eq!(format!("{}", ExpiryType::Quarterly), "Quarterly");
    }

    #[test]
    fn test_expiry_type_default() {
        assert_eq!(ExpiryType::default(), ExpiryType::Monthly);
    }

    #[test]
    fn test_expiry_type_all() {
        let all = ExpiryType::all();
        assert_eq!(all.len(), 4);
        assert!(all.contains(&ExpiryType::Daily));
        assert!(all.contains(&ExpiryType::Weekly));
        assert!(all.contains(&ExpiryType::Monthly));
        assert!(all.contains(&ExpiryType::Quarterly));
    }

    #[test]
    fn test_expiry_type_default_config_daily() {
        let config = ExpiryType::Daily.default_config();
        assert!((config.range_pct() - 0.05).abs() < f64::EPSILON);
        assert_eq!(config.min_strikes(), 3);
    }

    #[test]
    fn test_expiry_type_default_config_weekly() {
        let config = ExpiryType::Weekly.default_config();
        assert!((config.range_pct() - 0.10).abs() < f64::EPSILON);
    }

    #[test]
    fn test_expiry_type_default_config_monthly() {
        let config = ExpiryType::Monthly.default_config();
        assert!((config.range_pct() - 0.20).abs() < f64::EPSILON);
    }

    #[test]
    fn test_expiry_type_default_config_quarterly() {
        let config = ExpiryType::Quarterly.default_config();
        assert!((config.range_pct() - 0.30).abs() < f64::EPSILON);
        assert_eq!(config.max_strikes(), 150);
    }

    #[test]
    fn test_expiry_type_serialization_roundtrip() {
        let expiry = ExpiryType::Weekly;
        let json = match serde_json::to_string(&expiry) {
            Ok(j) => j,
            Err(err) => panic!("serialization failed: {}", err),
        };
        let deserialized: ExpiryType = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(err) => panic!("deserialization failed: {}", err),
        };
        assert_eq!(expiry, deserialized);
    }

    #[test]
    fn test_expiry_type_equality() {
        assert_eq!(ExpiryType::Daily, ExpiryType::Daily);
        assert_ne!(ExpiryType::Daily, ExpiryType::Weekly);
    }

    #[test]
    fn test_expiry_type_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ExpiryType::Daily);
        set.insert(ExpiryType::Weekly);
        assert!(set.contains(&ExpiryType::Daily));
        assert!(!set.contains(&ExpiryType::Monthly));
    }

    // ========== StrikeRangeConfig tests ==========

    #[test]
    fn test_strike_range_config_default() {
        let config = StrikeRangeConfig::default();
        assert!((config.range_pct() - 0.20).abs() < f64::EPSILON);
        assert_eq!(config.strike_interval(), 1000);
        assert_eq!(config.min_strikes(), 5);
        assert_eq!(config.max_strikes(), 100);
    }

    #[test]
    fn test_strike_range_config_builder_full() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.15)
            .strike_interval(500)
            .min_strikes(10)
            .max_strikes(200)
            .build();

        assert!(config.is_ok());
        let config = match config {
            Ok(c) => c,
            Err(err) => panic!("build failed: {}", err),
        };
        assert!((config.range_pct() - 0.15).abs() < f64::EPSILON);
        assert_eq!(config.strike_interval(), 500);
        assert_eq!(config.min_strikes(), 10);
        assert_eq!(config.max_strikes(), 200);
    }

    #[test]
    fn test_strike_range_config_builder_partial() {
        let config = StrikeRangeConfig::builder().strike_interval(50).build();

        assert!(config.is_ok());
        let config = match config {
            Ok(c) => c,
            Err(err) => panic!("build failed: {}", err),
        };
        assert_eq!(config.strike_interval(), 50);
        // Rest should be defaults
        assert!((config.range_pct() - 0.20).abs() < f64::EPSILON);
        assert_eq!(config.min_strikes(), 5);
        assert_eq!(config.max_strikes(), 100);
    }

    #[test]
    fn test_strike_range_config_validation_nan_range() {
        let result = StrikeRangeConfig::builder().range_pct(f64::NAN).build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("finite"));
    }

    #[test]
    fn test_strike_range_config_validation_infinite_range() {
        let result = StrikeRangeConfig::builder()
            .range_pct(f64::INFINITY)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_strike_range_config_validation_negative_range() {
        let result = StrikeRangeConfig::builder().range_pct(-0.10).build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("positive"));
    }

    #[test]
    fn test_strike_range_config_validation_range_exceeds_one() {
        let result = StrikeRangeConfig::builder().range_pct(1.5).build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("exceed"));
    }

    #[test]
    fn test_strike_range_config_validation_zero_interval() {
        let result = StrikeRangeConfig::builder().strike_interval(0).build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("interval"));
    }

    #[test]
    fn test_strike_range_config_validation_zero_min_strikes() {
        let result = StrikeRangeConfig::builder().min_strikes(0).build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("min_strikes"));
    }

    #[test]
    fn test_strike_range_config_validation_min_exceeds_max() {
        let result = StrikeRangeConfig::builder()
            .min_strikes(100)
            .max_strikes(10)
            .build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("min_strikes"));
    }

    #[test]
    fn test_strike_range_config_build_unchecked() {
        // This should not validate - use with caution
        let config = StrikeRangeConfig::builder()
            .range_pct(-0.50) // Invalid, but unchecked
            .build_unchecked();
        assert!(config.range_pct() < 0.0);
    }

    #[test]
    fn test_strike_range_config_display() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.15)
            .strike_interval(1000)
            .min_strikes(5)
            .max_strikes(100)
            .build();

        let config = match config {
            Ok(c) => c,
            Err(err) => panic!("build failed: {}", err),
        };
        let display = format!("{}", config);
        assert!(display.contains("±15%"));
        assert!(display.contains("interval=1000"));
        assert!(display.contains("5-100"));
    }

    #[test]
    fn test_strike_range_config_serialization_roundtrip() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.25)
            .strike_interval(500)
            .min_strikes(10)
            .max_strikes(150)
            .build();

        let config = match config {
            Ok(c) => c,
            Err(err) => panic!("build failed: {}", err),
        };
        let json = match serde_json::to_string(&config) {
            Ok(j) => j,
            Err(err) => panic!("serialization failed: {}", err),
        };
        let deserialized: StrikeRangeConfig = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(err) => panic!("deserialization failed: {}", err),
        };
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_strike_range_config_clone() {
        let config = StrikeRangeConfig::default();
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn test_builder_debug() {
        let builder = StrikeRangeConfig::builder().range_pct(0.10);
        let debug = format!("{:?}", builder);
        assert!(debug.contains("StrikeRangeConfigBuilder"));
    }

    // ========== SharedStrikeRangeConfigs tests ==========

    #[test]
    fn test_shared_strike_range_configs_new_is_empty() {
        let shared = SharedStrikeRangeConfigs::new();
        assert!(shared.is_empty());
        assert_eq!(shared.len(), 0);
    }

    #[test]
    fn test_shared_strike_range_configs_set_get() {
        let shared = SharedStrikeRangeConfigs::new();
        let config = StrikeRangeConfig::default();
        shared.set(ExpiryType::Weekly, config.clone());

        let retrieved = shared.get(ExpiryType::Weekly);
        assert_eq!(retrieved, Some(config));
    }

    #[test]
    fn test_shared_strike_range_configs_get_nonexistent() {
        let shared = SharedStrikeRangeConfigs::new();
        assert!(shared.get(ExpiryType::Daily).is_none());
    }

    #[test]
    fn test_shared_strike_range_configs_overwrite() {
        let shared = SharedStrikeRangeConfigs::new();
        let config1 = StrikeRangeConfig::builder()
            .range_pct(0.10)
            .build()
            .expect("valid");
        let config2 = StrikeRangeConfig::builder()
            .range_pct(0.20)
            .build()
            .expect("valid");

        shared.set(ExpiryType::Weekly, config1);
        shared.set(ExpiryType::Weekly, config2.clone());

        let retrieved = shared.get(ExpiryType::Weekly);
        assert_eq!(retrieved, Some(config2));
    }

    #[test]
    fn test_shared_strike_range_configs_multiple_types() {
        let shared = SharedStrikeRangeConfigs::new();
        shared.set(ExpiryType::Daily, ExpiryType::Daily.default_config());
        shared.set(ExpiryType::Weekly, ExpiryType::Weekly.default_config());
        shared.set(ExpiryType::Monthly, ExpiryType::Monthly.default_config());

        assert_eq!(shared.len(), 3);
        assert!(shared.get(ExpiryType::Daily).is_some());
        assert!(shared.get(ExpiryType::Weekly).is_some());
        assert!(shared.get(ExpiryType::Monthly).is_some());
        assert!(shared.get(ExpiryType::Quarterly).is_none());
    }

    #[test]
    fn test_shared_strike_range_configs_get_all() {
        let shared = SharedStrikeRangeConfigs::new();
        shared.set(ExpiryType::Daily, ExpiryType::Daily.default_config());
        shared.set(ExpiryType::Weekly, ExpiryType::Weekly.default_config());

        let all = shared.get_all();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key(&ExpiryType::Daily));
        assert!(all.contains_key(&ExpiryType::Weekly));
    }

    #[test]
    fn test_shared_strike_range_configs_remove() {
        let shared = SharedStrikeRangeConfigs::new();
        shared.set(ExpiryType::Weekly, ExpiryType::Weekly.default_config());

        let removed = shared.remove(ExpiryType::Weekly);
        assert!(removed.is_some());
        assert!(shared.get(ExpiryType::Weekly).is_none());
        assert!(shared.is_empty());
    }

    #[test]
    fn test_shared_strike_range_configs_remove_nonexistent() {
        let shared = SharedStrikeRangeConfigs::new();
        let removed = shared.remove(ExpiryType::Daily);
        assert!(removed.is_none());
    }

    #[test]
    fn test_shared_strike_range_configs_clear() {
        let shared = SharedStrikeRangeConfigs::new();
        shared.set(ExpiryType::Daily, ExpiryType::Daily.default_config());
        shared.set(ExpiryType::Weekly, ExpiryType::Weekly.default_config());

        shared.clear();
        assert!(shared.is_empty());
    }

    #[test]
    fn test_shared_strike_range_configs_debug() {
        let shared = SharedStrikeRangeConfigs::new();
        let debug = format!("{:?}", shared);
        assert!(debug.contains("SharedStrikeRangeConfigs"));
    }

    #[test]
    fn test_shared_strike_range_configs_default() {
        let shared = SharedStrikeRangeConfigs::default();
        assert!(shared.is_empty());
    }
}
