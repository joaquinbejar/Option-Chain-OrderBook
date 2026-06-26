//! Expiry lifecycle management module.
//!
//! This module provides [`ExpiryLifecycleManager`] for transitioning expirations
//! through the settlement lifecycle: Active → Settling → Expired → Removed.
//!
//! ## Lifecycle States
//!
//! ```text
//! Active ──(expiry_time)──→ Settling ──(settlement_time)──→ Expired ──(retention)──→ Removed
//! ```
//!
//! - **Settling**: All resting orders are cancelled; no new orders accepted.
//! - **Expired**: Settlement complete; instruments are frozen.
//! - **Removed**: Expiration cleaned up from the hierarchy after the retention period.
//!
//! ## Algorithm
//!
//! 1. For each expiration, compute expiry and settlement datetimes from config
//! 2. Derive the chain state as the **minimum** status across every call and put
//!    book in the chain, so a single lagging book forces the needed transition
//! 3. Apply the appropriate transition based on `now` vs. the computed times
//! 4. Emit [`LifecycleEvent`]s and invoke the optional listener
//!
//! ## Example
//!
//! ```
//! use option_chain_orderbook::orderbook::{
//!     ExpiryLifecycleManager, ExpiryCycleConfig, LifecycleConfig,
//!     UnderlyingOrderBook,
//! };
//! use chrono::Utc;
//!
//! let book = UnderlyingOrderBook::new("BTC");
//! let expiry_config = ExpiryCycleConfig::default();
//! let lifecycle_config = LifecycleConfig::default();
//!
//! let result = ExpiryLifecycleManager::check_expirations(
//!     &book,
//!     Utc::now(),
//!     &expiry_config,
//!     &lifecycle_config,
//!     None,
//! ).expect("lifecycle check should succeed");
//!
//! assert!(result.events.is_empty());
//! ```

use super::expiry_cycle::{ExpiryCycleConfig, to_datetime};
use super::instrument_status::InstrumentStatus;
use super::underlying::UnderlyingOrderBook;
use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use optionstratlib::ExpirationDate;
use std::sync::Arc;

// ─── LifecycleConfig ─────────────────────────────────────────────────────────

/// Configuration for expiration lifecycle management.
///
/// Controls how long expired expirations are retained in the hierarchy
/// before being removed.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::LifecycleConfig;
/// use chrono::Duration;
///
/// let config = LifecycleConfig {
///     retention_period: Duration::hours(24),
/// };
/// assert_eq!(config.retention_period.num_hours(), 24);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// How long after settlement to keep expired expirations before removal.
    ///
    /// After an expiration transitions to [`Expired`](InstrumentStatus::Expired),
    /// it remains in the hierarchy for this duration. Once the retention period
    /// elapses, the expiration is removed entirely.
    pub retention_period: chrono::Duration,
}

impl Default for LifecycleConfig {
    /// Default retention period is 24 hours.
    fn default() -> Self {
        Self {
            retention_period: chrono::Duration::hours(24),
        }
    }
}

impl LifecycleConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if `retention_period` is negative.
    pub fn validate(&self) -> Result<()> {
        if self.retention_period < chrono::Duration::zero() {
            return Err(Error::configuration(
                "retention_period must not be negative",
            ));
        }
        Ok(())
    }
}

// ─── LifecycleEvent ──────────────────────────────────────────────────────────

/// Events emitted during expiration lifecycle transitions.
///
/// Each variant represents a specific state transition that occurred during
/// a [`check_expirations`](ExpiryLifecycleManager::check_expirations) call.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// Instruments transitioned to [`Settling`](InstrumentStatus::Settling).
    ///
    /// All resting orders were cancelled. No new orders will be accepted.
    InstrumentSettling {
        /// The underlying asset symbol.
        underlying: String,
        /// The expiration date that entered the settling state.
        expiration: ExpirationDate,
        /// Number of orders that were cancelled during the transition.
        cancelled_orders: usize,
    },

    /// Instruments transitioned to [`Expired`](InstrumentStatus::Expired).
    ///
    /// Settlement is complete. The expiration will be retained for the
    /// configured retention period before removal.
    InstrumentExpired {
        /// The underlying asset symbol.
        underlying: String,
        /// The expiration date that expired.
        expiration: ExpirationDate,
    },

    /// Expired expiration removed from the hierarchy.
    ///
    /// The retention period has elapsed and the expiration was cleaned up.
    ExpirationRemoved {
        /// The underlying asset symbol.
        underlying: String,
        /// The expiration date that was removed.
        expiration: ExpirationDate,
    },
}

// ─── LifecycleListener ───────────────────────────────────────────────────────

