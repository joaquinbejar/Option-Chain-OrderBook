//! Order validation configuration module.
//!
//! This module provides the [`ValidationConfig`] struct for configuring
//! pre-trade validation rules (tick size, lot size, min/max order size)
//! across the option chain hierarchy.

use crate::error::{Error, Result};

/// Configuration for order validation rules.
///
/// Controls pre-trade validation at the `OrderBook` level:
/// - **Tick size**: prices must be exact multiples of the tick size
/// - **Lot size**: quantities must be exact multiples of the lot size
/// - **Min order size**: orders below this quantity are rejected
/// - **Max order size**: orders above this quantity are rejected
///
/// All fields default to `None`, which disables the corresponding validation.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::ValidationConfig;
///
/// let config = ValidationConfig::new()
///     .with_tick_size(100)
///     .with_lot_size(10)
///     .with_min_order_size(1)
///     .with_max_order_size(1_000_000);
///
/// assert_eq!(config.tick_size(), Some(100));
/// assert_eq!(config.lot_size(), Some(10));
/// assert!(!config.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationConfig {
    /// Minimum price increment. When set, order prices must be exact multiples
    /// of this value. `None` disables tick size validation.
    tick_size: Option<u128>,
    /// Minimum quantity increment. When set, order quantities must be exact
    /// multiples of this value. `None` disables lot size validation.
    lot_size: Option<u64>,
    /// Minimum allowed order quantity. Orders with quantity below this value
    /// are rejected. `None` disables minimum size validation.
    min_order_size: Option<u64>,
    /// Maximum allowed order quantity. Orders with quantity above this value
    /// are rejected. `None` disables maximum size validation.
    max_order_size: Option<u64>,
}

impl ValidationConfig {
    /// Creates a new empty validation configuration with all rules disabled.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the tick size (minimum price increment).
    ///
    /// # Arguments
    ///
    /// * `tick_size` - Minimum price increment in smallest price units
    ///
    /// # Footgun
    ///
    /// This setter is infallible. Setting `0` does NOT disable the rule cleanly
    /// here: the upstream matching engine treats a zero tick as "validation
    /// disabled", so an intended constraint silently vanishes. To disable tick
    /// validation, leave it unset (`None`); to reject a zero tick as an error,
    /// call [`validate`](Self::validate).
    #[must_use]
    #[inline]
    pub const fn with_tick_size(mut self, tick_size: u128) -> Self {
        self.tick_size = Some(tick_size);
        self
    }

    /// Sets the lot size (minimum quantity increment).
    ///
    /// # Arguments
    ///
    /// * `lot_size` - Minimum quantity increment in smallest quantity units
    ///
    /// # Footgun
    ///
    /// This setter is infallible. Setting `0` does NOT disable the rule cleanly
    /// here: the upstream matching engine treats a zero lot as "validation
    /// disabled", so an intended constraint silently vanishes. To disable lot
    /// validation, leave it unset (`None`); to reject a zero lot as an error,
    /// call [`validate`](Self::validate).
    #[must_use]
    #[inline]
    pub const fn with_lot_size(mut self, lot_size: u64) -> Self {
        self.lot_size = Some(lot_size);
        self
    }

    /// Sets the minimum order size.
    ///
    /// # Arguments
    ///
    /// * `min` - Minimum allowed order quantity
    ///
    /// # Footgun
    ///
    /// This setter is infallible. Setting `0` does NOT disable the rule cleanly
    /// here: the upstream matching engine treats a zero minimum as "validation
    /// disabled". Additionally, the minimum must not exceed the maximum (when
    /// both are set), otherwise every order is rejected. To enforce these
    /// invariants, call [`validate`](Self::validate).
    #[must_use]
    #[inline]
    pub const fn with_min_order_size(mut self, min: u64) -> Self {
        self.min_order_size = Some(min);
        self
    }

    /// Sets the maximum order size.
    ///
    /// # Arguments
    ///
    /// * `max` - Maximum allowed order quantity
    ///
    /// # Footgun
    ///
    /// This setter is infallible and does not check that `max >= min`. An
    /// inverted window (`max < min`) rejects every order. Call
    /// [`validate`](Self::validate) to reject an inverted window.
    #[must_use]
    #[inline]
    pub const fn with_max_order_size(mut self, max: u64) -> Self {
        self.max_order_size = Some(max);
        self
    }

    /// Returns the configured tick size, if any.
    #[must_use]
    #[inline]
    pub const fn tick_size(&self) -> Option<u128> {
        self.tick_size
    }

    /// Returns the configured lot size, if any.
    #[must_use]
    #[inline]
    pub const fn lot_size(&self) -> Option<u64> {
        self.lot_size
    }

    /// Returns the configured minimum order size, if any.
    #[must_use]
    #[inline]
    pub const fn min_order_size(&self) -> Option<u64> {
        self.min_order_size
    }

    /// Returns the configured maximum order size, if any.
    #[must_use]
    #[inline]
    pub const fn max_order_size(&self) -> Option<u64> {
        self.max_order_size
    }

