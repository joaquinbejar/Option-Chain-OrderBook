//! Expiry scheduling module.
//!
//! This module provides [`ExpiryScheduler`] for automatically creating missing
//! expiration order books based on [`ExpiryCycleConfig`] and generating strikes
//! using [`StrikeGenerator`].
//!
//! ## Algorithm
//!
//! 1. Generate expected expiration dates from config
//! 2. For each date, create expiration if missing
//! 3. If expiration is new (no strikes), generate strikes using spot price
//! 4. Invoke callback for each newly created expiration
//!
//! ## Example
//!
//! ```
//! use option_chain_orderbook::orderbook::{
//!     ExpiryScheduler, ExpiryCycleConfig, StrikeRangeConfig, UnderlyingOrderBook,
//! };
//! use chrono::Utc;
//!
//! let book = UnderlyingOrderBook::new("BTC");
//! let expiry_config = ExpiryCycleConfig::default();
//! let strike_config = StrikeRangeConfig::builder()
//!     .range_pct(0.10)
//!     .strike_interval(1000)
//!     .min_strikes(5)
//!     .max_strikes(50)
//!     .build()
//!     .expect("valid config");
//!
//! let result = ExpiryScheduler::refresh_expirations(
//!     &book,
//!     Utc::now(),
//!     &expiry_config,
//!     &strike_config,
//!     50000,
//!     None,
//! ).expect("refresh should succeed");
//!
//! assert!(!result.created.is_empty());
//! ```

use super::expiry_cycle::ExpiryCycleConfig;
use super::strike_generator::StrikeGenerator;
use super::strike_range::StrikeRangeConfig;
use super::underlying::UnderlyingOrderBook;
use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use optionstratlib::ExpirationDate;

// ─── RefreshResult ────────────────────────────────────────────────────────────

/// Result of a refresh operation.
///
/// Contains the list of newly created expiration dates and the total number
/// of strikes generated across all new expirations.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::RefreshResult;
/// use optionstratlib::prelude::{ExpirationDate, Positive};
///
/// let result = RefreshResult {
///     created: vec![ExpirationDate::Days(Positive::THIRTY)],
///     strikes_generated: 11,
/// };
/// assert_eq!(result.created.len(), 1);
/// ```
#[derive(Debug, Clone, Default)]
pub struct RefreshResult {
    /// Expiration dates that were newly created.
    pub created: Vec<ExpirationDate>,
    /// Total number of strikes generated across all new expirations.
    pub strikes_generated: usize,
}

impl RefreshResult {
    /// Returns true if no expirations were created.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.created.is_empty()
    }

    /// Returns the number of expirations created.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.created.len()
    }
}

// ─── ExpirationCallback ───────────────────────────────────────────────────────

/// Callback invoked for each newly created expiration.
///
/// The callback receives the expiration date that was just created.
pub type ExpirationCallback = Box<dyn Fn(&ExpirationDate) + Send + Sync>;

// ─── ExpiryScheduler ──────────────────────────────────────────────────────────

/// Zero-sized expiry scheduling utility.
///
/// Provides static methods for refreshing expirations on an underlying order
/// book. Creates missing expirations based on [`ExpiryCycleConfig`] and
/// generates strikes for new expirations using [`StrikeGenerator`].
///
/// All operations are idempotent: calling them multiple times produces the
/// same result as calling once.
pub struct ExpiryScheduler;