/// Callback invoked for each lifecycle event.
///
/// The listener receives a reference to each [`LifecycleEvent`] as it occurs.
/// Listeners must be thread-safe (`Send + Sync`).
pub type LifecycleListener = Arc<dyn Fn(&LifecycleEvent) + Send + Sync>;

// ─── LifecycleResult ─────────────────────────────────────────────────────────

/// Result of a lifecycle check.
///
/// Contains all events that were emitted during the check.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::LifecycleResult;
///
/// let result = LifecycleResult::default();
/// assert!(result.events.is_empty());
/// assert!(result.is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct LifecycleResult {
    /// All lifecycle events emitted during the check.
    pub events: Vec<LifecycleEvent>,
}

impl LifecycleResult {
    /// Returns true if no lifecycle events were emitted.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Returns the number of lifecycle events emitted.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }
}

// ─── ExpiryLifecycleManager ──────────────────────────────────────────────────

/// Manages expiration lifecycle transitions.
///
/// This is a stateless utility struct. All state is derived from the current
/// instrument statuses stored in the [`OptionOrderBook`](super::book::OptionOrderBook)
/// instances within the hierarchy.
///
/// # Thread Safety
///
/// All operations are safe to call from multiple threads. Status transitions
/// use atomic operations on the underlying order books.
pub struct ExpiryLifecycleManager;

