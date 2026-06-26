//! Strike generation and cleanup module.
//!
//! This module provides [`StrikeGenerator`] for computing strike prices from a
//! spot price and [`StrikeRangeConfig`], then applying them to an
//! [`OptionChainOrderBook`]. It also provides [`CleanupResult`] and a cleanup
//! method for removing far out-of-the-money (OTM) empty strikes.
//!
//! ## Generation Algorithm
//!
//! 1. Compute ATM strike by rounding spot to nearest interval
//! 2. Compute range bounds: `spot * (1 ± range_pct)`
//! 3. Generate strikes from low to high at interval steps
//! 4. Cap at `max_strikes`
//! 5. Expand symmetrically outward if below `min_strikes`
//!
//! ## Cleanup Algorithm
//!
//! 1. Compute buffered range: `spot * range_pct * buffer_multiplier`
//! 2. For each strike outside the buffered range, remove if empty
//! 3. Strikes with resting orders are never removed
//!
//! ## Example
//!
//! ```
//! use option_chain_orderbook::orderbook::{
//!     OptionChainOrderBook, StrikeGenerator, StrikeRangeConfig,
//! };
//! use optionstratlib::prelude::{ExpirationDate, Positive};
//!
//! let config = StrikeRangeConfig::builder()
//!     .range_pct(0.10)
//!     .strike_interval(1000)
//!     .min_strikes(5)
//!     .max_strikes(50)
//!     .build()
//!     .expect("valid config");
//!
//! let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");
//! assert!(strikes.len() >= 5);
//! assert!(strikes.len() <= 50);
//!
//! // Apply to a chain
//! let chain = OptionChainOrderBook::new("BTC", ExpirationDate::Days(Positive::THIRTY));
//! StrikeGenerator::apply_strikes(&chain, &strikes);
//! assert_eq!(chain.strike_count(), strikes.len());
//! ```

use super::chain::OptionChainOrderBook;
use super::strike_range::StrikeRangeConfig;
use crate::error::{Error, Result};

