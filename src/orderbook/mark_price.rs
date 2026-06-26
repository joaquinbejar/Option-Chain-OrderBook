//! Mark price calculation module.
//!
//! This module provides [`MarkPriceCalculator`] for computing the mark price as a
//! weighted average of index price, order book mid price, and last trade price,
//! with configurable dampening for manipulation resistance.
//!
//! ## Overview
//!
//! Mark price is used for:
//! - Position valuation and P&L calculation
//! - Margin requirement computation
//! - Liquidation triggering
//!
//! The calculator combines three price sources with configurable weights:
//! - **Index price**: External reference price (e.g., from Chainlink)
//! - **Mid price**: Order book best bid/ask midpoint
//! - **Last trade price**: Most recent execution price
//!
//! ## Dampening
//!
//! To prevent manipulation, the mark price change is limited per update by the
//! dampening factor. For example, with `dampening_factor = 0.01`, the mark price
//! can only move ±1% from its previous value in a single update.
//!
//! ## Example
//!
//! ```
//! use option_chain_orderbook::orderbook::{MarkPriceCalculator, MarkPriceConfig};
//!
//! let config = MarkPriceConfig::builder()
//!     .index_weight(0.5)
//!     .mid_weight(0.3)
//!     .last_trade_weight(0.2)
//!     .dampening_factor(0.01)
//!     .build()
//!     .expect("valid config");
//!
//! let calculator = MarkPriceCalculator::new(config);
//!
//! calculator.update_index_price(50000);
//! calculator.update_mid_price(50100);
//! calculator.update_last_trade_price(50050);
//!
//! // Advance the mark one dampening step for this update cycle...
//! let mark = calculator.advance_mark();
//! assert!(mark.is_some());
//! // ...then read it back any number of times without advancing dampening.
//! assert_eq!(calculator.current_mark_price(), mark);
//! ```
//!
//! ## Read vs. tick
//!
//! Computing the mark is a *mutating* step: each call to
//! [`MarkPriceCalculator::advance_mark`] moves the stored mark one dampening
//! step toward the raw weighted average and commits it. Call it **exactly once
//! per update cycle**. To merely observe the last committed mark — for
//! monitoring, display, or P&L snapshots — use the pure
//! [`MarkPriceCalculator::current_mark_price`], which never advances dampening
//! and is idempotent for static inputs.

use crate::error::{Error, Result};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Configuration for mark price calculation.
///
/// Defines the weights for each price source and the dampening factor that
/// limits how much the mark price can change per update.
///
/// All weight and dampening values are stored as [`Decimal`] for
/// deterministic, architecture-independent arithmetic.
///
/// ## Validation
///
/// - All weights must be in the range \[0, 1\]
/// - Weights must sum to exactly 1
/// - Dampening factor must be in the range (0, 1\]
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::MarkPriceConfig;
///
/// let config = MarkPriceConfig::builder()
///     .index_weight(0.5)
///     .mid_weight(0.3)
///     .last_trade_weight(0.2)
///     .build()
///     .expect("valid config");
///
/// assert_eq!(config.index_weight(), rust_decimal::Decimal::new(5, 1));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkPriceConfig {
    /// Weight for index price in range \[0, 1\].
    index_weight: Decimal,
    /// Weight for order book mid price in range \[0, 1\].
    mid_weight: Decimal,
    /// Weight for last trade price in range \[0, 1\].
    last_trade_weight: Decimal,
    /// Maximum price change per update as a fraction in range (0, 1\].
    /// For example, 0.01 means mark price can move at most 1% per update.
    dampening_factor: Decimal,
}

impl Default for MarkPriceConfig {
    fn default() -> Self {
        Self {
            index_weight: Decimal::new(5, 1),
            mid_weight: Decimal::new(3, 1),
            last_trade_weight: Decimal::new(2, 1),
            dampening_factor: Decimal::new(1, 2),
        }
    }
}

impl MarkPriceConfig {
    /// Creates a new builder for `MarkPriceConfig`.
    #[must_use]
    pub fn builder() -> MarkPriceConfigBuilder {
        MarkPriceConfigBuilder::new()
    }

    /// Returns the weight for index price.
    #[must_use]
    #[inline]
    pub fn index_weight(&self) -> Decimal {
        self.index_weight
    }

    /// Returns the weight for mid price.
    #[must_use]
    #[inline]
    pub fn mid_weight(&self) -> Decimal {
        self.mid_weight
    }

    /// Returns the weight for last trade price.
    #[must_use]
    #[inline]
    pub fn last_trade_weight(&self) -> Decimal {
        self.last_trade_weight
    }