impl ExpiryLifecycleManager {
    /// Checks all expirations and performs lifecycle transitions.
    ///
    /// For each expiration in the underlying, computes the expiry and settlement
    /// datetimes from the config, determines the current state, and applies the
    /// appropriate transition.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying order book to check
    /// * `now` - Current UTC datetime
    /// * `expiry_config` - Expiry cycle config with expiry/settlement times
    /// * `lifecycle_config` - Lifecycle config with retention period
    /// * `listener` - Optional callback for lifecycle events
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - `expiry_config` validation fails
    /// - `lifecycle_config` validation fails
    /// - Date conversion fails for an expiration
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{
    ///     ExpiryLifecycleManager, ExpiryCycleConfig, LifecycleConfig,
    ///     UnderlyingOrderBook,
    /// };
    /// use chrono::Utc;
    ///
    /// let book = UnderlyingOrderBook::new("BTC");
    /// let expiry_config = ExpiryCycleConfig::default();
    /// let lifecycle_config = LifecycleConfig::default();
    ///
    /// let result = ExpiryLifecycleManager::check_expirations(
    ///     &book,
    ///     Utc::now(),
    ///     &expiry_config,
    ///     &lifecycle_config,
    ///     None,
    /// ).expect("lifecycle check should succeed");
    /// ```
    pub fn check_expirations(
        underlying: &UnderlyingOrderBook,
        now: DateTime<Utc>,
        expiry_config: &ExpiryCycleConfig,
        lifecycle_config: &LifecycleConfig,
        listener: Option<&LifecycleListener>,
    ) -> Result<LifecycleResult> {
        expiry_config.validate()?;
        lifecycle_config.validate()?;

        let mut result = LifecycleResult::default();
        let underlying_name = underlying.underlying().to_string();

        // Collect expiration data first to avoid borrowing issues during removal.
        let expirations: Vec<(ExpirationDate, Option<InstrumentStatus>)> = underlying
            .expirations()
            .iter()
            .map(|(exp, book)| {
                let status = chain_status(book.chain());
                (exp, status)
            })
            .collect();

        // Track expirations to remove after iteration.
        let mut to_remove: Vec<ExpirationDate> = Vec::new();

        for (expiration, current_status) in &expirations {
            // Skip ExpirationDate::Days variants — they are relative and
            // cannot be used for fixed lifecycle transitions.
            let exp_date = match expiration {
                ExpirationDate::DateTime(dt) => dt.date_naive(),
                _ => continue,
            };

            let (eh, em) = expiry_config.expiry_time_utc;
            let (sh, sm) = expiry_config.settlement_time_utc;

            let expiry_dt = to_datetime(exp_date, eh, em)?;
            let settle_dt = to_datetime(exp_date, sh, sm)?;
            let removal_dt = settle_dt
                .checked_add_signed(lifecycle_config.retention_period)
                .ok_or_else(|| Error::configuration("overflow computing removal datetime"))?;

            // Check transitions in reverse order (furthest-along state first).
            // Allow catching up: if the scheduler missed ticks, we can jump
            // directly to the appropriate state based on `now`.
            if let Some(status) = current_status {
                if now >= removal_dt && *status >= InstrumentStatus::Expired {
                    // Already Expired → Remove
                    to_remove.push(*expiration);
                    let event = LifecycleEvent::ExpirationRemoved {
                        underlying: underlying_name.clone(),
                        expiration: *expiration,
                    };
                    if let Some(cb) = listener {
                        cb(&event);
                    }
                    result.events.push(event);
                } else if now >= removal_dt && *status < InstrumentStatus::Expired {
                    // Full catch-up: any pre-Expired state → cancel → Settling →
                    // Expired → Remove, all in one pass.
                    let exp_book = underlying.expirations().get(expiration)?;
                    if *status < InstrumentStatus::Settling {
                        set_all_book_status(exp_book.chain(), InstrumentStatus::Settling);
                        let cancel_result = exp_book.cancel_all()?;
                        let cancelled = cancel_result.total_cancelled();
                        let settling_event = LifecycleEvent::InstrumentSettling {
                            underlying: underlying_name.clone(),
                            expiration: *expiration,
                            cancelled_orders: cancelled,
                        };
                        if let Some(cb) = listener {
                            cb(&settling_event);
                        }
                        result.events.push(settling_event);
                    }
                    set_all_book_status(exp_book.chain(), InstrumentStatus::Expired);
                    let expired_event = LifecycleEvent::InstrumentExpired {
                        underlying: underlying_name.clone(),
                        expiration: *expiration,
                    };
                    if let Some(cb) = listener {
                        cb(&expired_event);
                    }
                    result.events.push(expired_event);
                    to_remove.push(*expiration);
                    let removed_event = LifecycleEvent::ExpirationRemoved {
                        underlying: underlying_name.clone(),
                        expiration: *expiration,
                    };
                    if let Some(cb) = listener {
                        cb(&removed_event);
                    }
                    result.events.push(removed_event);
                } else if now >= settle_dt && *status < InstrumentStatus::Expired {
                    // Any state before Expired → Expired (catch up)
                    let exp_book = underlying.expirations().get(expiration)?;
                    // If still Active/Halted/Pending, halt new orders first,
                    // then cancel. This closes the window where new orders
                    // could be accepted during cancellation.
                    if *status < InstrumentStatus::Settling {
                        set_all_book_status(exp_book.chain(), InstrumentStatus::Settling);
                        let cancel_result = exp_book.cancel_all()?;
                        let cancelled = cancel_result.total_cancelled();
                        let settling_event = LifecycleEvent::InstrumentSettling {
                            underlying: underlying_name.clone(),
                            expiration: *expiration,
                            cancelled_orders: cancelled,
                        };
                        if let Some(cb) = listener {
                            cb(&settling_event);
                        }
                        result.events.push(settling_event);
                    }
                    set_all_book_status(exp_book.chain(), InstrumentStatus::Expired);
                    let event = LifecycleEvent::InstrumentExpired {
                        underlying: underlying_name.clone(),
                        expiration: *expiration,
                    };
                    if let Some(cb) = listener {
                        cb(&event);
                    }
                    result.events.push(event);
                } else if now >= expiry_dt && *status < InstrumentStatus::Settling {
                    // Active/Halted/Pending → Settling
                    let exp_book = underlying.expirations().get(expiration)?;
                    set_all_book_status(exp_book.chain(), InstrumentStatus::Settling);
                    let cancel_result = exp_book.cancel_all()?;
                    let cancelled = cancel_result.total_cancelled();
                    let event = LifecycleEvent::InstrumentSettling {
                        underlying: underlying_name.clone(),
                        expiration: *expiration,
                        cancelled_orders: cancelled,
                    };
                    if let Some(cb) = listener {
                        cb(&event);
                    }
                    result.events.push(event);
                }
            } else {
                // No strikes → nothing to transition.
                // But still check removal for empty expired expirations.
                if now >= removal_dt {
                    to_remove.push(*expiration);
                    let event = LifecycleEvent::ExpirationRemoved {
                        underlying: underlying_name.clone(),
                        expiration: *expiration,
                    };
                    if let Some(cb) = listener {
                        cb(&event);
                    }
                    result.events.push(event);
                }
            }
        }

        // Remove collected expirations.
        for exp in &to_remove {
            underlying.expirations().remove(exp);
        }

        Ok(result)
    }
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Returns the **minimum** lifecycle status across every call **and** put book
/// of every strike in the chain, or `None` if the chain has no strikes.
///
/// # Why the minimum (min-based invariant)
///
/// A chain's books may briefly disagree: a strike added after settlement began,
/// or a partial advance, can leave a single book lagging behind the rest (e.g.
/// every book is [`Settling`](InstrumentStatus::Settling) except one newly-added
/// put still [`Active`](InstrumentStatus::Active)). Sampling a single book — as
/// this used to do with the lowest strike's call — would report the chain as
/// fully advanced, and the manager would skip the transition the lagging book
/// still needs.
///
/// Taking the minimum makes any lagging book pull the reported status back to
/// its earlier state. [`InstrumentStatus`] orders earlier lifecycle states below
/// later ones (`Active` < `Halted` < `Settling` < `Expired`), so the minimum is
/// exactly the least-advanced book. That earlier status forces the manager into
/// the catch-up branch, which then drives **all** books forward via
/// [`set_all_book_status`], sweeping up the laggard.
///
/// This is sound because book status only ever advances monotonically through
/// the CAS-forward [`set_all_book_status`]; a book can never regress, so the
/// minimum can never spuriously under-report a chain that is genuinely ahead.
///
/// A `min` over a set is order-invariant, but the chain is still traversed via
/// the ordered [`SkipMap`](crossbeam_skiplist::SkipMap) so the computation is
/// deterministic by construction. The empty-chain case returns `None`,
/// preserving the prior behavior for expirations with no strikes.
#[inline]
fn chain_status(chain: &super::chain::OptionChainOrderBook) -> Option<InstrumentStatus> {
    chain
        .strikes()
        .iter()
        .flat_map(|entry| {
            let strike = entry.value();
            [strike.call().status(), strike.put().status()]
        })
        .min()
}

/// Sets the lifecycle status on all option books (call and put) across every
/// strike in the chain.
///
/// This function only moves statuses *forward* in the lifecycle using atomic
/// compare-and-swap (CAS) operations. It will not downgrade a book that has
/// already advanced further, and concurrent calls are safe.
///
/// The CAS loop ensures that even under high concurrency, status transitions
/// are monotonic and no transitions are lost or duplicated.
fn set_all_book_status(chain: &super::chain::OptionChainOrderBook, status: InstrumentStatus) {
    for entry in chain.strikes().iter() {
        let strike = entry.value();

        // CAS loop for call book: advance only if current < target
        loop {
            let current = strike.call().status();
            if current >= status {
                break; // Already at or past target status
            }
            if strike.call().compare_and_set_status(current, status) {
                break; // Successfully advanced
            }
            // CAS failed — another thread advanced it, retry to check new value
        }

        // CAS loop for put book: advance only if current < target
        loop {
            let current = strike.put().status();
            if current >= status {
                break; // Already at or past target status
            }
            if strike.put().compare_and_set_status(current, status) {
                break; // Successfully advanced
            }
            // CAS failed — another thread advanced it, retry to check new value
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::orderbook::{StrikeGenerator, StrikeRangeConfig, UnderlyingOrderBook};
    use chrono::{Duration, NaiveDate, TimeZone};
    use orderbook_rs::{OrderId, Side};

    // ── Helpers ──────────────────────────────────────────────────────────

    fn fixed_expiry_config() -> ExpiryCycleConfig {
        use crate::orderbook::{CycleRule, ExpiryType};
        ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: 1,
            }],
            expiry_time_utc: (8, 0),
            settlement_time_utc: (8, 30),
        }
    }

    fn lifecycle_config_short() -> LifecycleConfig {
        LifecycleConfig {
            retention_period: Duration::hours(1),
        }
    }

    /// Creates a fixed ExpirationDate::DateTime for a given date at 08:00 UTC.
    fn fixed_expiration(y: i32, m: u32, d: u32) -> ExpirationDate {
        let dt = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(8, 0, 0)
                .unwrap(),
        );
        ExpirationDate::DateTime(dt)
    }

