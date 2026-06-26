//! Greeks aggregation module.
//!
//! This module provides [`GreeksAggregator`] for summing Greeks across multiple
//! positions, with per-account and per-underlying views. Positions carry a
//! signed quantity (positive = long, negative = short) and the instrument's
//! current [`Greek`] values from `optionstratlib`.
//!
//! ## Overview
//!
//! - [`Position`]: A single instrument holding with quantity and Greeks.
//! - [`AggregatedGreeks`]: Quantity-weighted sum of all 12 Greek fields.
//! - [`GreeksAggregator`]: Thread-safe, `DashMap`-backed store keyed by account.
//!
//! ## Sign Convention
//!
//! Long positions use positive quantities; short positions use negative
//! quantities. The aggregated value for each Greek is `Σ (quantity × greek)`.
//!
//! ## Example
//!
//! ```
//! use option_chain_orderbook::orderbook::greeks_aggregator::{
//!     AggregatedGreeks, GreeksAggregator, Position,
//! };
//! use optionstratlib::greeks::Greek;
//! use rust_decimal::Decimal;
//!
//! let agg = GreeksAggregator::new();
//!
//! let greeks = Greek {
//!     delta: Decimal::new(50, 2),   // 0.50
//!     gamma: Decimal::new(2, 2),    // 0.02
//!     theta: Decimal::new(-5, 2),   // -0.05
//!     vega: Decimal::new(20, 2),    // 0.20
//!     rho: Decimal::new(1, 2),      // 0.01
//!     rho_d: Decimal::ZERO,
//!     alpha: Decimal::ZERO,
//!     vanna: Decimal::ZERO,
//!     vomma: Decimal::ZERO,
//!     veta: Decimal::ZERO,
//!     charm: Decimal::ZERO,
//!     color: Decimal::ZERO,
//! };
//!
//! let pos = Position::new("BTC-20260130-50000-C", "BTC", 10, greeks);
//! agg.add_position("account-1", pos);
//!
//! let result = agg.aggregate_by_account("account-1");
//! assert_eq!(result.position_count, 1);
//! ```

use dashmap::DashMap;
use optionstratlib::greeks::Greek;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Quantity-weighted sum of Greeks across multiple positions.
///
/// All 12 Greek fields from [`optionstratlib::greeks::Greek`] are aggregated.
/// Each field value equals `Σ (position_quantity × instrument_greek)`.
///
/// ## Fields
///
/// All numeric fields are [`Decimal`] for precise arithmetic.
/// `position_count` tracks how many positions contributed to the sum.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregatedGreeks {
    /// Net delta exposure (sensitivity to underlying price).
    pub delta: Decimal,
    /// Net gamma exposure (rate of change of delta).
    pub gamma: Decimal,
    /// Net theta exposure (time decay).
    pub theta: Decimal,
    /// Net vega exposure (sensitivity to implied volatility).
    pub vega: Decimal,
    /// Net rho exposure (sensitivity to risk-free interest rate).
    pub rho: Decimal,
    /// Net rho_d exposure (sensitivity to dividend yield).
    pub rho_d: Decimal,
    /// Net alpha exposure (unexplained theoretical value).
    pub alpha: Decimal,
    /// Net vanna exposure (delta sensitivity to volatility).
    pub vanna: Decimal,
    /// Net vomma exposure (vega sensitivity to volatility).
    pub vomma: Decimal,
    /// Net veta exposure (vega sensitivity to time).
    pub veta: Decimal,
    /// Net charm exposure (delta sensitivity to time).
    pub charm: Decimal,
    /// Net color exposure (gamma sensitivity to time).
    pub color: Decimal,
    /// Number of positions aggregated.
    pub position_count: usize,
    /// True if any arithmetic operation saturated to MAX/MIN during aggregation.
    ///
    /// When this flag is set, the aggregated values may be inaccurate and should
    /// be treated as indicative only. Operators should investigate the cause.
    pub saturated: bool,
}