    /// Returns the dampening factor.
    #[must_use]
    #[inline]
    pub fn dampening_factor(&self) -> Decimal {
        self.dampening_factor
    }

    /// Validates the configuration.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - Any weight is outside [0.0, 1.0]
    /// - Weights don't sum to approximately 1.0
    /// - Dampening factor is outside (0.0, 1.0]
    pub fn validate(&self) -> Result<()> {
        // Check weight bounds
        if self.index_weight < Decimal::ZERO || self.index_weight > Decimal::ONE {
            return Err(Error::configuration(format!(
                "index_weight must be in [0, 1], got {}",
                self.index_weight
            )));
        }
        if self.mid_weight < Decimal::ZERO || self.mid_weight > Decimal::ONE {
            return Err(Error::configuration(format!(
                "mid_weight must be in [0, 1], got {}",
                self.mid_weight
            )));
        }
        if self.last_trade_weight < Decimal::ZERO || self.last_trade_weight > Decimal::ONE {
            return Err(Error::configuration(format!(
                "last_trade_weight must be in [0, 1], got {}",
                self.last_trade_weight
            )));
        }

        // Check weights sum to exactly 1 (Decimal has no floating-point drift)
        let sum = self
            .index_weight
            .checked_add(self.mid_weight)
            .and_then(|s| s.checked_add(self.last_trade_weight));
        match sum {
            Some(s) if s == Decimal::ONE => {}
            Some(s) => {
                return Err(Error::configuration(format!(
                    "weights must sum to 1, got {}",
                    s
                )));
            }
            None => {
                return Err(Error::configuration("overflow computing weight sum"));
            }
        }

        // Check dampening factor
        if self.dampening_factor <= Decimal::ZERO || self.dampening_factor > Decimal::ONE {
            return Err(Error::configuration(format!(
                "dampening_factor must be in (0, 1], got {}",
                self.dampening_factor
            )));
        }

        Ok(())
    }
}

/// Builder for [`MarkPriceConfig`].
///
/// Provides a fluent interface for constructing mark price configuration
/// with validation on build.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::MarkPriceConfig;
///
/// let config = MarkPriceConfig::builder()
///     .index_weight(0.6)
///     .mid_weight(0.25)
///     .last_trade_weight(0.15)
///     .dampening_factor(0.02)
///     .build()
///     .expect("valid config");
/// ```
#[derive(Debug, Clone, Default)]
pub struct MarkPriceConfigBuilder {
    index_weight: Option<Decimal>,
    mid_weight: Option<Decimal>,
    last_trade_weight: Option<Decimal>,
    dampening_factor: Option<Decimal>,
}

impl MarkPriceConfigBuilder {
    /// Creates a new builder with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the weight for index price.
    ///
    /// Accepts `f64` for ergonomic construction; the value is converted to
    /// [`Decimal`] internally for deterministic arithmetic.
    ///
    /// # Arguments
    ///
    /// * `weight` - Weight in range \[0.0, 1.0\]
    #[must_use]
    pub fn index_weight(mut self, weight: f64) -> Self {
        self.index_weight = Decimal::try_from(weight).ok();
        self
    }

    /// Sets the weight for mid price.
    ///
    /// Accepts `f64` for ergonomic construction; the value is converted to
    /// [`Decimal`] internally for deterministic arithmetic.
    ///
    /// # Arguments
    ///
    /// * `weight` - Weight in range \[0.0, 1.0\]
    #[must_use]
    pub fn mid_weight(mut self, weight: f64) -> Self {
        self.mid_weight = Decimal::try_from(weight).ok();
        self
    }

    /// Sets the weight for last trade price.
    ///
    /// Accepts `f64` for ergonomic construction; the value is converted to
    /// [`Decimal`] internally for deterministic arithmetic.
    ///
    /// # Arguments
    ///
    /// * `weight` - Weight in range \[0.0, 1.0\]
    #[must_use]
    pub fn last_trade_weight(mut self, weight: f64) -> Self {
        self.last_trade_weight = Decimal::try_from(weight).ok();
        self
    }

    /// Sets the dampening factor.
    ///
    /// Accepts `f64` for ergonomic construction; the value is converted to
    /// [`Decimal`] internally for deterministic arithmetic.
    ///
    /// # Arguments
    ///
    /// * `factor` - Maximum price change per update as a fraction (e.g., 0.01 = 1%)
    #[must_use]
    pub fn dampening_factor(mut self, factor: f64) -> Self {
        self.dampening_factor = Decimal::try_from(factor).ok();
        self
    }