    /// Returns `true` if no validation rules are configured.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.tick_size.is_none()
            && self.lot_size.is_none()
            && self.min_order_size.is_none()
            && self.max_order_size.is_none()
    }

    /// Validates the configured rules, rejecting structurally-broken settings.
    ///
    /// A field left unset (`None`) means "rule disabled" and always passes — the
    /// empty default is valid. A field explicitly set to zero is rejected,
    /// because the upstream matching engine treats a zero tick / lot / minimum
    /// as "validation disabled", which silently discards an intended
    /// constraint. An inverted `[min, max]` window is rejected because it would
    /// reject every order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ConfigurationError`] if:
    /// - `tick_size` is set to zero
    /// - `lot_size` is set to zero
    /// - `min_order_size` is set to zero
    /// - both `min_order_size` and `max_order_size` are set and
    ///   `max_order_size < min_order_size`
    pub fn validate(&self) -> Result<()> {
        if self.tick_size == Some(0) {
            return Err(Error::configuration(
                "tick_size must be at least 1 (got 0); leave it unset to disable tick validation",
            ));
        }
        if self.lot_size == Some(0) {
            return Err(Error::configuration(
                "lot_size must be at least 1 (got 0); leave it unset to disable lot validation",
            ));
        }
        if self.min_order_size == Some(0) {
            return Err(Error::configuration(
                "min_order_size must be at least 1 (got 0); leave it unset to disable minimum-size validation",
            ));
        }
        if let (Some(min), Some(max)) = (self.min_order_size, self.max_order_size)
            && max < min
        {
            return Err(Error::configuration(format!(
                "max_order_size ({max}) must be greater than or equal to min_order_size ({min})"
            )));
        }
        Ok(())
    }
}

impl std::fmt::Display for ValidationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return write!(f, "ValidationConfig(none)");
        }
        write!(f, "ValidationConfig(")?;
        let mut first = true;
        if let Some(tick) = self.tick_size {
            write!(f, "tick={tick}")?;
            first = false;
        }
        if let Some(lot) = self.lot_size {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "lot={lot}")?;
            first = false;
        }
        if let Some(min) = self.min_order_size {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "min={min}")?;
            first = false;
        }
        if let Some(max) = self.max_order_size {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "max={max}")?;
        }
        write!(f, ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation_config_default_is_empty() {
        let config = ValidationConfig::new();
        assert!(config.is_empty());
        assert_eq!(config.tick_size(), None);
        assert_eq!(config.lot_size(), None);
        assert_eq!(config.min_order_size(), None);
        assert_eq!(config.max_order_size(), None);
    }

    #[test]
    fn test_validation_config_builder() {
        let config = ValidationConfig::new()
            .with_tick_size(100)
            .with_lot_size(10)
            .with_min_order_size(1)
            .with_max_order_size(1_000_000);

        assert!(!config.is_empty());
        assert_eq!(config.tick_size(), Some(100));
        assert_eq!(config.lot_size(), Some(10));
        assert_eq!(config.min_order_size(), Some(1));
        assert_eq!(config.max_order_size(), Some(1_000_000));
    }

    #[test]
    fn test_validation_config_partial() {
        let config = ValidationConfig::new().with_tick_size(50);

        assert!(!config.is_empty());
        assert_eq!(config.tick_size(), Some(50));
        assert_eq!(config.lot_size(), None);
    }

    #[test]
    fn test_validation_config_clone() {
        let config = ValidationConfig::new()
            .with_tick_size(100)
            .with_lot_size(10);
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn test_validation_config_display_empty() {
        let config = ValidationConfig::new();
        assert_eq!(format!("{config}"), "ValidationConfig(none)");
    }

    #[test]
    fn test_validation_config_display_full() {
        let config = ValidationConfig::new()
            .with_tick_size(100)
            .with_lot_size(10)
            .with_min_order_size(1)
            .with_max_order_size(500);
        let display = format!("{config}");
        assert!(display.contains("tick=100"));
        assert!(display.contains("lot=10"));
        assert!(display.contains("min=1"));
        assert!(display.contains("max=500"));
    }

    #[test]
    fn test_validation_config_display_partial() {
        let config = ValidationConfig::new().with_lot_size(5);
        assert_eq!(format!("{config}"), "ValidationConfig(lot=5)");
    }

    // ========== ValidationConfig::validate tests ==========

    #[test]
    fn test_validation_config_validate_empty_ok() {
        let config = ValidationConfig::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_config_validate_full_ok() {
        let config = ValidationConfig::new()
            .with_tick_size(100)
            .with_lot_size(10)
            .with_min_order_size(1)
            .with_max_order_size(1_000_000);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_config_validate_zero_tick_rejected() {
        let config = ValidationConfig::new().with_tick_size(0);
        let err = match config.validate() {
            Ok(()) => panic!("expected zero tick_size to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("tick_size"));
        assert!(err.to_string().contains('0'));
    }

    #[test]
    fn test_validation_config_validate_zero_lot_rejected() {
        let config = ValidationConfig::new().with_lot_size(0);
        let err = match config.validate() {
            Ok(()) => panic!("expected zero lot_size to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("lot_size"));
    }

    #[test]
    fn test_validation_config_validate_zero_min_rejected() {
        let config = ValidationConfig::new().with_min_order_size(0);
        let err = match config.validate() {
            Ok(()) => panic!("expected zero min_order_size to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("min_order_size"));
    }

    #[test]
    fn test_validation_config_validate_inverted_window_rejected() {
        let config = ValidationConfig::new()
            .with_min_order_size(100)
            .with_max_order_size(10);
        let err = match config.validate() {
            Ok(()) => panic!("expected max < min to be rejected"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("max_order_size"));
        assert!(msg.contains("10"));
        assert!(msg.contains("100"));
    }

    #[test]
    fn test_validation_config_validate_only_max_set_ok() {
        // A max with no min is a valid one-sided window.
        let config = ValidationConfig::new().with_max_order_size(500);
        assert!(config.validate().is_ok());
    }
}