    /// Creates an underlying with one expiration having strikes and orders.
    fn setup_underlying_with_orders(exp: ExpirationDate) -> UnderlyingOrderBook {
        let underlying = UnderlyingOrderBook::new("BTC");
        let exp_book = underlying.expirations().get_or_create(exp);

        // Generate strikes
        let strike_config = StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(1000)
            .min_strikes(3)
            .max_strikes(10)
            .build()
            .unwrap();
        let strikes = StrikeGenerator::generate_strikes(50000, &strike_config).unwrap();
        StrikeGenerator::apply_strikes(exp_book.chain(), &strikes);

        // Add some orders to the first strike
        let first_strike = *strikes.first().unwrap();
        let strike_book = exp_book.chain().get_strike(first_strike).unwrap();
        strike_book
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 5)
            .unwrap();
        strike_book
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 200, 3)
            .unwrap();

        underlying
    }

    // ── test_settling_transition_cancels_orders ──────────────────────────

    #[test]
    fn test_settling_transition_cancels_orders() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // Time is after expiry (08:00) but before settlement (08:30)
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 15, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        match &result.events[0] {
            LifecycleEvent::InstrumentSettling {
                underlying: u,
                cancelled_orders,
                ..
            } => {
                assert_eq!(u, "BTC");
                assert!(*cancelled_orders > 0);
            }
            other => panic!("expected InstrumentSettling, got {:?}", other),
        }

        // Verify instruments are now Settling
        let exp_book = underlying.expirations().get(&exp).unwrap();
        for entry in exp_book.chain().strikes().iter() {
            assert_eq!(entry.value().call().status(), InstrumentStatus::Settling);
            assert_eq!(entry.value().put().status(), InstrumentStatus::Settling);
        }

        // Verify orders were cancelled
        assert_eq!(exp_book.total_order_count(), 0);
    }

    // ── test_expired_transition ──────────────────────────────────────────

    #[test]
    fn test_expired_transition() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // First: transition to Settling
        let exp_book = underlying.expirations().get(&exp).unwrap();
        set_all_book_status(exp_book.chain(), InstrumentStatus::Settling);

        // Time is after settlement (08:30)
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 45, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result.events[0],
            LifecycleEvent::InstrumentExpired { .. }
        ));

        // Verify instruments are now Expired
        let exp_book = underlying.expirations().get(&exp).unwrap();
        for entry in exp_book.chain().strikes().iter() {
            assert_eq!(entry.value().call().status(), InstrumentStatus::Expired);
            assert_eq!(entry.value().put().status(), InstrumentStatus::Expired);
        }
    }

    // ── test_removal_after_retention ─────────────────────────────────────

    #[test]
    fn test_removal_after_retention() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short(); // 1 hour retention

        // Set all books to Expired
        let exp_book = underlying.expirations().get(&exp).unwrap();
        set_all_book_status(exp_book.chain(), InstrumentStatus::Expired);

        // Time is after settlement + retention (08:30 + 1h = 09:30)
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(9, 45, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result.events[0],
            LifecycleEvent::ExpirationRemoved { .. }
        ));

        // Verify expiration was removed
        assert!(underlying.expirations().get(&exp).is_err());
    }

    // ── test_full_lifecycle ──────────────────────────────────────────────

    #[test]
    fn test_full_lifecycle() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // Step 1: Active → Settling (at 08:15)
        let now1 = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 15, 0)
                .unwrap(),
        );
        let r1 = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now1,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();
        assert_eq!(r1.len(), 1);
        assert!(matches!(
            &r1.events[0],
            LifecycleEvent::InstrumentSettling { .. }
        ));

        // Step 2: Settling → Expired (at 08:45)
        let now2 = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 45, 0)
                .unwrap(),
        );
        let r2 = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now2,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();
        assert_eq!(r2.len(), 1);
        assert!(matches!(
            &r2.events[0],
            LifecycleEvent::InstrumentExpired { .. }
        ));

        // Step 3: Expired → Removed (at 09:45)
        let now3 = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(9, 45, 0)
                .unwrap(),
        );
        let r3 = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now3,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();
        assert_eq!(r3.len(), 1);
        assert!(matches!(
            &r3.events[0],
            LifecycleEvent::ExpirationRemoved { .. }
        ));

        // Expiration is gone
        assert_eq!(underlying.expirations().len(), 0);
    }

    // ── test_no_transition_before_expiry ─────────────────────────────────

    #[test]
    fn test_no_transition_before_expiry() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // Time is before expiry (07:00 < 08:00)
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(7, 0, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        assert!(result.is_empty());

        // Orders still present
        let exp_book = underlying.expirations().get(&exp).unwrap();
        assert!(exp_book.total_order_count() > 0);
    }

    // ── test_catchup_active_to_expired ──────────────────────────────────

    #[test]
    fn test_catchup_active_to_expired() {
        // Tests that when check_expirations is first called after settle_dt,
        // an Active chain catches up directly to Expired (emitting both events).
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // Verify chain is Active
        let exp_book = underlying.expirations().get(&exp).unwrap();
        assert_eq!(
            chain_status(exp_book.chain()),
            Some(InstrumentStatus::Active)
        );
        let initial_orders = exp_book.total_order_count();
        assert!(initial_orders > 0);

        // Time is after settlement (08:30) — we "missed" the 08:00 expiry tick
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 45, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        // Should have 2 events: InstrumentSettling (for order cancellation) + InstrumentExpired
        assert_eq!(result.len(), 2);
        assert!(matches!(
            &result.events[0],
            LifecycleEvent::InstrumentSettling { cancelled_orders, .. } if *cancelled_orders > 0
        ));
        assert!(matches!(
            &result.events[1],
            LifecycleEvent::InstrumentExpired { .. }
        ));

        // Verify instruments are now Expired (skipped Settling)
        let exp_book = underlying.expirations().get(&exp).unwrap();
        for entry in exp_book.chain().strikes().iter() {
            assert_eq!(entry.value().call().status(), InstrumentStatus::Expired);
            assert_eq!(entry.value().put().status(), InstrumentStatus::Expired);
        }

        // Verify orders were cancelled
        assert_eq!(exp_book.total_order_count(), 0);
    }

    // ── test_catchup_active_to_removal ─────────────────────────────────

    #[test]
    fn test_catchup_active_to_removal() {
        // Tests that when check_expirations is first called after removal_dt,
        // an Active chain catches up through Settling → Expired → Removed in one pass.
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short(); // 1h retention

        // Verify chain is Active with orders
        let exp_book = underlying.expirations().get(&exp).unwrap();
        assert_eq!(
            chain_status(exp_book.chain()),
            Some(InstrumentStatus::Active)
        );
        assert!(exp_book.total_order_count() > 0);

        // Time is well past settlement + retention (08:30 + 1h = 09:30)
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        // Should have 3 events: Settling + Expired + Removed
        assert_eq!(result.len(), 3);
        assert!(matches!(
            &result.events[0],
            LifecycleEvent::InstrumentSettling { cancelled_orders, .. } if *cancelled_orders > 0
        ));
        assert!(matches!(
            &result.events[1],
            LifecycleEvent::InstrumentExpired { .. }
        ));
        assert!(matches!(
            &result.events[2],
            LifecycleEvent::ExpirationRemoved { .. }
        ));

        // Expiration was removed
        assert!(underlying.expirations().get(&exp).is_err());
    }

    // ── test_idempotent_settling ─────────────────────────────────────────

    #[test]
    fn test_idempotent_settling() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 15, 0)
                .unwrap(),
        );

        // First call: transitions to Settling
        let r1 = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();
        assert_eq!(r1.len(), 1);

        // Second call at the same time: no new events (already Settling,
        // and now < settle_dt so no Expired transition)
        let r2 = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();
        assert!(r2.is_empty());
    }

    // ── test_idempotent_expired ──────────────────────────────────────────

    #[test]
    fn test_idempotent_expired() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // Pre-set to Settling
        let exp_book = underlying.expirations().get(&exp).unwrap();
        set_all_book_status(exp_book.chain(), InstrumentStatus::Settling);

        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 45, 0)
                .unwrap(),
        );

        // First call: Settling → Expired
        let r1 = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();
        assert_eq!(r1.len(), 1);

        // Second call at the same time: no events (already Expired,
        // and now < removal_dt so no removal)
        let r2 = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();
        assert!(r2.is_empty());
    }

    // ── test_empty_expiration_skipped ─────────────────────────────────────

    #[test]
    fn test_empty_expiration_skipped() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = UnderlyingOrderBook::new("BTC");
        // Create expiration with no strikes
        underlying.expirations().get_or_create(exp);

        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // After expiry time — but no strikes means no status to check
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 15, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        // No transition events (no strikes to transition)
        assert!(result.is_empty());
        // But the expiration still exists
        assert!(underlying.expirations().get(&exp).is_ok());
    }

    // ── test_empty_expiration_removed_after_retention ─────────────────────

    #[test]
    fn test_empty_expiration_removed_after_retention() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = UnderlyingOrderBook::new("BTC");
        underlying.expirations().get_or_create(exp);

        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // Well after settlement + retention
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        // Empty expirations past retention are removed
        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result.events[0],
            LifecycleEvent::ExpirationRemoved { .. }
        ));
        assert!(underlying.expirations().get(&exp).is_err());
    }

    // ── test_listener_receives_events ─────────────────────────────────────

    #[test]
    fn test_listener_receives_events() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        let listener: LifecycleListener = Arc::new(move |event| {
            received_clone
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(format!("{:?}", event));
        });

        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 15, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            Some(&listener),
        )
        .unwrap();

        assert_eq!(result.len(), 1);

        let received_events = received.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(received_events.len(), 1);
        assert!(received_events[0].contains("InstrumentSettling"));
    }

    // ── test_multiple_expirations_different_stages ────────────────────────

    // Regression oracle for issue #50. `optionstratlib`'s `ExpirationDate` has a
    // wall-clock-relative `Ord`/`Eq` that collides distinct `DateTime`
    // expirations (every past instant collapses to zero days) into a single
    // `SkipMap` slot. The manager now keys on the internal `ExpirationKey`
    // (deterministic, absolute), so the three distinct expirations below stay
    // distinct through their full lifecycle.
    #[test]
    fn test_multiple_expirations_different_stages() {
        let exp_active = fixed_expiration(2026, 6, 15);
        let exp_settling = fixed_expiration(2026, 3, 15);
        let exp_expired = fixed_expiration(2025, 12, 15);

        let underlying = UnderlyingOrderBook::new("BTC");
        let strike_config = StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(1000)
            .min_strikes(3)
            .max_strikes(10)
            .build()
            .unwrap();

        // Setup active expiration with orders
        let active_book = underlying.expirations().get_or_create(exp_active);
        let strikes = StrikeGenerator::generate_strikes(50000, &strike_config).unwrap();
        StrikeGenerator::apply_strikes(active_book.chain(), &strikes);
        active_book
            .chain()
            .get_strike(strikes[0])
            .unwrap()
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 5)
            .unwrap();

        // Setup settling expiration
        let settling_book = underlying.expirations().get_or_create(exp_settling);
        StrikeGenerator::apply_strikes(settling_book.chain(), &strikes);
        set_all_book_status(settling_book.chain(), InstrumentStatus::Settling);

        // Setup expired expiration
        let expired_book = underlying.expirations().get_or_create(exp_expired);
        StrikeGenerator::apply_strikes(expired_book.chain(), &strikes);
        set_all_book_status(expired_book.chain(), InstrumentStatus::Expired);

        // Pre-check: verify all 3 expirations are distinct and present
        assert_eq!(underlying.expirations().len(), 3);
        assert_eq!(
            chain_status(underlying.expirations().get(&exp_active).unwrap().chain()),
            Some(InstrumentStatus::Active)
        );
        assert_eq!(
            chain_status(underlying.expirations().get(&exp_settling).unwrap().chain()),
            Some(InstrumentStatus::Settling)
        );
        assert_eq!(
            chain_status(underlying.expirations().get(&exp_expired).unwrap().chain()),
            Some(InstrumentStatus::Expired)
        );

        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // now = 2026-06-15 08:15 — after active expiry, after settling settlement,
        // after expired retention
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 6, 15)
                .unwrap()
                .and_hms_opt(8, 15, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        // 4 events total:
        //   exp_expired (2025-12-05, Expired): already Expired → Removed  (1 event)
        //   exp_settling (2026-03-10, Settling): past removal_dt → catch-up
        //       Expired + Removed                                         (2 events)
        //   exp_active (2026-06-15, Active): past expiry_dt but not
        //       settle_dt → Settling                                      (1 event)
        assert_eq!(result.len(), 4);

        let mut settling_count = 0;
        let mut expired_count = 0;
        let mut removed_count = 0;
        for event in &result.events {
            match event {
                LifecycleEvent::InstrumentSettling { .. } => settling_count += 1,
                LifecycleEvent::InstrumentExpired { .. } => expired_count += 1,
                LifecycleEvent::ExpirationRemoved { .. } => removed_count += 1,
            }
        }
        assert_eq!(settling_count, 1);
        assert_eq!(expired_count, 1);
        assert_eq!(removed_count, 2);

        // Verify: both expired and settling expirations were removed
        assert!(underlying.expirations().get(&exp_expired).is_err());
        assert!(underlying.expirations().get(&exp_settling).is_err());
        // Active (2026-06-15) is now Settling
        let active = underlying.expirations().get(&exp_active).unwrap();
        assert_eq!(
            active
                .chain()
                .strikes()
                .iter()
                .next()
                .unwrap()
                .value()
                .call()
                .status(),
            InstrumentStatus::Settling
        );
    }

    // ── test_lagging_book_forces_transition ───────────────────────────────

    // Regression oracle for issue #61. `chain_status` previously sampled a
    // SINGLE book (the lowest strike's call). When a strike is added after
    // settlement began, that newly-added book lags behind the rest of the
    // chain and the single-sample read misrepresents the chain, so the manager
    // skips the transition the lagging book still needs.
    //
    // Here every book is advanced to `Settling` except one newly-added (higher)
    // strike whose put is left `Active`. The old single-sample read returns the
    // lowest strike's call = `Settling`, so the `now >= expiry_dt && status <
    // Settling` branch is false and NO transition fires — the `Active` put stays
    // `Active` forever (the bug). The min-based read returns `Active` (the
    // laggard), forcing the Settling transition that sweeps the lagging put
    // forward.
    //
    // Regression direction: against the old single-sample logic this test fails
    // (zero events, put stuck at `Active`); against the min-based logic it
    // passes (one `InstrumentSettling`, lagging put advanced to `Settling`).
    #[test]
    fn test_lagging_book_forces_transition() {
        let exp = fixed_expiration(2026, 3, 10);
        let underlying = setup_underlying_with_orders(exp);
        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        let exp_book = underlying.expirations().get(&exp).unwrap();

        // Advance every existing call + put book to Settling.
        set_all_book_status(exp_book.chain(), InstrumentStatus::Settling);

        // A strike added *after* settlement began. It sorts above the generated
        // range (~45000..=55000), so the lowest strike's call (the old sample)
        // stays Settling. Advance only its call; leave its put lagging at Active.
        let lagging_strike_price = 60000_u64;
        let lagging_strike = exp_book.chain().get_or_create_strike(lagging_strike_price);
        loop {
            let current = lagging_strike.call().status();
            if current >= InstrumentStatus::Settling
                || lagging_strike
                    .call()
                    .compare_and_set_status(current, InstrumentStatus::Settling)
            {
                break;
            }
        }
        assert_eq!(
            lagging_strike.put().status(),
            InstrumentStatus::Active,
            "newly-added put should still be lagging at Active"
        );

        // The old single-sample read (lowest strike's call) would report
        // Settling and skip the transition.
        let lowest_call_status = exp_book
            .chain()
            .strikes()
            .iter()
            .next()
            .unwrap()
            .value()
            .call()
            .status();
        assert_eq!(
            lowest_call_status,
            InstrumentStatus::Settling,
            "single-sample read would have reported Settling and skipped the transition"
        );

        // The min-based read reports the lagging Active status.
        assert_eq!(
            chain_status(exp_book.chain()),
            Some(InstrumentStatus::Active),
            "chain_status must report the lagging (min) status"
        );

        // Time after expiry (08:00) but before settlement (08:30).
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 10)
                .unwrap()
                .and_hms_opt(8, 15, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        // The needed Settling transition is applied (skipped by the old read).
        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result.events[0],
            LifecycleEvent::InstrumentSettling { .. }
        ));

        // The lagging put has been swept forward to Settling.
        let exp_book = underlying.expirations().get(&exp).unwrap();
        let lagging = exp_book.chain().get_strike(lagging_strike_price).unwrap();
        assert_eq!(lagging.put().status(), InstrumentStatus::Settling);
        assert_eq!(lagging.call().status(), InstrumentStatus::Settling);

        // And every book in the chain is now uniformly Settling.
        for entry in exp_book.chain().strikes().iter() {
            assert_eq!(entry.value().call().status(), InstrumentStatus::Settling);
            assert_eq!(entry.value().put().status(), InstrumentStatus::Settling);
        }
    }

    // ── test_lifecycle_config_default ─────────────────────────────────────

    #[test]
    fn test_lifecycle_config_default() {
        let config = LifecycleConfig::default();
        assert_eq!(config.retention_period, Duration::hours(24));
        assert!(config.validate().is_ok());
    }

    // ── test_lifecycle_config_negative_retention ──────────────────────────

    #[test]
    fn test_lifecycle_config_negative_retention() {
        let config = LifecycleConfig {
            retention_period: Duration::hours(-1),
        };
        assert!(config.validate().is_err());
    }

    // ── test_lifecycle_result_default ─────────────────────────────────────

    #[test]
    fn test_lifecycle_result_default() {
        let result = LifecycleResult::default();
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
    }

    // ── test_days_variant_skipped ─────────────────────────────────────────

    #[test]
    fn test_days_variant_skipped() {
        use optionstratlib::prelude::Positive;

        let underlying = UnderlyingOrderBook::new("BTC");
        let days_exp = ExpirationDate::Days(Positive::THIRTY);
        let exp_book = underlying.expirations().get_or_create(days_exp);

        // Add strikes so there's something to check
        let strike_config = StrikeRangeConfig::builder()
            .range_pct(0.10)
            .strike_interval(1000)
            .min_strikes(3)
            .max_strikes(10)
            .build()
            .unwrap();
        let strikes = StrikeGenerator::generate_strikes(50000, &strike_config).unwrap();
        StrikeGenerator::apply_strikes(exp_book.chain(), &strikes);

        let expiry_config = fixed_expiry_config();
        let lifecycle_config = lifecycle_config_short();

        // Way in the future — but Days variant should be skipped
        let now = Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2030, 1, 1)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        );

        let result = ExpiryLifecycleManager::check_expirations(
            &underlying,
            now,
            &expiry_config,
            &lifecycle_config,
            None,
        )
        .unwrap();

        // Days variant is skipped — no events
        assert!(result.is_empty());
    }
}