impl AggregatedGreeks {
    /// Adds a single position's contribution to this aggregate.
    ///
    /// Each Greek field is multiplied by `quantity` and added with
    /// checked arithmetic. Overflows saturate to `Decimal::MAX` /
    /// `Decimal::MIN` to avoid panics, and the `saturated` flag is set.
    #[inline]
    fn accumulate(&mut self, greeks: &Greek, quantity: Decimal) {
        let (delta, sat1) = checked_mul_add_with_flag(self.delta, greeks.delta, quantity);
        let (gamma, sat2) = checked_mul_add_with_flag(self.gamma, greeks.gamma, quantity);
        let (theta, sat3) = checked_mul_add_with_flag(self.theta, greeks.theta, quantity);
        let (vega, sat4) = checked_mul_add_with_flag(self.vega, greeks.vega, quantity);
        let (rho, sat5) = checked_mul_add_with_flag(self.rho, greeks.rho, quantity);
        let (rho_d, sat6) = checked_mul_add_with_flag(self.rho_d, greeks.rho_d, quantity);
        let (alpha, sat7) = checked_mul_add_with_flag(self.alpha, greeks.alpha, quantity);
        let (vanna, sat8) = checked_mul_add_with_flag(self.vanna, greeks.vanna, quantity);
        let (vomma, sat9) = checked_mul_add_with_flag(self.vomma, greeks.vomma, quantity);
        let (veta, sat10) = checked_mul_add_with_flag(self.veta, greeks.veta, quantity);
        let (charm, sat11) = checked_mul_add_with_flag(self.charm, greeks.charm, quantity);
        let (color, sat12) = checked_mul_add_with_flag(self.color, greeks.color, quantity);

        self.delta = delta;
        self.gamma = gamma;
        self.theta = theta;
        self.vega = vega;
        self.rho = rho;
        self.rho_d = rho_d;
        self.alpha = alpha;
        self.vanna = vanna;
        self.vomma = vomma;
        self.veta = veta;
        self.charm = charm;
        self.color = color;
        self.position_count = self.position_count.saturating_add(1);
        let new_saturation = sat1
            || sat2
            || sat3
            || sat4
            || sat5
            || sat6
            || sat7
            || sat8
            || sat9
            || sat10
            || sat11
            || sat12;
        if new_saturation && !self.saturated {
            tracing::warn!(
                position_count = self.position_count,
                "greeks aggregation saturated to Decimal::MAX/MIN — aggregate values are indicative only"
            );
        }
        self.saturated = self.saturated || new_saturation;
    }
}

/// A single instrument position with Greeks and signed quantity.
///
/// ## Sign Convention
///
/// - Positive `quantity` represents a long position.
/// - Negative `quantity` represents a short position.
///
/// The aggregator multiplies each Greek by the quantity, so shorts
/// naturally produce negative contributions.
///
/// ## Quantity Bounds
///
/// The `quantity` field is `i64`, which is converted to `Decimal` during
/// aggregation via `Decimal::from(i64)`. This conversion is safe for all
/// `i64` values since `Decimal` can represent the full `i64` range without
/// overflow. However, the subsequent multiplication with Greek values uses
/// checked arithmetic and may saturate if the product exceeds `Decimal::MAX`.
/// Callers should ensure quantities stay within reasonable trading bounds
/// (e.g., ±1 billion contracts) to avoid saturation during aggregation.
#[derive(Debug, Clone)]
pub struct Position {
    /// Option symbol (e.g., `"BTC-20260130-50000-C"`).
    instrument_symbol: String,
    /// Underlying asset (e.g., `"BTC"`).
    underlying: String,
    /// Signed quantity: positive = long, negative = short.
    quantity: i64,
    /// Per-instrument Greeks from `optionstratlib`.
    greeks: Greek,
}

impl Position {
    /// Creates a new position.
    ///
    /// # Arguments
    ///
    /// * `instrument_symbol` - Option symbol (e.g., `"BTC-20260130-50000-C"`)
    /// * `underlying` - Underlying asset (e.g., `"BTC"`)
    /// * `quantity` - Signed quantity (positive = long, negative = short)
    /// * `greeks` - Per-instrument Greeks
    #[must_use]
    pub fn new(
        instrument_symbol: impl Into<String>,
        underlying: impl Into<String>,
        quantity: i64,
        greeks: Greek,
    ) -> Self {
        Self {
            instrument_symbol: instrument_symbol.into(),
            underlying: underlying.into(),
            quantity,
            greeks,
        }
    }