/// Converts an `f64` value to basis points (`value * 10000`) as `u64`.
///
/// Returns an error if the value is non-finite, negative, or overflows `u64`.
#[inline]
fn f64_to_u64_bps(value: f64, name: &str) -> Result<u64> {
    if !value.is_finite() || value < 0.0 {
        return Err(Error::configuration(format!(
            "{} must be a finite non-negative number, got {}",
            name, value
        )));
    }
    let rounded = (value * 10000.0).round();
    if rounded > u64::MAX as f64 {
        return Err(Error::configuration(format!(
            "{} overflows basis-point conversion: {}",
            name, value
        )));
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    Ok(rounded as u64)
}

/// Zero-sized strike generation utility.
///
/// Provides static methods for computing strike prices from a spot price and
/// configuration, and for applying those strikes to an option chain.
///
/// All arithmetic uses checked operations to prevent overflow on pathological
/// inputs.
pub struct StrikeGenerator;

impl StrikeGenerator {
    /// Computes the ATM-centered strike list for a given spot price and config.
    ///
    /// # Algorithm
    ///
    /// 1. Round `spot` to nearest `strike_interval` to get ATM
    /// 2. Compute range bounds: `spot * (1 ± range_pct)`
    /// 3. Floor/ceil bounds to interval multiples
    /// 4. Generate strikes from low to high, capped at `max_strikes`
    /// 5. If count < `min_strikes`, expand symmetrically outward
    ///
    /// # Arguments
    ///
    /// * `spot` - Current spot price in price units (e.g., 50000 for $50,000)
    /// * `config` - Strike range configuration
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - `spot` is zero
    /// - Arithmetic overflow occurs (pathological inputs)
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{StrikeGenerator, StrikeRangeConfig};
    ///
    /// let config = StrikeRangeConfig::builder()
    ///     .range_pct(0.10)
    ///     .strike_interval(1000)
    ///     .min_strikes(5)
    ///     .max_strikes(50)
    ///     .build()
    ///     .expect("valid config");
    ///
    /// let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");
    /// // With 10% range on 50000, we get 45000..55000, so 11 strikes at 1000 interval
    /// assert_eq!(strikes.len(), 11);
    /// ```
    pub fn generate_strikes(spot: u64, config: &StrikeRangeConfig) -> Result<Vec<u64>> {
        config.validate()?;

        if spot == 0 {
            return Err(Error::configuration("spot price must be positive"));
        }

        let interval = config.strike_interval();
        let range_pct = config.range_pct();
        let min_strikes = config.min_strikes();
        let max_strikes = config.max_strikes();

        // Compute ATM: round spot to nearest interval
        let half_interval = interval / 2;
        let atm = spot
            .checked_add(half_interval)
            .ok_or_else(|| Error::configuration("overflow computing ATM"))?
            / interval
            * interval;

        // Compute range bounds using integer arithmetic for deterministic behavior.
        // Convert range_pct to basis points (multiply by 10000) to avoid f64 jitter.
        // range = spot * range_pct = spot * (range_pct_bps / 10000)
        let range_pct_bps = f64_to_u64_bps(range_pct, "range_pct")?;
        let range = spot
            .checked_mul(range_pct_bps)
            .ok_or_else(|| Error::configuration("overflow computing range"))?
            / 10000;

        // low = spot - range, high = spot + range
        let low = spot.saturating_sub(range);
        let high = spot
            .checked_add(range)
            .ok_or_else(|| Error::configuration("overflow computing high bound"))?;

        // Floor low to interval multiple
        let low_strike = (low / interval) * interval;

        // Ceil high to interval multiple: ((high + interval - 1) / interval) * interval
        let high_strike = high
            .checked_add(interval.saturating_sub(1))
            .ok_or_else(|| Error::configuration("overflow computing high_strike"))?
            / interval
            * interval;

        // Generate strikes from low to high
        let mut strikes = Vec::new();
        let mut strike = low_strike;

        // Ensure we start at a valid strike (at least interval)
        if strike == 0 {
            strike = interval;
        }

        while strike <= high_strike {
            strikes.push(strike);
            strike = match strike.checked_add(interval) {
                Some(s) => s,
                None => break, // Overflow, stop generating
            };
        }

        // Cap at max_strikes by selecting ATM-centered window
        if strikes.len() > max_strikes {
            // Find ATM index or closest strike
            let atm_idx = strikes
                .iter()
                .position(|&s| s >= atm)
                .unwrap_or(strikes.len().saturating_sub(1));

            // Select centered window around ATM
            let half = max_strikes / 2;
            let start = atm_idx.saturating_sub(half);
            let end = (start + max_strikes).min(strikes.len());
            let start = end.saturating_sub(max_strikes);

            strikes = strikes
                .get(start..end)
                .map(<[u64]>::to_vec)
                .unwrap_or_default();
        }

        // Expand symmetrically if below min_strikes
        if strikes.len() < min_strikes && !strikes.is_empty() {
            let mut current_low = *strikes.first().unwrap_or(&atm);
            let mut current_high = *strikes.last().unwrap_or(&atm);
            let mut expand_low = true;

            while strikes.len() < min_strikes {
                if expand_low {
                    // Try to expand below
                    if let Some(new_low) = current_low.checked_sub(interval)
                        && new_low >= interval
                    {
                        strikes.insert(0, new_low);
                        current_low = new_low;
                    }
                    expand_low = false;
                } else {
                    // Try to expand above
                    if let Some(new_high) = current_high.checked_add(interval) {
                        strikes.push(new_high);
                        current_high = new_high;
                    } else {
                        // Can't expand above, stop to avoid infinite loop
                        break;
                    }
                    expand_low = true;
                }

                // Safety: if we can't expand in either direction, break
                if strikes.len() < min_strikes {
                    let can_expand_low = current_low
                        .checked_sub(interval)
                        .map(|l| l >= interval)
                        .unwrap_or(false);
                    let can_expand_high = current_high.checked_add(interval).is_some();
                    if !can_expand_low && !can_expand_high {
                        break;
                    }
                }
            }
        }

        // Ensure strikes are sorted (they should be, but defensive)
        strikes.sort_unstable();

        Ok(strikes)
    }

    /// Creates `StrikeOrderBook` entries for each strike (idempotent).
    ///
    /// Calls [`OptionChainOrderBook::get_or_create_strike`] for each strike
    /// in the slice. If a strike already exists, this is a no-op for that strike.
    ///
    /// # Arguments
    ///
    /// * `chain` - The option chain to populate with strikes
    /// * `strikes` - Slice of strike prices to create
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{OptionChainOrderBook, StrikeGenerator};
    /// use optionstratlib::prelude::{ExpirationDate, Positive};
    ///
    /// let chain = OptionChainOrderBook::new("BTC", ExpirationDate::Days(Positive::THIRTY));
    /// let strikes = vec![45000, 50000, 55000];
    ///
    /// StrikeGenerator::apply_strikes(&chain, &strikes);
    /// assert_eq!(chain.strike_count(), 3);
    ///
    /// // Idempotent: calling again doesn't change count
    /// StrikeGenerator::apply_strikes(&chain, &strikes);
    /// assert_eq!(chain.strike_count(), 3);
    /// ```
    pub fn apply_strikes(chain: &OptionChainOrderBook, strikes: &[u64]) {
        for &strike in strikes {
            let _ = chain.get_or_create_strike(strike);
        }
    }

    /// Combines strike generation and application in one call.
    ///
    /// This is equivalent to calling [`generate_strikes`](Self::generate_strikes)
    /// followed by [`apply_strikes`](Self::apply_strikes).
    ///
    /// # Arguments
    ///
    /// * `chain` - The option chain to populate with strikes
    /// * `spot` - Current spot price
    /// * `config` - Strike range configuration
    ///
    /// # Returns
    ///
    /// The generated strike prices that were applied to the chain.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if strike generation fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{
    ///     OptionChainOrderBook, StrikeGenerator, StrikeRangeConfig,
    /// };
    /// use optionstratlib::prelude::{ExpirationDate, Positive};
    ///
    /// let chain = OptionChainOrderBook::new("BTC", ExpirationDate::Days(Positive::THIRTY));
    /// let config = StrikeRangeConfig::builder()
    ///     .range_pct(0.10)
    ///     .strike_interval(1000)
    ///     .min_strikes(5)
    ///     .max_strikes(50)
    ///     .build()
    ///     .expect("valid config");
    ///
    /// let strikes = StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
    /// assert_eq!(chain.strike_count(), strikes.len());
    /// ```
    pub fn refresh_strikes(
        chain: &OptionChainOrderBook,
        spot: u64,
        config: &StrikeRangeConfig,
    ) -> Result<Vec<u64>> {
        let strikes = Self::generate_strikes(spot, config)?;
        Self::apply_strikes(chain, &strikes);
        Ok(strikes)
    }

    /// Removes empty strikes outside a buffered range around the current spot.
    ///
    /// Only strikes with **zero resting orders** (both call and put empty) are
    /// removed. Strikes that have any resting orders are never removed,
    /// regardless of how far they are from the current spot.
    ///
    /// The buffer multiplier widens the keep-range beyond the generation range
    /// to prevent aggressive cleanup near boundaries. A value of `1.5` means
    /// the cleanup range is 50% wider than the generation range.
    ///
    /// # Algorithm
    ///
    /// 1. Compute buffered range: `spot * range_pct * buffer_multiplier`
    /// 2. Compute bounds: `[spot - range, spot + range]`
    /// 3. For each strike outside bounds, remove if empty
    ///
    /// # Arguments
    ///
    /// * `chain` - The option chain to clean up
    /// * `spot` - Current spot price in price units
    /// * `config` - Strike range configuration (uses `range_pct`)
    /// * `buffer_multiplier` - Multiplier on the range (must be ≥ 1.0)
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - `spot` is zero
    /// - `buffer_multiplier` is not finite or less than 1.0
    /// - `config` validation fails
    /// - computing the keep-range overflows `u64` (`spot * range_pct_bps * buffer_bps`)
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{
    ///     OptionChainOrderBook, StrikeGenerator, StrikeRangeConfig,
    /// };
    /// use optionstratlib::prelude::{ExpirationDate, Positive};
    ///
    /// let chain = OptionChainOrderBook::new("BTC", ExpirationDate::Days(Positive::THIRTY));
    /// let config = StrikeRangeConfig::builder()
    ///     .range_pct(0.10)
    ///     .strike_interval(1000)
    ///     .min_strikes(5)
    ///     .max_strikes(50)
    ///     .build()
    ///     .expect("valid config");
    ///
    /// // Generate strikes at spot=50000, then cleanup at spot=60000 with 1.5x buffer
    /// StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
    /// let result = StrikeGenerator::cleanup_empty_strikes(&chain, 60000, &config, 1.5)
    ///     .expect("ok");
    /// ```
    pub fn cleanup_empty_strikes(
        chain: &OptionChainOrderBook,
        spot: u64,
        config: &StrikeRangeConfig,
        buffer_multiplier: f64,
    ) -> Result<CleanupResult> {
        config.validate()?;

        if spot == 0 {
            return Err(Error::configuration("spot price must be positive"));
        }

        if !buffer_multiplier.is_finite() {
            return Err(Error::configuration("buffer_multiplier must be finite"));
        }

        if buffer_multiplier < 1.0 {
            return Err(Error::configuration(
                "buffer_multiplier must be at least 1.0",
            ));
        }

        // Compute buffered range using integer arithmetic for deterministic behavior.
        // Convert range_pct to basis points and apply buffer_multiplier.
        // range = spot * range_pct * buffer_multiplier
        //       = spot * (range_pct_bps / 10000) * buffer_multiplier
        let range_pct = config.range_pct();
        let range_pct_bps = f64_to_u64_bps(range_pct, "range_pct")?;
        let buffer_bps = f64_to_u64_bps(buffer_multiplier, "buffer_multiplier")?;

        // Compute: spot * range_pct_bps * buffer_bps / 10000 / 10000.
        // Mirror generate_strikes: surface overflow as a typed error rather than
        // saturating to a huge wrong keep-range (which would feed a silently-wrong
        // CleanupResult into the sequencer / NATS).
        let range = spot
            .checked_mul(range_pct_bps)
            .and_then(|v| v.checked_mul(buffer_bps))
            .ok_or_else(|| Error::configuration("overflow computing cleanup range"))?
            / 10000
            / 10000;

        let interval = config.strike_interval();

        // First compute the raw integer bounds around spot
        let raw_low = spot.saturating_sub(range);
        let raw_high = spot.saturating_add(range);

        // Floor low to the nearest strike interval
        let low = (raw_low / interval) * interval;

        // Ceil high to the nearest strike interval
        let rem = raw_high % interval;
        let high = if rem == 0 {
            raw_high
        } else {
            raw_high.saturating_add(interval.saturating_sub(rem))
        };

        let mut result = CleanupResult::default();

        for strike_price in chain.strike_prices() {
            // Skip strikes inside the buffered range
            if strike_price >= low && strike_price <= high {
                continue;
            }

            // Strike is outside the buffered range — atomically remove if empty.
            // This avoids the TOCTOU race condition that would occur if we
            // checked is_empty() and then called remove() separately.
            if chain.strikes().remove_if_empty(strike_price) {
                result.removed.push(strike_price);
            } else if chain.get_strike(strike_price).is_ok() {
                // Strike exists but has orders — count as skipped
                result.skipped_with_orders = result.skipped_with_orders.saturating_add(1);
            }
        }

        Ok(result)
    }
}

// ─── CleanupResult ────────────────────────────────────────────────────────────

/// Result of a strike cleanup operation.
///
/// Contains the list of removed strike prices and the count of strikes that
/// were outside the range but had resting orders (and were therefore skipped).
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::CleanupResult;
///
/// let result = CleanupResult::default();
/// assert!(result.removed.is_empty());
/// assert_eq!(result.skipped_with_orders, 0);
/// ```
#[derive(Debug, Clone, Default)]
pub struct CleanupResult {
    /// Strike prices that were removed.
    pub removed: Vec<u64>,
    /// Number of strikes outside range that had orders and were skipped.
    pub skipped_with_orders: usize,
}

impl CleanupResult {
    /// Returns true if no strikes were removed.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty()
    }

    /// Returns the number of strikes removed.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.removed.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optionstratlib::prelude::{ExpirationDate, Positive};
    use orderbook_rs::{OrderId, Side};

    fn test_expiration() -> ExpirationDate {
        ExpirationDate::Days(Positive::THIRTY)
    }

    fn default_config() -> StrikeRangeConfig {
        StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(1000)
            .min_strikes(5)
            .max_strikes(50)
            .build()
            .expect("valid config")
    }

    // ── generate_strikes basic ─────────────────────────────────────────────────

    #[test]
    fn test_generate_strikes_basic() {
        let config = default_config();
        let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");

        // 10% of 50000 = 5000, so range is 45000..55000
        // At 1000 interval: 45000, 46000, ..., 55000 = 11 strikes
        assert_eq!(strikes.len(), 11);
        assert_eq!(strikes[0], 45000);
        assert_eq!(strikes[10], 55000);
    }

    #[test]
    fn test_generate_strikes_spot_on_interval() {
        let config = default_config();
        let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");

        // ATM should be 50000 (already on interval)
        assert!(strikes.contains(&50000));
    }

    #[test]
    fn test_generate_strikes_spot_between_intervals() {
        let config = default_config();
        let strikes = StrikeGenerator::generate_strikes(50500, &config).expect("ok");

        // ATM should round to 51000 (50500 + 500 = 51000)
        // Range: 45450..55550 → strikes 45000..56000
        assert!(strikes.contains(&51000));
    }

    #[test]
    fn test_generate_strikes_sorted() {
        let config = default_config();
        let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");

        let mut sorted = strikes.clone();
        sorted.sort_unstable();
        assert_eq!(strikes, sorted);
    }

    // ── generate_strikes edge cases ────────────────────────────────────────────

    #[test]
    fn test_generate_strikes_zero_spot_error() {
        let config = default_config();
        let result = StrikeGenerator::generate_strikes(0, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("positive"));
    }

    #[test]
    fn test_generate_strikes_small_spot() {
        // Spot = 500, interval = 1000 → low range would be negative
        let config = StrikeRangeConfig::builder()
            .range_pct(0.50)
            .strike_interval(1000)
            .min_strikes(3)
            .max_strikes(50)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(500, &config).expect("ok");

        // Should handle gracefully, starting at interval (1000)
        assert!(!strikes.is_empty());
        assert!(strikes[0] >= 1000);
    }

    #[test]
    fn test_generate_strikes_very_small_spot() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(100)
            .min_strikes(3)
            .max_strikes(50)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(50, &config).expect("ok");

        // Should generate at least min_strikes via expansion
        assert!(strikes.len() >= 3);
    }

    // ── min_strikes expansion ──────────────────────────────────────────────────

    #[test]
    fn test_generate_strikes_min_strikes_expansion() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.01) // Very small range
            .strike_interval(1000)
            .min_strikes(5)
            .max_strikes(50)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");

        // 1% of 50000 = 500, so initial range is 49500..50500
        // That's only 50000 (1 strike) initially
        // Should expand to at least 5 strikes
        assert!(strikes.len() >= 5);
    }

    #[test]
    fn test_generate_strikes_min_strikes_symmetric() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.01)
            .strike_interval(1000)
            .min_strikes(5)
            .max_strikes(50)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");

        // Should be symmetric around ATM (50000)
        let atm_idx = strikes.iter().position(|&s| s == 50000);
        assert!(atm_idx.is_some());
    }

    // ── max_strikes cap ────────────────────────────────────────────────────────

    #[test]
    fn test_generate_strikes_max_strikes_cap() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.50) // Large range
            .strike_interval(1000)
            .min_strikes(5)
            .max_strikes(10)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(50000, &config).expect("ok");

        // Should be capped at max_strikes
        assert!(strikes.len() <= 10);
    }

    // ── apply_strikes ──────────────────────────────────────────────────────────

    #[test]
    fn test_apply_strikes_creates_strikes() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let strikes = vec![45000, 50000, 55000];

        StrikeGenerator::apply_strikes(&chain, &strikes);

        assert_eq!(chain.strike_count(), 3);
        assert!(chain.get_strike(45000).is_ok());
        assert!(chain.get_strike(50000).is_ok());
        assert!(chain.get_strike(55000).is_ok());
    }

    #[test]
    fn test_apply_strikes_idempotent() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let strikes = vec![45000, 50000, 55000];

        StrikeGenerator::apply_strikes(&chain, &strikes);
        assert_eq!(chain.strike_count(), 3);

        // Apply again - should not change count
        StrikeGenerator::apply_strikes(&chain, &strikes);
        assert_eq!(chain.strike_count(), 3);
    }

    #[test]
    fn test_apply_strikes_empty() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let strikes: Vec<u64> = vec![];

        StrikeGenerator::apply_strikes(&chain, &strikes);
        assert_eq!(chain.strike_count(), 0);
    }

    // ── refresh_strikes ────────────────────────────────────────────────────────

    #[test]
    fn test_refresh_strikes_integration() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        let strikes = StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");

        assert_eq!(chain.strike_count(), strikes.len());
        for &strike in &strikes {
            assert!(chain.get_strike(strike).is_ok());
        }
    }

    #[test]
    fn test_refresh_strikes_idempotent() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        let strikes1 = StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
        let count1 = chain.strike_count();

        let strikes2 = StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
        let count2 = chain.strike_count();

        assert_eq!(strikes1, strikes2);
        assert_eq!(count1, count2);
    }

    // ── ATM rounding ───────────────────────────────────────────────────────────

    #[test]
    fn test_atm_rounding_down() {
        // 50400 + 500 = 50900, /1000 = 50, *1000 = 50000
        let config = StrikeRangeConfig::builder()
            .range_pct(0.001) // Tiny range to isolate ATM
            .strike_interval(1000)
            .min_strikes(1)
            .max_strikes(50)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(50400, &config).expect("ok");
        assert!(strikes.contains(&50000));
    }

    #[test]
    fn test_atm_rounding_up() {
        // 50600 + 500 = 51100, /1000 = 51, *1000 = 51000
        let config = StrikeRangeConfig::builder()
            .range_pct(0.001)
            .strike_interval(1000)
            .min_strikes(1)
            .max_strikes(50)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(50600, &config).expect("ok");
        assert!(strikes.contains(&51000));
    }

    // ── Large spot ─────────────────────────────────────────────────────────────

    #[test]
    fn test_generate_strikes_large_spot() {
        let config = StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(1_000_000)
            .min_strikes(5)
            .max_strikes(50)
            .build()
            .expect("valid");

        // Large spot but not near u64 max
        let strikes = StrikeGenerator::generate_strikes(100_000_000, &config).expect("ok");
        assert!(!strikes.is_empty());
    }

    // ── Different intervals ────────────────────────────────────────────────────

    #[test]
    fn test_generate_strikes_eth_style_interval() {
        // ETH uses $50 intervals
        let config = StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(50)
            .min_strikes(5)
            .max_strikes(100)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(3500, &config).expect("ok");

        // 10% of 3500 = 350, range 3150..3850
        // At 50 interval that's many strikes
        assert!(strikes.len() >= 14); // (3850-3150)/50 + 1 = 15
        for &s in &strikes {
            assert_eq!(s % 50, 0);
        }
    }

    #[test]
    fn test_generate_strikes_btc_style_interval() {
        // BTC uses $1000 intervals
        let config = StrikeRangeConfig::builder()
            .range_pct(0.20)
            .strike_interval(1000)
            .min_strikes(5)
            .max_strikes(100)
            .build()
            .expect("valid");

        let strikes = StrikeGenerator::generate_strikes(65000, &config).expect("ok");

        // 20% of 65000 = 13000, range 52000..78000
        // At 1000 interval that's 27 strikes
        for &s in &strikes {
            assert_eq!(s % 1000, 0);
        }
    }

    // ── cleanup_empty_strikes ──────────────────────────────────────────────────

    #[test]
    fn test_cleanup_removes_empty_strikes() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config(); // 10% range, 1000 interval

        // Generate strikes at spot=50000: 45000..55000
        StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
        let initial_count = chain.strike_count();
        assert!(initial_count > 0);

        // Move spot to 70000 with buffer=1.0 (exact range)
        // Keep range: 70000 * 0.10 * 1.0 = 7000 → [63000, 77000]
        // All strikes 45000..55000 are below 63000, so all should be removed
        let result =
            StrikeGenerator::cleanup_empty_strikes(&chain, 70000, &config, 1.0).expect("ok");

        assert_eq!(result.len(), initial_count);
        assert_eq!(result.skipped_with_orders, 0);
        assert_eq!(chain.strike_count(), 0);
    }

    #[test]
    fn test_cleanup_skips_strikes_with_orders() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        // Generate strikes at spot=50000: 45000..55000
        StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
        let initial_count = chain.strike_count();

        // Add an order to the 45000 strike (far OTM when spot moves)
        let strike_45k = chain.get_strike(45000).expect("strike exists");
        strike_45k
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("order added");

        // Move spot to 70000 — all old strikes outside range
        let result =
            StrikeGenerator::cleanup_empty_strikes(&chain, 70000, &config, 1.0).expect("ok");

        // 45000 should be skipped (has orders), rest removed
        assert_eq!(result.skipped_with_orders, 1);
        assert_eq!(
            result.len(),
            initial_count.checked_sub(1).expect("at least 1")
        );
        // 45000 should still exist
        assert!(chain.get_strike(45000).is_ok());
    }

    #[test]
    fn test_cleanup_buffer_prevents_aggressive_removal() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config(); // 10% range

        // Generate strikes at spot=50000: 45000..55000
        StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");

        // Move spot to 55000 with buffer=1.5
        // Raw range: 55000 * 0.10 * 1.5 = 8250 → raw bounds [46750, 63250]
        // Aligned to interval: floor(46750/1000)*1000 = 46000, ceil(63250) = 64000
        // Keep range: [46000, 64000]
        // Strikes below 46000 should be removed (45000)
        // Strikes 46000..55000 should be kept
        let result =
            StrikeGenerator::cleanup_empty_strikes(&chain, 55000, &config, 1.5).expect("ok");

        // 45000 is below 46000, so removed
        assert!(result.removed.contains(&45000));

        // 46000 should still exist (at aligned boundary)
        assert!(chain.get_strike(46000).is_ok());

        // 47000 should still exist (inside buffer)
        assert!(chain.get_strike(47000).is_ok());

        // 50000 should still exist
        assert!(chain.get_strike(50000).is_ok());
    }

    #[test]
    fn test_cleanup_no_removals_when_all_in_range() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        // Generate strikes at spot=50000
        StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
        let initial_count = chain.strike_count();

        // Cleanup at same spot with buffer=1.5 — everything stays
        let result =
            StrikeGenerator::cleanup_empty_strikes(&chain, 50000, &config, 1.5).expect("ok");

        assert!(result.is_empty());
        assert_eq!(result.skipped_with_orders, 0);
        assert_eq!(chain.strike_count(), initial_count);
    }

    #[test]
    fn test_cleanup_preserves_high_strike_for_non_aligned_spot() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        // Use a spot that is not aligned to the strike interval (50_123 with 1_000 interval)
        let spot = 50_123u64;

        // Generate and apply strikes for the non-aligned spot
        StrikeGenerator::refresh_strikes(&chain, spot, &config).expect("ok");

        // Identify the highest strike produced by generate_strikes for this spot
        let strikes = StrikeGenerator::generate_strikes(spot, &config).expect("ok");
        let high_strike = *strikes.iter().max().expect("non-empty strikes");

        // Cleanup at the same spot with buffer=1.0 — the high_strike should not be removed
        let result =
            StrikeGenerator::cleanup_empty_strikes(&chain, spot, &config, 1.0).expect("ok");

        // Ensure that the high_strike was not removed by cleanup logic
        assert!(
            !result.removed.contains(&high_strike),
            "high_strike {} should not be removed when cleaning up at the same non-aligned spot",
            high_strike
        );
        assert!(
            chain.get_strike(high_strike).is_ok(),
            "high_strike {} should still exist in the chain after cleanup",
            high_strike
        );
    }

    #[test]
    fn test_cleanup_empty_chain() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        let result =
            StrikeGenerator::cleanup_empty_strikes(&chain, 50000, &config, 1.0).expect("ok");

        assert!(result.is_empty());
        assert_eq!(result.skipped_with_orders, 0);
    }

    #[test]
    fn test_cleanup_invalid_spot_zero() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        let result = StrikeGenerator::cleanup_empty_strikes(&chain, 0, &config, 1.5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("positive"));
    }

    #[test]
    fn test_cleanup_range_overflow_returns_error() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        // A spot near u64::MAX overflows spot * range_pct_bps * buffer_bps; the
        // keep-range must surface a typed configuration error, not saturate to a
        // huge wrong value that feeds a silently-wrong CleanupResult.
        let result = StrikeGenerator::cleanup_empty_strikes(&chain, u64::MAX, &config, 1.5);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("overflow"),
            "expected an overflow error"
        );
    }

    #[test]
    fn test_cleanup_invalid_buffer_below_one() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        let result = StrikeGenerator::cleanup_empty_strikes(&chain, 50000, &config, 0.5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("at least 1.0"));
    }

    #[test]
    fn test_cleanup_invalid_buffer_nan() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        let result = StrikeGenerator::cleanup_empty_strikes(&chain, 50000, &config, f64::NAN);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("finite"));
    }

    #[test]
    fn test_cleanup_invalid_buffer_infinity() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config();

        let result = StrikeGenerator::cleanup_empty_strikes(&chain, 50000, &config, f64::INFINITY);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("finite"));
    }

    #[test]
    fn test_cleanup_result_default() {
        let result = CleanupResult::default();
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert_eq!(result.skipped_with_orders, 0);
    }

    #[test]
    fn test_cleanup_integration_generate_move_cleanup() {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let config = default_config(); // 10% range, 1000 interval

        // Step 1: Generate strikes at spot=50000 → 45000..55000 (11 strikes)
        let initial = StrikeGenerator::refresh_strikes(&chain, 50000, &config).expect("ok");
        assert_eq!(initial.len(), 11);
        assert_eq!(chain.strike_count(), 11);

        // Step 2: Spot moves to 60000 — generate new strikes 54000..66000
        let new = StrikeGenerator::refresh_strikes(&chain, 60000, &config).expect("ok");
        assert!(!new.is_empty());
        // Chain now has old + new strikes (some overlap around 54000-55000)
        let count_before_cleanup = chain.strike_count();
        assert!(count_before_cleanup > 11);

        // Step 3: Cleanup with buffer=1.0 at spot=60000
        // Keep range: 60000 * 0.10 * 1.0 = 6000 → [54000, 66000]
        // Strikes below 54000 should be removed (45000..53000)
        let result =
            StrikeGenerator::cleanup_empty_strikes(&chain, 60000, &config, 1.0).expect("ok");

        assert!(!result.is_empty());
        // All removed strikes should be below 54000
        for &s in &result.removed {
            assert!(s < 54000, "removed strike {} should be below 54000", s);
        }
        // Verify remaining strikes are all in range
        for s in chain.strike_prices() {
            assert!(
                (54000..=66000).contains(&s),
                "remaining strike {} should be in [54000, 66000]",
                s
            );
        }
    }
}