    /// Builds the configuration, validating all parameters.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if validation fails.
    pub fn build(self) -> Result<MarkPriceConfig> {
        let defaults = MarkPriceConfig::default();

        let config = MarkPriceConfig {
            index_weight: self.index_weight.unwrap_or(defaults.index_weight),
            mid_weight: self.mid_weight.unwrap_or(defaults.mid_weight),
            last_trade_weight: self.last_trade_weight.unwrap_or(defaults.last_trade_weight),
            dampening_factor: self.dampening_factor.unwrap_or(defaults.dampening_factor),
        };

        config.validate()?;
        Ok(config)
    }
}

/// Thread-safe mark price calculator.
///
/// Computes the mark price as a weighted average of index price, mid price,
/// and last trade price, with dampening to limit price movement.
///
/// ## Thread Safety
///
/// All price updates and reads use atomic operations, making this safe for
/// concurrent access from multiple threads without external synchronization.
/// The dampening logic uses a compare-and-swap loop to guarantee the
/// dampening invariant holds even under concurrent [`advance_mark`] calls.
///
/// [`advance_mark`]: MarkPriceCalculator::advance_mark
///
/// Note that the three input prices (index, mid, last trade) are loaded
/// individually — they do not form an atomic snapshot. Under rapid concurrent
/// updates a mark price computation may see a mix of old and new inputs.
/// This is acceptable because mark price is recomputed frequently and the
/// inputs converge quickly.
///
/// ## Precision
///
/// Prices are stored as `u64` and converted to `f64` for the weighted
/// average calculation. Values above 2^53 (≈ 9 × 10^15) may lose
/// integer precision through the `f64` round-trip. For typical financial
/// prices in smallest units (satoshis, wei, cents) this is not a concern.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::{MarkPriceCalculator, MarkPriceConfig};
///
/// let calculator = MarkPriceCalculator::with_default_config();
///
/// // Update prices
/// calculator.update_index_price(50000);
/// calculator.update_mid_price(50100);
/// calculator.update_last_trade_price(50050);
///
/// // Advance the mark once for this update cycle.
/// if let Some(mark) = calculator.advance_mark() {
///     println!("Mark price: {}", mark);
/// }
/// // Subsequent observations do not advance dampening.
/// let _ = calculator.current_mark_price();
/// ```
pub struct MarkPriceCalculator {
    /// Configuration for weights and dampening.
    config: MarkPriceConfig,
    /// Latest index price (external reference).
    index_price: AtomicU64,
    /// Latest mid price (order book midpoint).
    mid_price: AtomicU64,
    /// Latest last trade price.
    last_trade_price: AtomicU64,
    /// Previously computed mark price for dampening.
    last_mark_price: AtomicU64,
}

impl MarkPriceCalculator {
    /// Creates a new calculator with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Mark price configuration
    #[must_use]
    pub fn new(config: MarkPriceConfig) -> Self {
        Self {
            config,
            index_price: AtomicU64::new(0),
            mid_price: AtomicU64::new(0),
            last_trade_price: AtomicU64::new(0),
            last_mark_price: AtomicU64::new(0),
        }
    }

    /// Creates a new calculator with default configuration.
    ///
    /// Default weights: index=0.5, mid=0.3, last_trade=0.2
    /// Default dampening: 1% (0.01)
    #[must_use]
    pub fn with_default_config() -> Self {
        Self::new(MarkPriceConfig::default())
    }

    /// Returns a reference to the configuration.
    #[must_use]
    #[inline]
    pub fn config(&self) -> &MarkPriceConfig {
        &self.config
    }

    /// Updates the index price.
    ///
    /// # Arguments
    ///
    /// * `price` - New index price in smallest units
    #[inline]
    pub fn update_index_price(&self, price: u64) {
        self.index_price.store(price, Ordering::Release);
    }

    /// Updates the mid price (order book midpoint).
    ///
    /// # Arguments
    ///
    /// * `price` - New mid price in smallest units
    #[inline]
    pub fn update_mid_price(&self, price: u64) {
        self.mid_price.store(price, Ordering::Release);
    }

    /// Updates the last trade price.
    ///
    /// # Arguments
    ///
    /// * `price` - New last trade price in smallest units
    #[inline]
    pub fn update_last_trade_price(&self, price: u64) {
        self.last_trade_price.store(price, Ordering::Release);
    }

    /// Returns the current index price.
    #[must_use]
    #[inline]
    pub fn index_price(&self) -> u64 {
        self.index_price.load(Ordering::Acquire)
    }

