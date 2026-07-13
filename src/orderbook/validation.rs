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
/// - **Min price**: orders priced below this bound are rejected crate-side
/// - **Max price**: orders priced above this bound are rejected crate-side
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
    /// Maximum allowed order price in smallest price units; orders priced above
    /// are rejected crate-side (the upstream engine has no price-bound hook);
    /// `None` disables.
    max_price: Option<u128>,
    /// Minimum allowed order price in smallest price units; orders priced below
    /// are rejected crate-side (the upstream engine has no price-bound hook);
    /// `None` disables. Together with [`max_price`](Self::max_price) this forms
    /// an inclusive `[min_price, max_price]` band.
    min_price: Option<u128>,
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

    /// Sets the maximum order price (in smallest price units).
    ///
    /// Orders priced above `max_price` are rejected crate-side, because the
    /// upstream matching engine has no price-bound hook to delegate to.
    ///
    /// # Arguments
    ///
    /// * `max_price` - Maximum allowed order price in smallest price units
    ///
    /// # Footgun
    ///
    /// This setter is infallible. Setting `Some(0)` rejects every order (no
    /// price is `<= 0`); to disable the price bound leave it unset (`None`), and
    /// call [`validate`](Self::validate) to reject a zero bound as an error.
    ///
    /// A finite price bound also lets venues prove upstream fee saturation
    /// unreachable (see orderbook-rs `FeeSchedule::max_guaranteed_exact_notional_for_bps`,
    /// available from 0.10.4).
    #[must_use]
    #[inline]
    pub const fn with_max_price(mut self, max_price: u128) -> Self {
        self.max_price = Some(max_price);
        self
    }

    /// Sets the minimum order price (in smallest price units).
    ///
    /// Orders priced below `min_price` are rejected crate-side, because the
    /// upstream matching engine has no price-bound hook to delegate to. Together
    /// with [`with_max_price`](Self::with_max_price) this forms an inclusive
    /// `[min_price, max_price]` band.
    ///
    /// # Arguments
    ///
    /// * `min_price` - Minimum allowed order price in smallest price units
    ///
    /// # Footgun
    ///
    /// This setter is infallible. Setting `Some(0)` is a no-op bound (every
    /// price is `>= 0`); to disable the lower bound leave it unset (`None`), and
    /// call [`validate`](Self::validate) to reject a zero bound as an error. A
    /// `min_price` above the configured `max_price` is an inverted band that
    /// rejects every order; [`validate`](Self::validate) rejects that too.
    #[must_use]
    #[inline]
    pub const fn with_min_price(mut self, min_price: u128) -> Self {
        self.min_price = Some(min_price);
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

    /// Returns the configured maximum order price, if any.
    #[must_use]
    #[inline]
    pub const fn max_price(&self) -> Option<u128> {
        self.max_price
    }

    /// Returns the configured minimum order price, if any.
    #[must_use]
    #[inline]
    pub const fn min_price(&self) -> Option<u128> {
        self.min_price
    }

    /// Returns a copy with the price band tightened against an incoming
    /// `[min, max]` band, applying the canonical **tightest-wins** policy.
    ///
    /// This is the single source of truth for merging two price bands that come
    /// from different sources (for example a `ValidationConfig` slot and a
    /// [`ContractSpecs`](super::contract_specs::ContractSpecs) band) into one
    /// effective band at leaf creation:
    ///
    /// - the resulting **upper** bound is the *smaller* of the two `max_price`
    ///   values (the tighter cap wins);
    /// - the resulting **lower** bound is the *larger* of the two `min_price`
    ///   values (the tighter floor wins);
    /// - when one side is unset on one input, the value from the other input is
    ///   carried through unchanged.
    ///
    /// Only the two band fields are affected; tick / lot / order-size fields are
    /// left as-is.
    ///
    /// This merge is deliberately *not* self-validating: two individually
    /// coherent bands from different sources can still combine into an inverted
    /// `min_price > max_price` band that rejects every order. That incoherence is
    /// intended to surface (via [`validate`](Self::validate) or a caller-side
    /// warning), not to be silently repaired here.
    #[must_use]
    pub const fn tightened_price_band(mut self, min: Option<u128>, max: Option<u128>) -> Self {
        self.max_price = match (self.max_price, max) {
            // Tighter cap wins: the smaller upper bound.
            (Some(a), Some(b)) => Some(if a < b { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        self.min_price = match (self.min_price, min) {
            // Tighter floor wins: the larger lower bound.
            (Some(a), Some(b)) => Some(if a > b { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        self
    }

    /// Returns `true` if no validation rules are configured.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.tick_size.is_none()
            && self.lot_size.is_none()
            && self.min_order_size.is_none()
            && self.max_order_size.is_none()
            && self.max_price.is_none()
            && self.min_price.is_none()
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
    /// - `max_price` is set to zero
    /// - `min_price` is set to zero
    /// - both `min_order_size` and `max_order_size` are set and
    ///   `max_order_size < min_order_size`
    /// - both `min_price` and `max_price` are set and `min_price > max_price`
    ///   (an inverted band; `min_price == max_price` is a valid single-price band)
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
        if self.max_price == Some(0) {
            return Err(Error::configuration(
                "max_price must be at least 1 (got 0); leave it unset to disable the price bound",
            ));
        }
        if self.min_price == Some(0) {
            return Err(Error::configuration(
                "min_price must be at least 1 (got 0); leave it unset to disable the price bound",
            ));
        }
        if let (Some(min), Some(max)) = (self.min_order_size, self.max_order_size)
            && max < min
        {
            return Err(Error::configuration(format!(
                "max_order_size ({max}) must be greater than or equal to min_order_size ({min})"
            )));
        }
        if let (Some(min), Some(max)) = (self.min_price, self.max_price)
            && min > max
        {
            return Err(Error::configuration(format!(
                "min_price ({min}) must be less than or equal to max_price ({max})"
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
            first = false;
        }
        if let Some(min_price) = self.min_price {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "min_price={min_price}")?;
            first = false;
        }
        if let Some(max_price) = self.max_price {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "max_price={max_price}")?;
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

    // ========== ValidationConfig::max_price tests ==========

    #[test]
    fn test_validation_config_with_max_price_builder() {
        let config = ValidationConfig::new().with_max_price(1_000_000);
        assert_eq!(config.max_price(), Some(1_000_000));
    }

    #[test]
    fn test_validation_config_is_empty_false_with_only_max_price() {
        let config = ValidationConfig::new().with_max_price(500);
        assert!(!config.is_empty());
        assert_eq!(config.tick_size(), None);
        assert_eq!(config.max_order_size(), None);
    }

    #[test]
    fn test_validation_config_validate_zero_max_price_rejected() {
        let config = ValidationConfig::new().with_max_price(0);
        let err = match config.validate() {
            Ok(()) => panic!("expected zero max_price to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("max_price"));
        assert!(err.to_string().contains('0'));
    }

    #[test]
    fn test_validation_config_validate_max_price_set_ok() {
        let config = ValidationConfig::new().with_max_price(1_000_000);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_config_display_includes_max_price() {
        let config = ValidationConfig::new()
            .with_tick_size(100)
            .with_max_price(999);
        let display = format!("{config}");
        assert!(display.contains("tick=100"));
        assert!(display.contains("max_price=999"));
    }

    // ========== ValidationConfig::min_price / price band tests ==========

    #[test]
    fn test_validation_config_with_min_price_builder() {
        let config = ValidationConfig::new().with_min_price(100);
        assert_eq!(config.min_price(), Some(100));
    }

    #[test]
    fn test_validation_config_is_empty_false_with_only_min_price() {
        let config = ValidationConfig::new().with_min_price(50);
        assert!(!config.is_empty());
        assert_eq!(config.tick_size(), None);
        assert_eq!(config.max_price(), None);
    }

    #[test]
    fn test_validation_config_validate_zero_min_price_rejected() {
        let config = ValidationConfig::new().with_min_price(0);
        let err = match config.validate() {
            Ok(()) => panic!("expected zero min_price to be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("min_price"));
        assert!(err.to_string().contains('0'));
    }

    #[test]
    fn test_validation_config_validate_min_price_set_ok() {
        let config = ValidationConfig::new()
            .with_min_price(100)
            .with_max_price(1_000);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_config_validate_inverted_price_band_rejected() {
        // min_price above max_price is an inverted band that rejects everything.
        let config = ValidationConfig::new()
            .with_min_price(1_000)
            .with_max_price(500);
        let err = match config.validate() {
            Ok(()) => panic!("expected an inverted price band to be rejected"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("min_price"));
        assert!(msg.contains("500"));
        assert!(msg.contains("1000"));
    }

    #[test]
    fn test_validation_config_validate_degenerate_price_band_ok() {
        // A single-price band (min == max) is valid: it admits exactly one price.
        let config = ValidationConfig::new()
            .with_min_price(750)
            .with_max_price(750);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_config_display_min_price_before_max_price() {
        let config = ValidationConfig::new()
            .with_min_price(100)
            .with_max_price(900);
        let display = format!("{config}");
        let min_at = display
            .find("min_price=100")
            .expect("min_price segment present");
        let max_at = display
            .find("max_price=900")
            .expect("max_price segment present");
        assert!(
            min_at < max_at,
            "min_price must render before max_price: {display}"
        );
    }

    #[test]
    fn test_tightened_price_band_takes_min_of_maxes() {
        // Two upper bounds: the tighter (smaller) one wins.
        let config = ValidationConfig::new()
            .with_max_price(1_000)
            .tightened_price_band(None, Some(800));
        assert_eq!(config.max_price(), Some(800));
    }

    #[test]
    fn test_tightened_price_band_takes_max_of_mins() {
        // Two lower bounds: the tighter (larger) one wins.
        let config = ValidationConfig::new()
            .with_min_price(100)
            .tightened_price_band(Some(250), None);
        assert_eq!(config.min_price(), Some(250));
    }

    #[test]
    fn test_tightened_price_band_fills_unset_side_from_other() {
        // Each side is unset on exactly one input; the merge carries both through.
        let config = ValidationConfig::new()
            .with_max_price(1_000)
            .tightened_price_band(Some(100), None);
        assert_eq!(config.min_price(), Some(100));
        assert_eq!(config.max_price(), Some(1_000));
    }

    #[test]
    fn test_tightened_price_band_preserves_non_band_fields() {
        // Only the two band fields change; tick/lot/order-size are untouched.
        let config = ValidationConfig::new()
            .with_tick_size(10)
            .with_lot_size(2)
            .with_min_order_size(1)
            .with_max_order_size(500)
            .tightened_price_band(Some(100), Some(900));
        assert_eq!(config.tick_size(), Some(10));
        assert_eq!(config.lot_size(), Some(2));
        assert_eq!(config.min_order_size(), Some(1));
        assert_eq!(config.max_order_size(), Some(500));
        assert_eq!(config.min_price(), Some(100));
        assert_eq!(config.max_price(), Some(900));
    }

    #[test]
    fn test_tightened_price_band_incoherent_yields_inverted_that_fails_validate() {
        // Two individually-coherent bands can merge into an inverted band:
        // floor from one source (600) above cap from the other (500). The merge
        // does not repair it; validate() surfaces the incoherence.
        let config = ValidationConfig::new()
            .with_min_price(600)
            .tightened_price_band(None, Some(500));
        assert_eq!(config.min_price(), Some(600));
        assert_eq!(config.max_price(), Some(500));
        assert!(config.validate().is_err());
    }
}