impl ExpiryScheduler {
    /// Refreshes expirations for an underlying, creating missing ones and
    /// generating strikes for new expirations.
    ///
    /// # Algorithm
    ///
    /// 1. Generate expected dates from `expiry_config.generate_dates(now)`
    /// 2. For each date, atomically get-or-create the expiration, learning from
    ///    the insert result whether this call created it
    /// 3. If this call created it, generate strikes
    /// 4. Invoke callback for each newly created expiration
    ///
    /// # Concurrency
    ///
    /// Newness is derived from the atomic
    /// [`UnderlyingOrderBook::get_or_create_expiration_inserted`] result rather
    /// than a separate existence probe, so concurrent refreshes of the same
    /// date create it, generate strikes, and invoke the callback exactly once.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying order book to refresh
    /// * `now` - Current datetime for computing expected expirations
    /// * `expiry_config` - Configuration for which expirations to create
    /// * `strike_config` - Configuration for strike generation
    /// * `spot_price` - Current spot price for strike generation
    /// * `callback` - Optional callback invoked for each new expiration
    ///
    /// # Returns
    ///
    /// A [`RefreshResult`] containing the created expirations and strike count.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - `expiry_config` validation fails
    /// - `strike_config` validation fails
    /// - Strike generation fails
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{
    ///     ExpiryScheduler, ExpiryCycleConfig, StrikeRangeConfig, UnderlyingOrderBook,
    /// };
    /// use chrono::Utc;
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// let expiry_config = ExpiryCycleConfig::default();
    /// let strike_config = StrikeRangeConfig::builder()
    ///     .range_pct(0.10)
    ///     .strike_interval(1000)
    ///     .min_strikes(5)
    ///     .max_strikes(50)
    ///     .build()
    ///     .expect("valid config");
    ///
    /// let result = ExpiryScheduler::refresh_expirations(
    ///     &book,
    ///     Utc::now(),
    ///     &expiry_config,
    ///     &strike_config,
    ///     50000,
    ///     None,
    /// ).expect("refresh should succeed");
    ///
    /// // Default config creates multiple expirations (daily, weekly, monthly, quarterly)
    /// assert!(!result.created.is_empty());
    /// ```
    pub fn refresh_expirations(
        underlying: &UnderlyingOrderBook,
        now: DateTime<Utc>,
        expiry_config: &ExpiryCycleConfig,
        strike_config: &StrikeRangeConfig,
        spot_price: u64,
        callback: Option<&ExpirationCallback>,
    ) -> Result<RefreshResult> {
        // Generate expected dates from config
        let expected_dates = expiry_config.generate_dates(now)?;

        let mut result = RefreshResult::default();

        for date in expected_dates {
            // Atomically get-or-create the expiration. `is_new` is the
            // insert result of that single atomic publish, NOT a separate
            // check-then-act probe: exactly one concurrent caller observes
            // `is_new == true` for a given date (the `get_or_insert` winner),
            // so strike generation and the callback run exactly once per date
            // even under concurrent refreshes.
            let (exp, is_new) = underlying.get_or_create_expiration_inserted(date);

            // Only process truly new expirations (the unique creating caller).
            if is_new {
                // Generate and apply strikes
                let strikes =
                    StrikeGenerator::refresh_strikes(exp.chain(), spot_price, strike_config)?;

                result.strikes_generated = result
                    .strikes_generated
                    .checked_add(strikes.len())
                    .ok_or_else(|| {
                        tracing::error!(
                            underlying = %underlying.underlying(),
                            "strikes_generated overflow",
                        );
                        Error::configuration("strikes_generated overflow")
                    })?;

                result.created.push(date);

                // Cold path: the scheduler generated the strike ladder for a
                // newly-created expiration. The fundamental "expiration created"
                // INFO is emitted by the expiration-manager insertion-winner
                // (covering every creator); this records the scheduler's own
                // strike-generation step. Never on the order-submission path.
                tracing::info!(
                    underlying = %underlying.underlying(),
                    expiration = %date,
                    strikes = strikes.len(),
                    "strikes generated for expiration",
                );

                // Invoke callback if provided
                if let Some(cb) = callback {
                    cb(&date);
                }
            }
        }

        Ok(result)
    }

    /// Refreshes expirations using configs stored on the underlying.
    ///
    /// This is a convenience method that retrieves the expiry cycle and strike
    /// range configurations from the underlying order book.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying order book (must have configs set)
    /// * `now` - Current datetime for computing expected expirations
    /// * `spot_price` - Current spot price for strike generation
    /// * `callback` - Optional callback invoked for each new expiration
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - Expiry cycle config is not set on the underlying
    /// - No strike range configs are set on the underlying
    /// - Any config validation fails
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{
    ///     ExpiryScheduler, ExpiryCycleConfig, ExpiryType, StrikeRangeConfig,
    ///     UnderlyingOrderBook,
    /// };
    /// use chrono::Utc;
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    ///
    /// // Set configs on the underlying
    /// book.set_expiry_cycle_config(ExpiryCycleConfig::default()).expect("valid");
    /// book.set_strike_range_config(
    ///     ExpiryType::Daily,
    ///     StrikeRangeConfig::builder()
    ///         .range_pct(0.10)
    ///         .strike_interval(1000)
    ///         .build()
    ///         .expect("valid"),
    /// ).expect("valid");
    ///
    /// let result = ExpiryScheduler::refresh_from_underlying(&book, Utc::now(), 50000, None)
    ///     .expect("refresh should succeed");
    /// ```
    pub fn refresh_from_underlying(
        underlying: &UnderlyingOrderBook,
        now: DateTime<Utc>,
        spot_price: u64,
        callback: Option<&ExpirationCallback>,
    ) -> Result<RefreshResult> {
        // Get expiry cycle config
        let expiry_config = underlying
            .expiry_cycle_config()
            .ok_or_else(|| Error::configuration("expiry cycle config not set on underlying"))?;

        // Get strike range configs - require exactly one to avoid
        // nondeterministic selection. Drain the iterator in a single pass so
        // the "exactly one" check and the extraction share the same probe: no
        // separate `.len()` + `.expect()` that could panic if they disagreed.
        let strike_configs = underlying.strike_range_configs();
        let mut values = strike_configs.values();
        let strike_config = match (values.next(), values.next()) {
            (None, _) => {
                return Err(Error::configuration(
                    "no strike range configs set on underlying",
                ));
            }
            (Some(config), None) => config.clone(),
            (Some(_), Some(_)) => {
                return Err(Error::configuration(
                    "multiple strike range configs set; refresh_from_underlying requires exactly one",
                ));
            }
        };

        Self::refresh_expirations(
            underlying,
            now,
            &expiry_config,
            &strike_config,
            spot_price,
            callback,
        )
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::{CycleRule, ExpiryType};
    use chrono::NaiveTime;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn time(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).expect("valid test time")
    }

    fn default_strike_config() -> StrikeRangeConfig {
        StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(1000)
            .min_strikes(5)
            .max_strikes(50)
            .build()
            .expect("valid config")
    }

    fn minimal_expiry_config() -> ExpiryCycleConfig {
        ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: 2,
            }],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        }
    }

    // ── refresh_expirations basic ─────────────────────────────────────────────

    #[test]
    fn test_refresh_creates_expirations() {
        let book = UnderlyingOrderBook::new("BTC");
        let expiry_config = minimal_expiry_config();
        let strike_config = default_strike_config();

        let result = ExpiryScheduler::refresh_expirations(
            &book,
            Utc::now(),
            &expiry_config,
            &strike_config,
            50000,
            None,
        )
        .expect("refresh should succeed");

        // Should create 2 daily expirations
        assert_eq!(result.created.len(), 2);
        assert!(result.strikes_generated > 0);
        assert_eq!(book.expiration_count(), 2);
    }

    #[test]
    fn test_refresh_generates_strikes() {
        let book = UnderlyingOrderBook::new("BTC");
        let expiry_config = minimal_expiry_config();
        let strike_config = default_strike_config();

        let result = ExpiryScheduler::refresh_expirations(
            &book,
            Utc::now(),
            &expiry_config,
            &strike_config,
            50000,
            None,
        )
        .expect("refresh should succeed");

        // Each expiration should have strikes
        for date in &result.created {
            let exp = book.get_expiration(date).expect("expiration exists");
            assert!(!exp.is_empty(), "expiration should have strikes");
        }
    }

    #[test]
    fn test_refresh_is_idempotent() {
        let book = UnderlyingOrderBook::new("BTC");
        let expiry_config = minimal_expiry_config();
        let strike_config = default_strike_config();
        let now = Utc::now(); // Capture once to avoid flakiness across day boundary

        // First refresh
        let result1 = ExpiryScheduler::refresh_expirations(
            &book,
            now,
            &expiry_config,
            &strike_config,
            50000,
            None,
        )
        .expect("first refresh should succeed");

        assert_eq!(result1.created.len(), 2);

        // Second refresh - same config, same time
        let result2 = ExpiryScheduler::refresh_expirations(
            &book,
            now,
            &expiry_config,
            &strike_config,
            50000,
            None,
        )
        .expect("second refresh should succeed");

        // No new expirations should be created
        assert!(
            result2.created.is_empty(),
            "idempotent refresh should not create new expirations"
        );
        assert_eq!(result2.strikes_generated, 0);

        // Total expiration count unchanged
        assert_eq!(book.expiration_count(), 2);
    }

    #[test]
    fn test_existing_expirations_untouched() {
        let book = UnderlyingOrderBook::new("BTC");
        let expiry_config = minimal_expiry_config();
        let strike_config = default_strike_config();
        let now = Utc::now(); // Capture once to avoid flakiness across day boundary

        // First refresh
        let result1 = ExpiryScheduler::refresh_expirations(
            &book,
            now,
            &expiry_config,
            &strike_config,
            50000,
            None,
        )
        .expect("first refresh should succeed");

        // Get strike counts for each expiration
        let original_strikes: Vec<_> = result1
            .created
            .iter()
            .map(|d| {
                book.get_expiration(d)
                    .expect("exists")
                    .chain()
                    .strike_count()
            })
            .collect();

        // Second refresh with different spot price
        let _ = ExpiryScheduler::refresh_expirations(
            &book,
            now,
            &expiry_config,
            &strike_config,
            60000, // Different spot
            None,
        )
        .expect("second refresh should succeed");

        // Strike counts should be unchanged
        for (i, date) in result1.created.iter().enumerate() {
            let current_strikes = book
                .get_expiration(date)
                .expect("exists")
                .chain()
                .strike_count();
            assert_eq!(
                current_strikes, original_strikes[i],
                "existing expiration strikes should not change"
            );
        }
    }

    #[test]
    fn test_empty_config_returns_error() {
        let book = UnderlyingOrderBook::new("BTC");
        let expiry_config = ExpiryCycleConfig {
            cycles: vec![],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        };
        let strike_config = default_strike_config();

        let result = ExpiryScheduler::refresh_expirations(
            &book,
            Utc::now(),
            &expiry_config,
            &strike_config,
            50000,
            None,
        );

        // Empty cycles config is invalid per ExpiryCycleConfig::validate()
        assert!(result.is_err(), "empty cycles should fail validation");
    }

    #[test]
    fn test_callback_invoked_for_new_expirations() {
        let book = UnderlyingOrderBook::new("BTC");
        let expiry_config = minimal_expiry_config();
        let strike_config = default_strike_config();

        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_clone = Arc::clone(&callback_count);

        let callback: ExpirationCallback = Box::new(move |_date| {
            callback_count_clone.fetch_add(1, Ordering::SeqCst);
        });

        let result = ExpiryScheduler::refresh_expirations(
            &book,
            Utc::now(),
            &expiry_config,
            &strike_config,
            50000,
            Some(&callback),
        )
        .expect("refresh should succeed");

        assert_eq!(
            callback_count.load(Ordering::SeqCst),
            result.created.len(),
            "callback should be invoked for each new expiration"
        );
    }

    #[test]
    fn test_callback_not_invoked_for_existing() {
        let book = UnderlyingOrderBook::new("BTC");
        let expiry_config = minimal_expiry_config();
        let strike_config = default_strike_config();
        let now = Utc::now(); // Capture once to avoid flakiness across day boundary

        // First refresh without callback
        let _ = ExpiryScheduler::refresh_expirations(
            &book,
            now,
            &expiry_config,
            &strike_config,
            50000,
            None,
        )
        .expect("first refresh should succeed");

        // Second refresh with callback
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_clone = Arc::clone(&callback_count);

        let callback: ExpirationCallback = Box::new(move |_date| {
            callback_count_clone.fetch_add(1, Ordering::SeqCst);
        });

        let _ = ExpiryScheduler::refresh_expirations(
            &book,
            now,
            &expiry_config,
            &strike_config,
            50000,
            Some(&callback),
        )
        .expect("second refresh should succeed");

        assert_eq!(
            callback_count.load(Ordering::SeqCst),
            0,
            "callback should not be invoked for existing expirations"
        );
    }

    // ── concurrent refresh race ────────────────────────────────────────────────

    #[test]
    fn test_refresh_expirations_concurrent_creates_each_date_once() {
        use std::sync::Barrier;
        use std::thread;

        // N threads race to refresh the SAME set of expiration dates at the
        // SAME wall-clock instant, started in lockstep via a Barrier (no
        // sleeps). With newness derived from the atomic get-or-create insert
        // result, each date is created exactly once: the manager holds one
        // entry per date AND the callback fires exactly once per date.
        //
        // Regression direction: against the old separate `get_expiration`
        // probe (check-then-act), multiple threads could each observe
        // `is_new == true` for the same date before any insert landed,
        // double-invoking the callback and re-running strike generation. That
        // logic would make `callback_count` and `total_created` exceed the
        // distinct-date count, failing the assertions below.
        const N: usize = 16;
        const SPOT: u64 = 50_000;

        let book = Arc::new(UnderlyingOrderBook::new("BTC"));
        let expiry_config = minimal_expiry_config(); // 2 daily expirations
        let strike_config = default_strike_config();
        // Capture a single fixed instant so every thread computes the SAME
        // dates deterministically (no day-boundary flakiness, no RNG).
        let now = Utc::now();

        let expected_dates = expiry_config
            .generate_dates(now)
            .expect("valid config")
            .len();
        assert_eq!(expected_dates, 2, "fixture should produce 2 dates");

        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_clone = Arc::clone(&callback_count);
        let callback: Arc<ExpirationCallback> = Arc::new(Box::new(move |_date| {
            callback_count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        let barrier = Arc::new(Barrier::new(N));

        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let book = Arc::clone(&book);
            let expiry_config = expiry_config.clone();
            let strike_config = strike_config.clone();
            let callback = Arc::clone(&callback);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                ExpiryScheduler::refresh_expirations(
                    &book,
                    now,
                    &expiry_config,
                    &strike_config,
                    SPOT,
                    Some(callback.as_ref()),
                )
                .expect("refresh should succeed")
            }));
        }

        let mut total_created = 0usize;
        for handle in handles {
            let result = handle.join().expect("thread panicked");
            total_created = total_created
                .checked_add(result.created.len())
                .expect("created overflow");
        }

        // Exactly-once create: the manager holds one entry per date.
        assert_eq!(
            book.expiration_count(),
            expected_dates,
            "each date must be created exactly once in the manager"
        );

        // Exactly-once create across threads: summed `created` over all
        // refreshes equals the distinct-date count (one winner per date).
        assert_eq!(
            total_created, expected_dates,
            "exactly one thread should report creating each date"
        );

        // Exactly-once callback: callback fired once per distinct date.
        assert_eq!(
            callback_count.load(Ordering::SeqCst),
            expected_dates,
            "callback must fire exactly once per date"
        );
    }

    // ── refresh_from_underlying ───────────────────────────────────────────────

    #[test]
    fn test_refresh_from_underlying_works() {
        let book = UnderlyingOrderBook::new("BTC");

        // Set configs on underlying
        book.set_expiry_cycle_config(minimal_expiry_config())
            .expect("valid config");
        book.set_strike_range_config(ExpiryType::Daily, default_strike_config())
            .expect("valid config");

        let result = ExpiryScheduler::refresh_from_underlying(&book, Utc::now(), 50000, None)
            .expect("refresh should succeed");

        assert_eq!(result.created.len(), 2);
    }

    #[test]
    fn test_refresh_from_underlying_missing_expiry_config() {
        let book = UnderlyingOrderBook::new("BTC");

        // Set only strike config
        book.set_strike_range_config(ExpiryType::Daily, default_strike_config())
            .expect("valid config");

        let result = ExpiryScheduler::refresh_from_underlying(&book, Utc::now(), 50000, None);

        assert!(result.is_err(), "should fail without expiry cycle config");
    }

    #[test]
    fn test_refresh_from_underlying_missing_strike_config() {
        let book = UnderlyingOrderBook::new("BTC");

        // Set only expiry config
        book.set_expiry_cycle_config(minimal_expiry_config())
            .expect("valid config");

        let result = ExpiryScheduler::refresh_from_underlying(&book, Utc::now(), 50000, None);

        assert!(result.is_err(), "should fail without strike range config");
    }

    // ── RefreshResult ─────────────────────────────────────────────────────────

    #[test]
    fn test_refresh_result_default() {
        let result = RefreshResult::default();
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert_eq!(result.strikes_generated, 0);
    }
}