    /// Returns the current mid price.
    #[must_use]
    #[inline]
    pub fn mid_price(&self) -> u64 {
        self.mid_price.load(Ordering::Acquire)
    }

    /// Returns the current last trade price.
    #[must_use]
    #[inline]
    pub fn last_trade_price(&self) -> u64 {
        self.last_trade_price.load(Ordering::Acquire)
    }

    /// Returns the last committed mark price as a raw integer (`0` if unset).
    ///
    /// This is the same stored value exposed by
    /// [`current_mark_price`](Self::current_mark_price), but as a bare `u64`
    /// where `0` means "no mark committed yet" instead of `None`. Reading it
    /// never advances dampening.
    #[must_use]
    #[inline]
    pub fn last_mark_price(&self) -> u64 {
        self.last_mark_price.load(Ordering::Acquire)
    }

    /// Returns the last committed mark price **without** advancing dampening.
    ///
    /// This is a pure accessor: it loads the stored mark and returns it
    /// unchanged. It performs no weighted-average computation and no
    /// compare-and-swap, so calling it repeatedly with static inputs is
    /// idempotent — the value never walks toward the raw weighted average.
    ///
    /// Use this for monitoring, display, and P&L snapshots. To move the mark
    /// one dampening step for an update cycle, call
    /// [`advance_mark`](Self::advance_mark) instead.
    ///
    /// # Returns
    ///
    /// - `Some(price)` if a mark has been committed by a prior
    ///   [`advance_mark`](Self::advance_mark) call
    /// - `None` if no mark has been committed yet (or after [`reset`](Self::reset))
    #[must_use]
    #[inline]
    pub fn current_mark_price(&self) -> Option<u64> {
        match self.last_mark_price.load(Ordering::Acquire) {
            0 => None,
            committed => Some(committed),
        }
    }

    /// Advances the mark price by exactly one dampening step and commits it.
    ///
    /// This is the **mutating tick**: it computes the raw weighted average of
    /// the current inputs, clamps the change to the dampening band relative to
    /// the last committed mark, then stores and returns the new value. Each
    /// call advances the mark at most one dampening step toward the raw
    /// weighted average.
    ///
    /// # Per-tick contract
    ///
    /// Call this **exactly once per update cycle**. Because every call commits
    /// a new value, calling it repeatedly with static inputs will walk the mark
    /// toward the raw weighted average one dampening step per call. To merely
    /// observe the committed mark without advancing, use the pure
    /// [`current_mark_price`](Self::current_mark_price).
    ///
    /// # Returns
    ///
    /// - `Some(price)` if at least one input price is non-zero
    /// - `None` if all input prices are zero (no mark is committed)
    ///
    /// # Algorithm
    ///
    /// 1. Load all input prices (individually atomic, not a consistent snapshot)
    /// 2. Compute weighted average, using only non-zero inputs
    /// 3. Re-normalize weights if some inputs are missing
    /// 4. Apply dampening via CAS loop: clamp change to ±ceil(prev × dampening_factor)
    /// 5. Store and return the new mark price
    pub fn advance_mark(&self) -> Option<u64> {
        let raw_mark = self.raw_weighted_average()?;

        // Apply dampening using a CAS loop so concurrent updates always
        // respect the dampening invariant relative to the latest stored value.
        let mut prev_mark = self.last_mark_price.load(Ordering::Acquire);
        loop {
            let final_mark = self.dampen(prev_mark, raw_mark);

            match self.last_mark_price.compare_exchange(
                prev_mark,
                final_mark,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(final_mark),
                Err(actual_prev) => {
                    // Another thread updated the mark price; retry with the
                    // latest value so dampening is applied correctly.
                    prev_mark = actual_prev;
                }
            }
        }
    }