    /// Returns the instrument symbol.
    #[must_use]
    #[inline]
    pub fn instrument_symbol(&self) -> &str {
        &self.instrument_symbol
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    #[inline]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the signed quantity.
    #[must_use]
    #[inline]
    pub const fn quantity(&self) -> i64 {
        self.quantity
    }

    /// Returns a reference to the per-instrument Greeks.
    #[must_use]
    #[inline]
    pub const fn greeks(&self) -> &Greek {
        &self.greeks
    }
}

/// Thread-safe Greeks aggregator keyed by account.
///
/// Stores positions per account in a [`DashMap`] for concurrent access.
/// Provides aggregation by account, by underlying (cross-account), and
/// a global aggregate across all positions.
///
/// ## Thread Safety
///
/// All methods are safe to call from any thread. The [`DashMap`] provides
/// fine-grained sharded locking for concurrent reads and writes.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::greeks_aggregator::{
///     GreeksAggregator, Position,
/// };
/// use optionstratlib::greeks::Greek;
/// use rust_decimal::Decimal;
///
/// let agg = GreeksAggregator::new();
///
/// let greeks = Greek {
///     delta: Decimal::ONE,
///     gamma: Decimal::ZERO,
///     theta: Decimal::ZERO,
///     vega: Decimal::ZERO,
///     rho: Decimal::ZERO,
///     rho_d: Decimal::ZERO,
///     alpha: Decimal::ZERO,
///     vanna: Decimal::ZERO,
///     vomma: Decimal::ZERO,
///     veta: Decimal::ZERO,
///     charm: Decimal::ZERO,
///     color: Decimal::ZERO,
/// };
///
/// agg.add_position("alice", Position::new("BTC-20260130-50000-C", "BTC", 5, greeks));
/// let result = agg.aggregate_by_account("alice");
/// assert_eq!(result.delta, Decimal::new(5, 0));
/// ```
pub struct GreeksAggregator {
    /// Positions keyed by account identifier, with inner `HashMap` keyed by
    /// `instrument_symbol` for O(1) lookups.
    positions: DashMap<String, HashMap<String, Position>>,
}

impl GreeksAggregator {
    /// Creates a new empty aggregator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            positions: DashMap::new(),
        }
    }

    /// Adds a position to the specified account.
    ///
    /// If the account does not exist, it is created. If a position with the
    /// same `instrument_symbol` already exists for this account, it is
    /// replaced. Use [`update_position_greeks`](Self::update_position_greeks)
    /// to modify only the Greeks of an existing entry.
    ///
    /// # Arguments
    ///
    /// * `account` - Account identifier
    /// * `position` - The position to add
    pub fn add_position(&self, account: &str, position: Position) {
        let key = position.instrument_symbol.clone();
        // Fast path: avoid allocating a new String key when the account
        // already exists (common case in hot loops).
        if let Some(mut entry) = self.positions.get_mut(account) {
            entry.value_mut().insert(key, position);
        } else {
            self.positions
                .entry(account.to_string())
                .or_default()
                .insert(key, position);
        }
    }

    /// Removes the position matching `instrument_symbol` from the account.
    ///
    /// Returns the removed position if found, `None` otherwise.
    /// This is an O(1) operation.
    ///
    /// # Account pruning policy
    ///
    /// When this removal empties the account's inner map (i.e. it was the
    /// account's last position), the account key is pruned from the underlying
    /// store so that repeated open/close cycles on a single-position account do
    /// not leak permanent empty shells. Pruning never changes any aggregate: an
    /// empty account contributes zero to [`aggregate_by_underlying`] /
    /// [`aggregate_all`], so removing the shell only spares those calls from
    /// iterating a dead entry.
    ///
    /// The prune uses [`DashMap::remove_if`] rather than a plain `remove`: it
    /// re-checks `inner.is_empty()` while holding the shard lock, so if a
    /// concurrent [`add_position`](Self::add_position) re-populated the account
    /// between our emptiness observation and the prune, `is_empty()` is now
    /// `false` and the account is correctly **kept**. A plain `remove` would
    /// drop that concurrently-added position — a race this method must not
    /// reintroduce.
    ///
    /// The mutable inner guard from `get_mut` is dropped before the
    /// `remove_if` call (it re-enters the same `DashMap` shard), avoiding a
    /// re-entrant shard deadlock.
    ///
    /// # Arguments
    ///
    /// * `account` - Account identifier
    /// * `instrument_symbol` - Symbol of the instrument to remove
    ///
    /// [`aggregate_by_underlying`]: Self::aggregate_by_underlying
    /// [`aggregate_all`]: Self::aggregate_all
    #[must_use]
    pub fn remove_position(&self, account: &str, instrument_symbol: &str) -> Option<Position> {
        // Scope the mutable guard so it is dropped before we re-enter the same
        // `DashMap` shard via `remove_if` below.
        let (removed, now_empty) = {
            let mut entry = self.positions.get_mut(account)?;
            let removed = entry.value_mut().remove(instrument_symbol);
            let now_empty = entry.value().is_empty();
            (removed, now_empty)
        };

        if now_empty {
            // Atomically prune the account shell. `remove_if` re-checks
            // emptiness under the shard lock, so a concurrent `add_position`
            // that re-populated the account keeps it (is_empty() == false).
            self.positions
                .remove_if(account, |_, inner| inner.is_empty());
        }

        removed
    }

    /// Updates the Greeks for an existing position.
    ///
    /// Finds the position matching `instrument_symbol` in the account
    /// and replaces its Greeks. Returns `true` if updated, `false` if not found.
    /// This is an O(1) operation.
    ///
    /// # Arguments
    ///
    /// * `account` - Account identifier
    /// * `instrument_symbol` - Symbol of the instrument to update
    /// * `greeks` - New Greeks values
    #[must_use]
    pub fn update_position_greeks(
        &self,
        account: &str,
        instrument_symbol: &str,
        greeks: Greek,
    ) -> bool {
        if let Some(mut entry) = self.positions.get_mut(account)
            && let Some(pos) = entry.value_mut().get_mut(instrument_symbol)
        {
            pos.greeks = greeks;
            return true;
        }
        false
    }

    /// Updates the quantity for an existing position.
    ///
    /// Finds the position matching `instrument_symbol` in the account
    /// and replaces its quantity. Returns `true` if updated, `false` if not found.
    /// This is an O(1) operation.
    ///
    /// # Arguments
    ///
    /// * `account` - Account identifier
    /// * `instrument_symbol` - Symbol of the instrument to update
    /// * `quantity` - New signed quantity
    #[must_use]
    pub fn update_position_quantity(
        &self,
        account: &str,
        instrument_symbol: &str,
        quantity: i64,
    ) -> bool {
        if let Some(mut entry) = self.positions.get_mut(account)
            && let Some(pos) = entry.value_mut().get_mut(instrument_symbol)
        {
            pos.quantity = quantity;
            return true;
        }
        false
    }

    /// Aggregates Greeks for a single account.
    ///
    /// Returns [`AggregatedGreeks::default()`] if the account has no positions.
    ///
    /// # Arguments
    ///
    /// * `account` - Account identifier
    #[must_use]
    pub fn aggregate_by_account(&self, account: &str) -> AggregatedGreeks {
        let mut agg = AggregatedGreeks::default();
        if let Some(entry) = self.positions.get(account) {
            for pos in entry.value().values() {
                let qty = Decimal::from(pos.quantity);
                agg.accumulate(&pos.greeks, qty);
            }
        }
        agg
    }

    /// Aggregates Greeks across all accounts for a specific underlying.
    ///
    /// Iterates every account and sums positions whose `underlying` matches.
    /// Returns [`AggregatedGreeks::default()`] if no matching positions exist.
    ///
    /// # Arguments
    ///
    /// * `underlying` - Underlying asset symbol (e.g., `"BTC"`)
    #[must_use]
    pub fn aggregate_by_underlying(&self, underlying: &str) -> AggregatedGreeks {
        let mut agg = AggregatedGreeks::default();
        for entry in self.positions.iter() {
            for pos in entry.value().values() {
                if pos.underlying == underlying {
                    let qty = Decimal::from(pos.quantity);
                    agg.accumulate(&pos.greeks, qty);
                }
            }
        }
        agg
    }

    /// Aggregates Greeks across all accounts and all underlyings.
    ///
    /// Returns [`AggregatedGreeks::default()`] if no positions exist.
    #[must_use]
    pub fn aggregate_all(&self) -> AggregatedGreeks {
        let mut agg = AggregatedGreeks::default();
        for entry in self.positions.iter() {
            for pos in entry.value().values() {
                let qty = Decimal::from(pos.quantity);
                agg.accumulate(&pos.greeks, qty);
            }
        }
        agg
    }

    /// Returns a snapshot of all positions for the given account.
    ///
    /// Returns an empty `Vec` if the account does not exist.
    ///
    /// # Arguments
    ///
    /// * `account` - Account identifier
    #[must_use]
    pub fn positions_for_account(&self, account: &str) -> Vec<Position> {
        self.positions
            .get(account)
            .map(|entry| entry.value().values().cloned().collect())
            .unwrap_or_default()
    }

    /// Returns the number of accounts with at least one position.
    ///
    /// Empty account shells are pruned eagerly by
    /// [`remove_position`](Self::remove_position), so in normal operation no
    /// empty entries exist. The non-empty filter is retained as defense in
    /// depth and to keep the count correct regardless of store internals.
    #[must_use]
    pub fn account_count(&self) -> usize {
        self.positions
            .iter()
            .filter(|entry| !entry.value().is_empty())
            .count()
    }

    /// Removes all positions from all accounts.
    pub fn clear(&self) {
        self.positions.clear();
    }
}