    /// Computes the mark price, advancing dampening one step (deprecated).
    ///
    /// This call **mutates on read**: each invocation commits a new dampened
    /// value, so two consecutive calls with identical inputs return different
    /// results. Prefer the explicit split:
    ///
    /// - [`current_mark_price`](Self::current_mark_price) to read the committed
    ///   mark without advancing dampening, or
    /// - [`advance_mark`](Self::advance_mark) to advance one dampening step per
    ///   update cycle.
    ///
    /// Retained as a thin alias delegating to [`advance_mark`](Self::advance_mark)
    /// to preserve the previous behavior for existing callers.
    ///
    /// # Returns
    ///
    /// - `Some(price)` if at least one input price is non-zero
    /// - `None` if all input prices are zero
    #[deprecated(
        since = "0.5.1",
        note = "mark_price() mutates on read; use current_mark_price() to read or advance_mark() to tick"
    )]
    #[must_use]
    pub fn mark_price(&self) -> Option<u64> {
        self.advance_mark()
    }

    /// Computes the raw weighted average of the current inputs (no dampening).
    ///
    /// Returns `None` if all inputs are zero or the active weights sum to zero.
    #[must_use]
    fn raw_weighted_average(&self) -> Option<u64> {
        let index = self.index_price.load(Ordering::Acquire);
        let mid = self.mid_price.load(Ordering::Acquire);
        let last_trade = self.last_trade_price.load(Ordering::Acquire);

        // If all prices are zero, no mark price available
        if index == 0 && mid == 0 && last_trade == 0 {
            return None;
        }

        // Compute weighted sum using Decimal for deterministic arithmetic,
        // only including non-zero prices.
        let mut weighted_sum = Decimal::ZERO;
        let mut total_weight = Decimal::ZERO;

        if index > 0 {
            weighted_sum = weighted_sum
                .saturating_add(Decimal::from(index).saturating_mul(self.config.index_weight));
            total_weight = total_weight.saturating_add(self.config.index_weight);
        }
        if mid > 0 {
            weighted_sum = weighted_sum
                .saturating_add(Decimal::from(mid).saturating_mul(self.config.mid_weight));
            total_weight = total_weight.saturating_add(self.config.mid_weight);
        }
        if last_trade > 0 {
            weighted_sum = weighted_sum.saturating_add(
                Decimal::from(last_trade).saturating_mul(self.config.last_trade_weight),
            );
            total_weight = total_weight.saturating_add(self.config.last_trade_weight);
        }

        // Normalize if not all inputs are present
        if total_weight > Decimal::ZERO {
            let avg = weighted_sum
                .checked_div(total_weight)
                .unwrap_or(Decimal::ZERO);
            Some(avg.to_u64().unwrap_or(0))
        } else {
            None
        }
    }

    /// Clamps `raw_mark` to the dampening band around `prev_mark`.
    ///
    /// With `prev_mark == 0` (no committed mark yet) the raw value is returned
    /// unchanged. Otherwise the change is clamped to
    /// ±ceil(`prev_mark` × `dampening_factor`), with a floor of 1 unit so a
    /// non-zero target is never permanently stuck.
    #[inline]
    fn dampen(&self, prev_mark: u64, raw_mark: u64) -> u64 {
        if prev_mark > 0 {
            let base_change = Decimal::from(prev_mark).saturating_mul(self.config.dampening_factor);
            let ceil_change = base_change.ceil();
            let mut max_change = ceil_change.to_u64().unwrap_or(0);
            if max_change == 0 && raw_mark != prev_mark {
                max_change = 1;
            }
            let min_price = prev_mark.saturating_sub(max_change);
            let max_price = prev_mark.saturating_add(max_change);
            raw_mark.clamp(min_price, max_price)
        } else {
            // First calculation, no dampening
            raw_mark
        }
    }

    /// Resets all prices to zero.
    ///
    /// Useful for testing or when switching instruments.
    pub fn reset(&self) {
        self.index_price.store(0, Ordering::Release);
        self.mid_price.store(0, Ordering::Release);
        self.last_trade_price.store(0, Ordering::Release);
        self.last_mark_price.store(0, Ordering::Release);
    }
}