impl Default for GreeksAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GreeksAggregator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total: usize = self.positions.iter().map(|entry| entry.value().len()).sum();
        f.debug_struct("GreeksAggregator")
            .field("accounts", &self.positions.len())
            .field("total_positions", &total)
            .finish()
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Computes `accumulator + (greek_value * quantity)` with checked arithmetic.
///
/// Returns `(result, saturated)` where `saturated` is true if either the
/// multiplication or addition overflowed and was capped to MAX/MIN.
#[inline]
fn checked_mul_add_with_flag(
    accumulator: Decimal,
    greek_value: Decimal,
    quantity: Decimal,
) -> (Decimal, bool) {
    let (product, mul_saturated) = match greek_value.checked_mul(quantity) {
        Some(p) => (p, false),
        None => {
            let sat = if quantity.is_sign_positive() == greek_value.is_sign_positive() {
                Decimal::MAX
            } else {
                Decimal::MIN
            };
            (sat, true)
        }
    };
    match accumulator.checked_add(product) {
        Some(r) => (r, mul_saturated),
        None => {
            let sat = if product.is_sign_positive() {
                Decimal::MAX
            } else {
                Decimal::MIN
            };
            (sat, true)
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Creates a `Greek` with all fields set to the given value.
    fn uniform_greek(value: Decimal) -> Greek {
        Greek {
            delta: value,
            gamma: value,
            theta: value,
            vega: value,
            rho: value,
            rho_d: value,
            alpha: value,
            vanna: value,
            vomma: value,
            veta: value,
            charm: value,
            color: value,
        }
    }

    /// Creates a `Greek` with only delta set (all others zero).
    fn delta_only(delta: Decimal) -> Greek {
        Greek {
            delta,
            gamma: Decimal::ZERO,
            theta: Decimal::ZERO,
            vega: Decimal::ZERO,
            rho: Decimal::ZERO,
            rho_d: Decimal::ZERO,
            alpha: Decimal::ZERO,
            vanna: Decimal::ZERO,
            vomma: Decimal::ZERO,
            veta: Decimal::ZERO,
            charm: Decimal::ZERO,
            color: Decimal::ZERO,
        }
    }

    // ── AggregatedGreeks ────────────────────────────────────────────────

    #[test]
    fn test_aggregated_greeks_default_is_zero() {
        let agg = AggregatedGreeks::default();
        assert_eq!(agg.delta, Decimal::ZERO);
        assert_eq!(agg.gamma, Decimal::ZERO);
        assert_eq!(agg.theta, Decimal::ZERO);
        assert_eq!(agg.vega, Decimal::ZERO);
        assert_eq!(agg.rho, Decimal::ZERO);
        assert_eq!(agg.rho_d, Decimal::ZERO);
        assert_eq!(agg.alpha, Decimal::ZERO);
        assert_eq!(agg.vanna, Decimal::ZERO);
        assert_eq!(agg.vomma, Decimal::ZERO);
        assert_eq!(agg.veta, Decimal::ZERO);
        assert_eq!(agg.charm, Decimal::ZERO);
        assert_eq!(agg.color, Decimal::ZERO);
        assert_eq!(agg.position_count, 0);
    }

    #[test]
    fn test_aggregated_greeks_serde_roundtrip() {
        let agg = AggregatedGreeks {
            delta: Decimal::new(123, 2),
            position_count: 5,
            ..AggregatedGreeks::default()
        };
        let json = serde_json::to_string(&agg).unwrap();
        let deserialized: AggregatedGreeks = serde_json::from_str(&json).unwrap();
        assert_eq!(agg, deserialized);
    }

    // ── Position ────────────────────────────────────────────────────────

    #[test]
    fn test_position_creation_and_accessors() {
        let greeks = delta_only(Decimal::new(5, 1));
        let pos = Position::new("BTC-20260130-50000-C", "BTC", 10, greeks.clone());

        assert_eq!(pos.instrument_symbol(), "BTC-20260130-50000-C");
        assert_eq!(pos.underlying(), "BTC");
        assert_eq!(pos.quantity(), 10);
        assert_eq!(pos.greeks().delta, Decimal::new(5, 1));
    }

    // ── Single position aggregation ─────────────────────────────────────

    #[test]
    fn test_single_position_aggregation() {
        let agg = GreeksAggregator::new();
        let greeks = uniform_greek(Decimal::new(1, 1)); // 0.1

        agg.add_position("acc1", Position::new("BTC-C", "BTC", 10, greeks));

        let result = agg.aggregate_by_account("acc1");
        // 10 * 0.1 = 1.0
        assert_eq!(result.delta, Decimal::ONE);
        assert_eq!(result.gamma, Decimal::ONE);
        assert_eq!(result.theta, Decimal::ONE);
        assert_eq!(result.vega, Decimal::ONE);
        assert_eq!(result.rho, Decimal::ONE);
        assert_eq!(result.rho_d, Decimal::ONE);
        assert_eq!(result.alpha, Decimal::ONE);
        assert_eq!(result.vanna, Decimal::ONE);
        assert_eq!(result.vomma, Decimal::ONE);
        assert_eq!(result.veta, Decimal::ONE);
        assert_eq!(result.charm, Decimal::ONE);
        assert_eq!(result.color, Decimal::ONE);
        assert_eq!(result.position_count, 1);
    }

    // ── Multiple positions ──────────────────────────────────────────────

    #[test]
    fn test_multiple_positions_sum_correctly() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "acc1",
            Position::new("BTC-C", "BTC", 5, delta_only(Decimal::new(4, 1))),
        );
        agg.add_position(
            "acc1",
            Position::new("BTC-P", "BTC", 3, delta_only(Decimal::new(-3, 1))),
        );

        let result = agg.aggregate_by_account("acc1");
        // 5 * 0.4 + 3 * (-0.3) = 2.0 + (-0.9) = 1.1
        assert_eq!(result.delta, Decimal::new(11, 1));
        assert_eq!(result.position_count, 2);
    }

    // ── Long / short sign convention ────────────────────────────────────

    #[test]
    fn test_long_short_sign_convention() {
        let agg = GreeksAggregator::new();

        // Long 10 contracts with delta 0.5
        agg.add_position(
            "acc1",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::new(5, 1))),
        );
        // Short 10 contracts with delta 0.5 → -10 * 0.5 = -5.0
        agg.add_position(
            "acc1",
            Position::new("BTC-C-2", "BTC", -10, delta_only(Decimal::new(5, 1))),
        );

        let result = agg.aggregate_by_account("acc1");
        // 10 * 0.5 + (-10) * 0.5 = 5.0 + (-5.0) = 0.0
        assert_eq!(result.delta, Decimal::ZERO);
        assert_eq!(result.position_count, 2);
    }

    // ── Remove position ─────────────────────────────────────────────────

    #[test]
    fn test_remove_position() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "acc1",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "acc1",
            Position::new("ETH-C", "ETH", 5, delta_only(Decimal::ONE)),
        );

        let removed = agg.remove_position("acc1", "BTC-C");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().instrument_symbol(), "BTC-C");

        let result = agg.aggregate_by_account("acc1");
        // Only ETH-C remains: 5 * 1.0 = 5.0
        assert_eq!(result.delta, Decimal::new(5, 0));
        assert_eq!(result.position_count, 1);
    }

    #[test]
    fn test_remove_nonexistent_position() {
        let agg = GreeksAggregator::new();
        assert!(agg.remove_position("ghost", "BTC-C").is_none());
    }

    // ── Account-shell pruning (issue #82) ───────────────────────────────

    #[test]
    fn test_remove_last_position_prunes_account_shell() {
        let agg = GreeksAggregator::new();
        agg.add_position(
            "solo",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );
        assert!(agg.positions.contains_key("solo"));
        assert_eq!(agg.account_count(), 1);

        let removed = agg.remove_position("solo", "BTC-C");
        assert!(removed.is_some());

        // (a) The account shell must be pruned, not left as an empty entry.
        assert!(!agg.positions.contains_key("solo"));
        assert_eq!(agg.account_count(), 0);

        // (b) Aggregations skip the (now absent) account entirely — no
        // contribution and no iteration over a dead entry.
        assert_eq!(
            agg.aggregate_by_underlying("BTC"),
            AggregatedGreeks::default()
        );
        assert_eq!(agg.aggregate_all(), AggregatedGreeks::default());
    }

    #[test]
    fn test_remove_non_last_position_keeps_account() {
        let agg = GreeksAggregator::new();
        agg.add_position(
            "acc",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "acc",
            Position::new("ETH-C", "ETH", 5, delta_only(Decimal::ONE)),
        );

        // Removing one of two positions must NOT prune the account.
        let _ = agg.remove_position("acc", "BTC-C");
        assert!(agg.positions.contains_key("acc"));
        assert_eq!(agg.account_count(), 1);
        assert_eq!(agg.aggregate_by_account("acc").position_count, 1);
    }

    #[test]
    fn test_repeated_open_close_does_not_leak_account_shells() {
        let agg = GreeksAggregator::new();
        // An account that repeatedly opens and closes its only position must
        // not accumulate dead entries.
        for _ in 0..100 {
            agg.add_position(
                "churner",
                Position::new("BTC-C", "BTC", 1, delta_only(Decimal::ONE)),
            );
            let _ = agg.remove_position("churner", "BTC-C");
        }
        assert!(!agg.positions.contains_key("churner"));
        assert_eq!(agg.account_count(), 0);
        assert_eq!(agg.positions.len(), 0);
    }

    #[test]
    fn test_remove_if_keeps_concurrently_repopulated_account() {
        // Deterministic single-thread reproduction of the add/remove race that
        // `remove_if` must tolerate: the account is emptied, then re-populated
        // before the prune runs. The prune must observe `is_empty() == false`
        // and KEEP the account (a plain `remove` would drop the new position).
        let agg = GreeksAggregator::new();
        agg.add_position(
            "acc",
            Position::new("BTC-C", "BTC", 1, delta_only(Decimal::ONE)),
        );

        // Step 1: emulate "last position removed" — inner map becomes empty.
        {
            let mut entry = agg.positions.get_mut("acc").unwrap();
            let _ = entry.value_mut().remove("BTC-C");
        } // guard dropped

        // Step 2: a concurrent add re-populates the account before the prune.
        agg.add_position(
            "acc",
            Position::new("ETH-C", "ETH", 1, delta_only(Decimal::ONE)),
        );

        // Step 3: the prune that `remove_position` would have issued. Because
        // the account is no longer empty, `remove_if` keeps it.
        agg.positions.remove_if("acc", |_, inner| inner.is_empty());

        assert!(agg.positions.contains_key("acc"));
        assert_eq!(agg.account_count(), 1);
        let result = agg.aggregate_by_account("acc");
        assert_eq!(result.position_count, 1);
        assert_eq!(result.delta, Decimal::ONE);
    }

    // ── Update position Greeks ──────────────────────────────────────────

    #[test]
    fn test_update_position_greeks() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "acc1",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );

        // Delta was 1.0, now update to 2.0
        let updated = agg.update_position_greeks("acc1", "BTC-C", delta_only(Decimal::TWO));
        assert!(updated);

        let result = agg.aggregate_by_account("acc1");
        // 10 * 2.0 = 20.0
        assert_eq!(result.delta, Decimal::new(20, 0));
    }

    #[test]
    fn test_update_nonexistent_position() {
        let agg = GreeksAggregator::new();
        assert!(!agg.update_position_greeks("ghost", "BTC-C", delta_only(Decimal::ONE)));
    }

    // ── Update position quantity ────────────────────────────────────────

    #[test]
    fn test_update_position_quantity() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "acc1",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );

        let updated = agg.update_position_quantity("acc1", "BTC-C", -5);
        assert!(updated);

        let result = agg.aggregate_by_account("acc1");
        // -5 * 1.0 = -5.0
        assert_eq!(result.delta, Decimal::new(-5, 0));
    }

    // ── Aggregate by account ────────────────────────────────────────────

    #[test]
    fn test_aggregate_by_account_isolation() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "alice",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "bob",
            Position::new("BTC-C", "BTC", 20, delta_only(Decimal::ONE)),
        );

        let alice_result = agg.aggregate_by_account("alice");
        assert_eq!(alice_result.delta, Decimal::new(10, 0));
        assert_eq!(alice_result.position_count, 1);

        let bob_result = agg.aggregate_by_account("bob");
        assert_eq!(bob_result.delta, Decimal::new(20, 0));
        assert_eq!(bob_result.position_count, 1);
    }

    // ── Aggregate by underlying ─────────────────────────────────────────

    #[test]
    fn test_aggregate_by_underlying_cross_account() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "alice",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "bob",
            Position::new("BTC-P", "BTC", 5, delta_only(Decimal::new(-5, 1))),
        );
        agg.add_position(
            "alice",
            Position::new("ETH-C", "ETH", 100, delta_only(Decimal::ONE)),
        );

        let btc_result = agg.aggregate_by_underlying("BTC");
        // 10 * 1.0 + 5 * (-0.5) = 10.0 + (-2.5) = 7.5
        assert_eq!(btc_result.delta, Decimal::new(75, 1));
        assert_eq!(btc_result.position_count, 2);

        let eth_result = agg.aggregate_by_underlying("ETH");
        assert_eq!(eth_result.delta, Decimal::new(100, 0));
        assert_eq!(eth_result.position_count, 1);
    }

    // ── Aggregate all ───────────────────────────────────────────────────

    #[test]
    fn test_aggregate_all() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "alice",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "bob",
            Position::new("ETH-C", "ETH", 20, delta_only(Decimal::TWO)),
        );

        let result = agg.aggregate_all();
        // 10 * 1.0 + 20 * 2.0 = 10.0 + 40.0 = 50.0
        assert_eq!(result.delta, Decimal::new(50, 0));
        assert_eq!(result.position_count, 2);
    }

    // ── Empty account ───────────────────────────────────────────────────

    #[test]
    fn test_empty_account_returns_default() {
        let agg = GreeksAggregator::new();
        let result = agg.aggregate_by_account("nonexistent");
        assert_eq!(result, AggregatedGreeks::default());
    }

    #[test]
    fn test_empty_underlying_returns_default() {
        let agg = GreeksAggregator::new();
        let result = agg.aggregate_by_underlying("DOESNOTEXIST");
        assert_eq!(result, AggregatedGreeks::default());
    }

    // ── positions_for_account ───────────────────────────────────────────

    #[test]
    fn test_positions_for_account() {
        let agg = GreeksAggregator::new();

        agg.add_position(
            "acc1",
            Position::new("BTC-C", "BTC", 10, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "acc1",
            Position::new("ETH-C", "ETH", 5, delta_only(Decimal::ONE)),
        );

        let positions = agg.positions_for_account("acc1");
        assert_eq!(positions.len(), 2);
    }

    #[test]
    fn test_positions_for_nonexistent_account() {
        let agg = GreeksAggregator::new();
        assert!(agg.positions_for_account("ghost").is_empty());
    }

    // ── account_count and clear ─────────────────────────────────────────

    #[test]
    fn test_account_count_and_clear() {
        let agg = GreeksAggregator::new();
        assert_eq!(agg.account_count(), 0);

        agg.add_position(
            "alice",
            Position::new("BTC-C", "BTC", 1, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "bob",
            Position::new("BTC-C", "BTC", 1, delta_only(Decimal::ONE)),
        );
        assert_eq!(agg.account_count(), 2);

        agg.clear();
        assert_eq!(agg.account_count(), 0);
        assert_eq!(agg.aggregate_all(), AggregatedGreeks::default());
    }

    // ── Debug ───────────────────────────────────────────────────────────

    #[test]
    fn test_debug_format() {
        let agg = GreeksAggregator::new();
        agg.add_position(
            "acc1",
            Position::new("BTC-C", "BTC", 1, delta_only(Decimal::ONE)),
        );
        let debug = format!("{:?}", agg);
        assert!(debug.contains("GreeksAggregator"));
        assert!(debug.contains("accounts"));
        assert!(debug.contains("total_positions"));
    }

    // ── Thread safety ───────────────────────────────────────────────────

    #[test]
    fn test_concurrent_add_and_aggregate() {
        let agg = Arc::new(GreeksAggregator::new());

        let mut handles = vec![];
        for i in 0..4 {
            let agg_clone = Arc::clone(&agg);
            handles.push(thread::spawn(move || {
                for j in 0..50 {
                    let symbol = format!("INST-{}-{}", i, j);
                    let account = format!("acc-{}", i);
                    agg_clone.add_position(
                        &account,
                        Position::new(symbol, "BTC", 1, delta_only(Decimal::ONE)),
                    );
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // 4 accounts × 50 positions = 200 total
        let result = agg.aggregate_all();
        assert_eq!(result.position_count, 200);
        assert_eq!(result.delta, Decimal::new(200, 0));
        assert_eq!(agg.account_count(), 4);
    }

    // ── checked_mul_add_with_flag ─────────────────────────────────────────

    #[test]
    fn test_checked_mul_add_normal() {
        let (result, saturated) =
            checked_mul_add_with_flag(Decimal::ONE, Decimal::TWO, Decimal::new(3, 0));
        // 1 + (2 * 3) = 7
        assert_eq!(result, Decimal::new(7, 0));
        assert!(!saturated);
    }

    #[test]
    fn test_checked_mul_add_zero_quantity() {
        let (result, saturated) =
            checked_mul_add_with_flag(Decimal::new(5, 0), Decimal::new(100, 0), Decimal::ZERO);
        // 5 + (100 * 0) = 5
        assert_eq!(result, Decimal::new(5, 0));
        assert!(!saturated);
    }

    #[test]
    fn test_checked_mul_add_overflow_saturates_to_max() {
        // Force multiplication overflow: MAX * 2 should saturate.
        let acc = Decimal::ZERO;
        let value = Decimal::MAX;
        let quantity = Decimal::TWO;

        let (result, saturated) = checked_mul_add_with_flag(acc, value, quantity);
        assert_eq!(result, Decimal::MAX);
        assert!(saturated);
    }

    #[test]
    fn test_checked_mul_add_overflow_saturates_to_min() {
        // Force addition overflow in the negative direction: MIN + (MIN * 2) should saturate.
        let acc = Decimal::MIN;
        let value = Decimal::MIN;
        let quantity = Decimal::TWO;

        let (result, saturated) = checked_mul_add_with_flag(acc, value, quantity);
        assert_eq!(result, Decimal::MIN);
        assert!(saturated);
    }

    // ── Saturation flag ─────────────────────────────────────────────────

    #[test]
    fn test_aggregated_greeks_saturation_flag_set_on_overflow() {
        let agg = GreeksAggregator::new();
        // Create a position with extreme Greeks that will cause overflow
        let extreme_greeks = Greek {
            delta: Decimal::MAX,
            gamma: Decimal::ZERO,
            theta: Decimal::ZERO,
            vega: Decimal::ZERO,
            rho: Decimal::ZERO,
            rho_d: Decimal::ZERO,
            alpha: Decimal::ZERO,
            vanna: Decimal::ZERO,
            vomma: Decimal::ZERO,
            veta: Decimal::ZERO,
            charm: Decimal::ZERO,
            color: Decimal::ZERO,
        };
        // Add two positions that will overflow delta when summed
        agg.add_position("acc", Position::new("A", "BTC", 2, extreme_greeks.clone()));
        agg.add_position("acc", Position::new("B", "BTC", 2, extreme_greeks));

        let result = agg.aggregate_by_account("acc");
        assert!(result.saturated, "Expected saturation flag to be set");
        assert_eq!(result.delta, Decimal::MAX);
    }

    #[test]
    fn test_aggregated_greeks_no_saturation_for_normal_values() {
        let agg = GreeksAggregator::new();
        agg.add_position(
            "acc",
            Position::new("A", "BTC", 10, delta_only(Decimal::ONE)),
        );
        agg.add_position(
            "acc",
            Position::new("B", "BTC", 5, delta_only(Decimal::TWO)),
        );

        let result = agg.aggregate_by_account("acc");
        assert!(!result.saturated, "Expected no saturation");
        assert_eq!(result.delta, Decimal::new(20, 0)); // 10*1 + 5*2 = 20
    }
}