impl std::fmt::Debug for MarkPriceCalculator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkPriceCalculator")
            .field("config", &self.config)
            .field("index_price", &self.index_price.load(Ordering::Relaxed))
            .field("mid_price", &self.mid_price.load(Ordering::Relaxed))
            .field(
                "last_trade_price",
                &self.last_trade_price.load(Ordering::Relaxed),
            )
            .field(
                "last_mark_price",
                &self.last_mark_price.load(Ordering::Relaxed),
            )
            .finish()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // ── MarkPriceConfig Tests ────────────────────────────────────────────

    #[test]
    fn test_default_config() {
        let config = MarkPriceConfig::default();
        assert_eq!(config.index_weight(), dec!(0.5));
        assert_eq!(config.mid_weight(), dec!(0.3));
        assert_eq!(config.last_trade_weight(), dec!(0.2));
        assert_eq!(config.dampening_factor(), dec!(0.01));
    }

    #[test]
    fn test_config_validation_valid() {
        let config = MarkPriceConfig {
            index_weight: dec!(0.5),
            mid_weight: dec!(0.3),
            last_trade_weight: dec!(0.2),
            dampening_factor: dec!(0.01),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_weights_dont_sum_to_one() {
        let config = MarkPriceConfig {
            index_weight: dec!(0.5),
            mid_weight: dec!(0.3),
            last_trade_weight: dec!(0.3), // Sum = 1.1
            dampening_factor: dec!(0.01),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_weight_out_of_range() {
        let config = MarkPriceConfig {
            index_weight: dec!(1.5), // > 1
            mid_weight: dec!(0.0),
            last_trade_weight: dec!(-0.5), // < 0
            dampening_factor: dec!(0.01),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_dampening_zero() {
        let config = MarkPriceConfig {
            index_weight: dec!(0.5),
            mid_weight: dec!(0.3),
            last_trade_weight: dec!(0.2),
            dampening_factor: dec!(0.0), // Invalid
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_dampening_greater_than_one() {
        let config = MarkPriceConfig {
            index_weight: dec!(0.5),
            mid_weight: dec!(0.3),
            last_trade_weight: dec!(0.2),
            dampening_factor: dec!(1.5), // Invalid
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_negative_dampening() {
        let config = MarkPriceConfig {
            index_weight: dec!(0.5),
            mid_weight: dec!(0.3),
            last_trade_weight: dec!(0.2),
            dampening_factor: dec!(-0.01),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let config = MarkPriceConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: MarkPriceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.index_weight(), config.index_weight());
        assert_eq!(deserialized.mid_weight(), config.mid_weight());
    }

    // ── MarkPriceConfigBuilder Tests ─────────────────────────────────────

    #[test]
    fn test_builder_default_values() {
        let config = MarkPriceConfig::builder().build().unwrap();
        assert_eq!(config.index_weight(), dec!(0.5));
        assert_eq!(config.mid_weight(), dec!(0.3));
        assert_eq!(config.last_trade_weight(), dec!(0.2));
    }

    #[test]
    fn test_builder_custom_values() {
        let config = MarkPriceConfig::builder()
            .index_weight(0.6)
            .mid_weight(0.25)
            .last_trade_weight(0.15)
            .dampening_factor(0.02)
            .build()
            .unwrap();

        assert_eq!(config.index_weight(), dec!(0.6));
        assert_eq!(config.mid_weight(), dec!(0.25));
        assert_eq!(config.last_trade_weight(), dec!(0.15));
        assert_eq!(config.dampening_factor(), dec!(0.02));
    }

    #[test]
    fn test_builder_invalid_weights() {
        let result = MarkPriceConfig::builder()
            .index_weight(0.5)
            .mid_weight(0.5)
            .last_trade_weight(0.5) // Sum = 1.5
            .build();
        assert!(result.is_err());
    }

    // ── MarkPriceCalculator Tests ────────────────────────────────────────

    #[test]
    fn test_calculator_creation() {
        let calc = MarkPriceCalculator::with_default_config();
        assert_eq!(calc.index_price(), 0);
        assert_eq!(calc.mid_price(), 0);
        assert_eq!(calc.last_trade_price(), 0);
    }

    #[test]
    fn test_calculator_no_prices() {
        let calc = MarkPriceCalculator::with_default_config();
        assert!(calc.advance_mark().is_none());
    }

    #[test]
    fn test_calculator_all_prices_present() {
        let calc = MarkPriceCalculator::with_default_config();

        calc.update_index_price(50000);
        calc.update_mid_price(50000);
        calc.update_last_trade_price(50000);

        let mark = calc.advance_mark();
        assert!(mark.is_some());
        // All same price, weighted average should equal the price
        assert_eq!(mark.unwrap(), 50000);
    }

    #[test]
    fn test_calculator_weighted_average() {
        // Weights: index=0.5, mid=0.3, last=0.2
        let calc = MarkPriceCalculator::with_default_config();

        calc.update_index_price(100);
        calc.update_mid_price(200);
        calc.update_last_trade_price(300);

        let mark = calc.advance_mark().unwrap();
        // Expected: 100*0.5 + 200*0.3 + 300*0.2 = 50 + 60 + 60 = 170
        assert_eq!(mark, 170);
    }

    #[test]
    fn test_calculator_partial_prices_index_only() {
        let calc = MarkPriceCalculator::with_default_config();

        calc.update_index_price(50000);

        let mark = calc.advance_mark();
        assert!(mark.is_some());
        // Only index present, should use full weight on index
        assert_eq!(mark.unwrap(), 50000);
    }

    #[test]
    fn test_calculator_partial_prices_mid_and_last() {
        let config = MarkPriceConfig::builder()
            .index_weight(0.4)
            .mid_weight(0.3)
            .last_trade_weight(0.3)
            .build()
            .unwrap();
        let calc = MarkPriceCalculator::new(config);

        calc.update_mid_price(100);
        calc.update_last_trade_price(200);

        let mark = calc.advance_mark().unwrap();
        // Normalize weights: mid=0.3/(0.3+0.3)=0.5, last=0.5
        // Expected: 100*0.5 + 200*0.5 = 150
        assert_eq!(mark, 150);
    }

    #[test]
    fn test_calculator_dampening() {
        let config = MarkPriceConfig::builder()
            .index_weight(1.0)
            .mid_weight(0.0)
            .last_trade_weight(0.0)
            .dampening_factor(0.10) // 10% max change
            .build()
            .unwrap();
        let calc = MarkPriceCalculator::new(config);

        // First update: no dampening
        calc.update_index_price(1000);
        let mark1 = calc.advance_mark().unwrap();
        assert_eq!(mark1, 1000);

        // Second update: try to jump to 2000 (100% increase)
        // Should be clamped to 1000 + 10% = 1100
        calc.update_index_price(2000);
        let mark2 = calc.advance_mark().unwrap();
        assert_eq!(mark2, 1100);

        // Third update: continue toward 2000
        // From 1100, max is 1100 + 110 = 1210
        calc.update_index_price(2000);
        let mark3 = calc.advance_mark().unwrap();
        assert_eq!(mark3, 1210);
    }

    #[test]
    fn test_calculator_dampening_decrease() {
        let config = MarkPriceConfig::builder()
            .index_weight(1.0)
            .mid_weight(0.0)
            .last_trade_weight(0.0)
            .dampening_factor(0.10) // 10% max change
            .build()
            .unwrap();
        let calc = MarkPriceCalculator::new(config);

        // First update
        calc.update_index_price(1000);
        let mark1 = calc.advance_mark().unwrap();
        assert_eq!(mark1, 1000);

        // Try to drop to 500 (50% decrease)
        // Should be clamped to 1000 - 10% = 900
        calc.update_index_price(500);
        let mark2 = calc.advance_mark().unwrap();
        assert_eq!(mark2, 900);
    }

    #[test]
    fn test_calculator_dampening_small_price() {
        let config = MarkPriceConfig::builder()
            .index_weight(1.0)
            .mid_weight(0.0)
            .last_trade_weight(0.0)
            .dampening_factor(0.001) // 0.1% max change
            .build()
            .unwrap();
        let calc = MarkPriceCalculator::new(config);

        // First update: set initial mark to 5 (small price)
        calc.update_index_price(5);
        let mark1 = calc.advance_mark().unwrap();
        assert_eq!(mark1, 5);

        // Second update: try to jump to 10
        // Without ceil fix, max_change = (5 * 0.001) as u64 = 0, mark stuck at 5
        // With ceil fix, max_change = ceil(0.005) = 1, so mark can move to 6
        calc.update_index_price(10);
        let mark2 = calc.advance_mark().unwrap();
        assert_eq!(mark2, 6);
    }

    // ── Read-vs-tick split (issue #57) ───────────────────────────────────

    fn dampening_calc() -> MarkPriceCalculator {
        let config = MarkPriceConfig::builder()
            .index_weight(1.0)
            .mid_weight(0.0)
            .last_trade_weight(0.0)
            .dampening_factor(0.10) // 10% max change per tick
            .build()
            .unwrap();
        MarkPriceCalculator::new(config)
    }

    #[test]
    fn test_current_mark_price_before_tick_returns_none() {
        let calc = MarkPriceCalculator::with_default_config();
        calc.update_index_price(50000);
        // Inputs are set but no tick has committed a mark yet.
        assert!(calc.current_mark_price().is_none());

        let _ = calc.advance_mark();
        assert_eq!(calc.current_mark_price(), Some(50000));
    }

    #[test]
    fn test_current_mark_price_static_inputs_returns_same_value() {
        let calc = dampening_calc();

        // Seed an initial committed mark.
        calc.update_index_price(1000);
        assert_eq!(calc.advance_mark(), Some(1000));

        // Set a far target so a tick would walk the mark by one dampening step.
        calc.update_index_price(2000);
        assert_eq!(calc.advance_mark(), Some(1100));

        // Pure reads must NOT advance dampening: repeated calls with static
        // inputs are idempotent and never walk toward the raw 2000 target.
        assert_eq!(calc.current_mark_price(), Some(1100));
        assert_eq!(calc.current_mark_price(), Some(1100));
        assert_eq!(calc.current_mark_price(), Some(1100));
        // The committed value is unchanged by the reads.
        assert_eq!(calc.last_mark_price(), 1100);
    }

    #[test]
    fn test_advance_mark_advances_once_per_call() {
        let calc = dampening_calc();

        // First commit, no dampening.
        calc.update_index_price(1000);
        assert_eq!(calc.advance_mark(), Some(1000));

        // Far target: each tick moves exactly one dampening step.
        calc.update_index_price(2000);

        // First tick: 1000 + 10% = 1100.
        assert_eq!(calc.advance_mark(), Some(1100));

        // A pure read between ticks must NOT add a step.
        assert_eq!(calc.current_mark_price(), Some(1100));

        // Second tick: 1100 + 10% = 1210. Two ticks → two steps; the
        // intervening read did not contribute a step.
        assert_eq!(calc.advance_mark(), Some(1210));
        assert_eq!(calc.current_mark_price(), Some(1210));
    }

    #[test]
    #[allow(deprecated)]
    fn test_deprecated_mark_price_still_advances() {
        // The deprecated alias must preserve the original mutate-on-read
        // behavior by delegating to advance_mark().
        let calc = dampening_calc();

        calc.update_index_price(1000);
        assert_eq!(calc.mark_price(), Some(1000));

        calc.update_index_price(2000);
        // Still advances one dampening step per call, exactly like advance_mark.
        assert_eq!(calc.mark_price(), Some(1100));
        assert_eq!(calc.current_mark_price(), Some(1100));
    }

    #[test]
    fn test_calculator_reset() {
        let calc = MarkPriceCalculator::with_default_config();

        calc.update_index_price(50000);
        calc.update_mid_price(50100);
        calc.update_last_trade_price(50050);
        let _ = calc.advance_mark();

        calc.reset();

        assert_eq!(calc.index_price(), 0);
        assert_eq!(calc.mid_price(), 0);
        assert_eq!(calc.last_trade_price(), 0);
        assert_eq!(calc.last_mark_price(), 0);
        // Pure read reflects the cleared committed mark.
        assert!(calc.current_mark_price().is_none());
        assert!(calc.advance_mark().is_none());
    }

    #[test]
    fn test_calculator_debug() {
        let calc = MarkPriceCalculator::with_default_config();
        calc.update_index_price(50000);
        let debug_str = format!("{:?}", calc);
        assert!(debug_str.contains("MarkPriceCalculator"));
        assert!(debug_str.contains("50000"));
    }

    #[test]
    fn test_calculator_thread_safety() {
        use std::sync::Arc;
        use std::thread;

        let calc = Arc::new(MarkPriceCalculator::with_default_config());
        let mut handles = vec![];

        // Spawn multiple threads updating prices
        for i in 0..10 {
            let calc_clone = Arc::clone(&calc);
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    let price = (i * 100 + j) as u64 * 100;
                    calc_clone.update_index_price(price);
                    calc_clone.update_mid_price(price);
                    calc_clone.update_last_trade_price(price);
                    let _ = calc_clone.advance_mark();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Should not panic or corrupt data
        let mark = calc.advance_mark();
        assert!(mark.is_some());
    }

    #[test]
    fn test_equal_weights() {
        // Use exact Decimal values that sum to 1:
        // 0.34 + 0.33 + 0.33 = 1.00
        let config = MarkPriceConfig {
            index_weight: dec!(0.34),
            mid_weight: dec!(0.33),
            last_trade_weight: dec!(0.33),
            dampening_factor: dec!(0.01),
        };
        assert!(config.validate().is_ok());
        let calc = MarkPriceCalculator::new(config);

        calc.update_index_price(100);
        calc.update_mid_price(200);
        calc.update_last_trade_price(300);

        let mark = calc.advance_mark().unwrap();
        // Expected: 100*0.34 + 200*0.33 + 300*0.33 = 34 + 66 + 99 = 199
        assert_eq!(mark, 199);
    }

    #[test]
    fn test_zero_weight_ignored() {
        let config = MarkPriceConfig::builder()
            .index_weight(1.0)
            .mid_weight(0.0)
            .last_trade_weight(0.0)
            .build()
            .unwrap();
        let calc = MarkPriceCalculator::new(config);

        calc.update_index_price(1000);
        calc.update_mid_price(5000); // Should be ignored due to 0 weight
        calc.update_last_trade_price(9000); // Should be ignored

        let mark = calc.advance_mark().unwrap();
        assert_eq!(mark, 1000);
    }
}
