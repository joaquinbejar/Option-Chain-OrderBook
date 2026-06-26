//! Greeks calculation engine module.
//!
//! This module provides [`GreeksEngine`] for recalculating Greeks using Black-Scholes
//! via `optionstratlib`, with a [`VolSurface`] trait for pluggable IV sources and
//! a listener/callback pattern for change notifications.
//!
//! ## Overview
//!
//! The engine supports:
//! - Manual recalculation of Greeks at strike or chain level
//! - Throttling to prevent excessive recalculations
//! - Callback notifications when Greeks are updated
//! - Pluggable volatility surface implementations
//!
//! ## Example
//!
//! ```
//! use option_chain_orderbook::orderbook::{
//!     GreeksEngine, FlatVolSurface, GreeksRecalcTrigger,
//! };
//! use std::sync::Arc;
//!
//! let engine = GreeksEngine::new();
//! let vol_surface = FlatVolSurface::new(0.30); // 30% IV
//!
//! // Subscribe to Greeks updates; keep the id to unsubscribe later.
//! let sub_id = engine.subscribe(Arc::new(|update| {
//!     println!("Greeks updated for strike {}", update.strike);
//! }));
//!
//! // Stop receiving updates and drop the listener.
//! assert!(engine.unsubscribe(sub_id));
//! ```

use super::index_feed::SubscriptionId;
use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use optionstratlib::greeks::Greek;
use optionstratlib::greeks::{charm, color, delta, gamma, rho, theta, vanna, vega, veta, vomma};
use optionstratlib::model::types::{OptionStyle, OptionType, Side};
use optionstratlib::prelude::Positive;
use optionstratlib::{ExpirationDate, Options};
use rust_decimal::Decimal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ─── GreeksRecalcTrigger ─────────────────────────────────────────────────────

/// Trigger reason for Greeks recalculation.
///
/// Used for logging, metrics, and to inform listeners why the recalculation
/// occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GreeksRecalcTrigger {
    /// Triggered by a change in the underlying spot price.
    PriceChange,
    /// Triggered by a change in implied volatility.
    VolChange,
    /// Triggered by time decay (passage of time).
    TimeDecay,
    /// Manually triggered by the user or system.
    Manual,
}

impl std::fmt::Display for GreeksRecalcTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PriceChange => write!(f, "price_change"),
            Self::VolChange => write!(f, "vol_change"),
            Self::TimeDecay => write!(f, "time_decay"),
            Self::Manual => write!(f, "manual"),
        }
    }
}

// ─── VolSurface Trait ────────────────────────────────────────────────────────

/// Trait for implied volatility surface lookup.
///
/// Implementations provide IV values for a given strike and option style.
/// This allows pluggable IV sources (flat, term structure, smile, etc.).
///
/// ## Thread Safety
///
/// All methods must be safe to call from any thread.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::{VolSurface, FlatVolSurface};
/// use optionstratlib::OptionStyle;
///
/// let surface = FlatVolSurface::new(0.25);
/// let iv = surface.get_iv(50000, OptionStyle::Call);
/// assert!((iv - 0.25).abs() < 0.001);
/// ```
pub trait VolSurface: Send + Sync {
    /// Returns the implied volatility for the given strike and option style.
    ///
    /// # Arguments
    ///
    /// * `strike` - Strike price in smallest units
    /// * `style` - Call or Put
    ///
    /// # Returns
    ///
    /// Implied volatility as a decimal (e.g., 0.25 for 25% IV).
    fn get_iv(&self, strike: u64, style: OptionStyle) -> f64;
}

// ─── FlatVolSurface ──────────────────────────────────────────────────────────

/// Simple flat volatility surface with a single IV value.
///
/// Returns the same IV for all strikes and option styles.
/// Useful for testing or when a uniform IV assumption is acceptable.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::{VolSurface, FlatVolSurface};
/// use optionstratlib::OptionStyle;
///
/// let surface = FlatVolSurface::new(0.30);
/// assert!((surface.get_iv(50000, OptionStyle::Call) - 0.30).abs() < 0.001);
/// assert!((surface.get_iv(60000, OptionStyle::Put) - 0.30).abs() < 0.001);
/// ```
#[derive(Debug, Clone)]
pub struct FlatVolSurface {
    /// The constant implied volatility value.
    iv: f64,
}

impl FlatVolSurface {
    /// Creates a new flat volatility surface.
    ///
    /// # Arguments
    ///
    /// * `iv` - Implied volatility as a decimal (e.g., 0.25 for 25%)
    #[must_use]
    pub const fn new(iv: f64) -> Self {
        Self { iv }
    }

    /// Returns the IV value.
    #[must_use]
    pub const fn iv(&self) -> f64 {
        self.iv
    }
}

impl VolSurface for FlatVolSurface {
    #[inline]
    fn get_iv(&self, _strike: u64, _style: OptionStyle) -> f64 {
        self.iv
    }
}

// ─── GreeksUpdate ────────────────────────────────────────────────────────────

/// Payload for Greeks update notifications.
///
/// Contains the strike price, updated Greeks for both call and put,
/// the trigger reason, and a timestamp.
#[derive(Debug, Clone)]
pub struct GreeksUpdate {
    /// Strike price in smallest units.
    pub strike: u64,
    /// Updated call option Greeks.
    pub call_greeks: Greek,
    /// Updated put option Greeks.
    pub put_greeks: Greek,
    /// Reason for the recalculation.
    pub trigger: GreeksRecalcTrigger,
    /// Timestamp of the update in nanoseconds since Unix epoch.
    pub timestamp_ns: u64,
}

/// Callback invoked when Greeks are updated.
///
/// Receives a shared reference to the [`GreeksUpdate`] to avoid cloning
/// when multiple listeners are registered.
///
/// # Panics
///
/// Listener implementations **should not panic**. Listeners are invoked
/// outside the engine's internal lock (the list is snapshotted under the lock
/// first), so a panicking listener does **not** poison the engine and does
/// **not** disable future notifications — a later notification still reaches
/// healthy listeners. Within the single notification pass in which it panics,
/// however, listeners ordered after the panicking one are skipped, so keep
/// listener bodies panic-free.
pub type GreeksUpdateListener = Arc<dyn Fn(&GreeksUpdate) + Send + Sync>;

// ─── GreeksEngine ────────────────────────────────────────────────────────────

/// Engine for recalculating option Greeks using Black-Scholes.
///
/// Provides methods to recalculate Greeks at the strike or chain level,
/// with configurable throttling and listener notifications.
///
/// ## Thread Safety
///
/// The engine is thread-safe and can be shared across threads via `Arc`.
///
/// ## Example
///
/// ```
/// use option_chain_orderbook::orderbook::GreeksEngine;
/// use std::time::Duration;
///
/// // Create with default throttle (100ms)
/// let engine = GreeksEngine::new();
///
/// // Create with custom throttle
/// let engine = GreeksEngine::with_throttle(Duration::from_millis(50));
/// ```
pub struct GreeksEngine {
    /// Registered listeners notified on each Greeks update, each keyed by the
    /// [`SubscriptionId`] handed back from [`subscribe`](Self::subscribe).
    ///
    /// Stored as an **ordered** `Vec<(SubscriptionId, GreeksUpdateListener)>`
    /// (never a `HashMap`/`DashMap`): notification order must remain
    /// deterministic — listeners fire in subscription (id) order. New
    /// subscriptions push to the back with a monotonically increasing id, and
    /// [`unsubscribe`](Self::unsubscribe) removes in place (preserving the
    /// relative order of the survivors).
    listeners: Mutex<Vec<(SubscriptionId, GreeksUpdateListener)>>,
    /// Monotonically increasing counter allocating the next [`SubscriptionId`].
    next_id: AtomicU64,
    /// Timestamp of the last recalculation in nanoseconds.
    last_recalc_ns: AtomicU64,
    /// Minimum interval between recalculations in nanoseconds.
    throttle_interval_ns: u64,
}

impl GreeksEngine {
    /// Default throttle interval: 100 milliseconds.
    const DEFAULT_THROTTLE_MS: u64 = 100;

    /// Creates a new Greeks engine with default throttle (100ms).
    #[must_use]
    pub fn new() -> Self {
        Self::with_throttle(Duration::from_millis(Self::DEFAULT_THROTTLE_MS))
    }

    /// Creates a new Greeks engine with a custom throttle interval.
    ///
    /// # Arguments
    ///
    /// * `throttle` - Minimum interval between recalculations
    #[must_use]
    pub fn with_throttle(throttle: Duration) -> Self {
        Self {
            listeners: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            last_recalc_ns: AtomicU64::new(0),
            throttle_interval_ns: throttle.as_nanos() as u64,
        }
    }

    /// Registers a listener for Greeks update notifications.
    ///
    /// Returns a [`SubscriptionId`] handle; pass it to
    /// [`unsubscribe`](Self::unsubscribe) to stop receiving updates and drop the
    /// listener (releasing any state it captured). Ids are unique and
    /// monotonically increasing within a single engine instance; listeners fire
    /// in id (subscription) order, so the notification order is deterministic.
    ///
    /// Mirrors [`IndexPriceFeed::subscribe`](super::IndexPriceFeed::subscribe):
    /// the returned id is **not** `#[must_use]` — a transient caller that never
    /// unsubscribes may ignore it.
    ///
    /// A poisoned lock (left by a panic at some other lock site) is recovered
    /// via [`std::sync::PoisonError::into_inner`] rather than silently
    /// swallowed, so a single panicking listener can never permanently disable
    /// subscription.
    pub fn subscribe(&self, listener: GreeksUpdateListener) -> SubscriptionId {
        let mut listeners = self.listeners.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("greeks listeners lock poisoned; recovering");
            poisoned.into_inner()
        });
        // Allocate the id *under* the listeners lock so id order == push order:
        // ids are handed out in the same sequence entries are appended, keeping
        // the Vec ascending by id and the notification order in id (subscription)
        // order even under concurrent `subscribe`. Allocating before the lock
        // would let a lower-id thread push after a higher-id one, reordering the
        // Vec and breaking the documented id-order delivery guarantee.
        let id = SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        listeners.push((id, listener));
        id
    }

    /// Removes a previously registered listener by its [`SubscriptionId`].
    ///
    /// Returns `true` if a listener with that id was found and removed, `false`
    /// if the id was not present (e.g. already unsubscribed, or never issued).
    /// An unsubscribed listener stops receiving updates and is dropped, freeing
    /// any state it captured.
    ///
    /// **Pass only an id returned by this engine's [`subscribe`](Self::subscribe).**
    /// [`SubscriptionId`] is a bare counter shared with other subscription
    /// sources (e.g. an [`IndexPriceFeed`](super::IndexPriceFeed)), and each
    /// source numbers from zero independently, so an id minted elsewhere is not
    /// interchangeable: it may coincidentally match — and remove — an unrelated
    /// Greeks listener. Ids are engine-scoped handles, not global.
    ///
    /// Removal is in place ([`Vec::remove`]) so the relative order of the
    /// surviving listeners — and therefore the deterministic notification order
    /// — is preserved. Mirrors
    /// [`IndexPriceFeed::unsubscribe`](super::IndexPriceFeed::unsubscribe).
    ///
    /// A poisoned lock is recovered via [`std::sync::PoisonError::into_inner`]
    /// rather than silently swallowed.
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut listeners = self.listeners.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("greeks listeners lock poisoned; recovering");
            poisoned.into_inner()
        });
        match listeners.iter().position(|(entry_id, _)| *entry_id == id) {
            Some(pos) => {
                listeners.remove(pos);
                true
            }
            None => false,
        }
    }

    /// Calculates Greeks for a single option.
    ///
    /// # Arguments
    ///
    /// * `spot` - Underlying spot price
    /// * `strike` - Strike price
    /// * `tte_years` - Time to expiry in years
    /// * `risk_free_rate` - Risk-free interest rate (e.g., 0.05 for 5%)
    /// * `iv` - Implied volatility (e.g., 0.25 for 25%)
    /// * `style` - Call or Put
    /// * `dividend_yield` - Dividend yield (e.g., 0.01 for 1%)
    ///
    /// # Returns
    ///
    /// Calculated Greeks or an error if calculation fails.
    ///
    /// # Errors
    ///
    /// Returns [`Error::GreeksError`] when an input fails validation or the
    /// underlying `optionstratlib` calculation fails. Specifically:
    ///
    /// - `spot`, `strike`, `tte_years`, or `iv` is non-positive (`<= 0.0`);
    /// - `risk_free_rate` is non-finite (`NaN` or `±∞`) — a negative rate is
    ///   permitted to support negative-rate regimes;
    /// - `dividend_yield` is non-finite or negative (a true `0.0` is valid);
    /// - any validated value cannot be converted to a `Positive` or to a
    ///   `Decimal` risk-free rate; or
    /// - any individual Greek (delta, gamma, theta, vega, rho, vanna, vomma,
    ///   veta, charm, color) fails to compute in `optionstratlib`.
    pub fn calculate_greeks(
        spot: f64,
        strike: f64,
        tte_years: f64,
        risk_free_rate: f64,
        iv: f64,
        style: OptionStyle,
        dividend_yield: f64,
    ) -> Result<Greek> {
        // Validate inputs. These are cold rejection branches: the WARN only
        // fires when an input is invalid and the Greek is skipped — the
        // success path below emits nothing here.
        if spot <= 0.0 || strike <= 0.0 || tte_years <= 0.0 || iv <= 0.0 {
            tracing::warn!(
                spot,
                strike,
                tte_years,
                iv,
                "greeks input rejected: spot, strike, tte, and iv must be positive",
            );
            return Err(Error::greeks(
                "Invalid input: spot, strike, tte, and iv must be positive",
            ));
        }
        // The risk-free rate may legitimately be negative (negative-rate
        // regimes); only a non-finite value is invalid.
        if !risk_free_rate.is_finite() {
            tracing::warn!(
                risk_free_rate,
                "greeks input rejected: risk_free_rate must be finite",
            );
            return Err(Error::greeks(
                "Invalid input: risk_free_rate must be finite",
            ));
        }
        // A true 0.0 dividend yield is the normal case for crypto options and
        // must be priced as 0.0 — only NaN/Inf or a negative value is invalid.
        if !dividend_yield.is_finite() || dividend_yield < 0.0 {
            tracing::warn!(
                dividend_yield,
                "greeks input rejected: dividend_yield must be finite and non-negative",
            );
            return Err(Error::greeks(
                "Invalid input: dividend_yield must be finite and non-negative",
            ));
        }

        // Create Positive values, handling potential failures
        let spot_pos = Positive::new(spot).map_err(|_| Error::greeks("Invalid spot price"))?;
        let strike_pos =
            Positive::new(strike).map_err(|_| Error::greeks("Invalid strike price"))?;
        let iv_pos = Positive::new(iv).map_err(|_| Error::greeks("Invalid implied volatility"))?;
        // No clamp: a 0.0 dividend yield is valid (Positive::new(0.0) succeeds —
        // the `positive` crate's non-zero feature is not enabled). Clamping to
        // 0.0001 would inject a systematic bias into every Greek for the common
        // crypto-options case of a true zero dividend.
        let div_yield =
            Positive::new(dividend_yield).map_err(|_| Error::greeks("Invalid dividend yield"))?;
        let tte_pos =
            Positive::new(tte_years).map_err(|_| Error::greeks("Invalid time to expiry"))?;

        // Build the Options struct for optionstratlib
        let option = Options {
            option_type: OptionType::European,
            side: Side::Long,
            underlying_symbol: String::new(),
            strike_price: strike_pos,
            expiration_date: ExpirationDate::Days(
                tte_pos
                    * Positive::new(365.0)
                        .map_err(|_| Error::greeks("failed to create days-per-year constant"))?,
            ),
            implied_volatility: iv_pos,
            quantity: Positive::ONE,
            underlying_price: spot_pos,
            // The rate is already guaranteed finite above; surface any
            // remaining conversion failure as a typed error rather than
            // silently returning plausible-but-wrong 5% risk numbers.
            risk_free_rate: Decimal::try_from(risk_free_rate)
                .map_err(|_| Error::greeks("Invalid risk-free rate"))?,
            option_style: style,
            dividend_yield: div_yield,
            exotic_params: None,
        };

        // Calculate all Greeks - each returns Result<Decimal, GreeksError>
        let delta_val = delta(&option)
            .map_err(|e| Error::greeks(format!("delta calculation failed: {}", e)))?;
        let gamma_val = gamma(&option)
            .map_err(|e| Error::greeks(format!("gamma calculation failed: {}", e)))?;
        let theta_val = theta(&option)
            .map_err(|e| Error::greeks(format!("theta calculation failed: {}", e)))?;
        let vega_val =
            vega(&option).map_err(|e| Error::greeks(format!("vega calculation failed: {}", e)))?;
        let rho_val =
            rho(&option).map_err(|e| Error::greeks(format!("rho calculation failed: {}", e)))?;
        let vanna_val = vanna(&option)
            .map_err(|e| Error::greeks(format!("vanna calculation failed: {}", e)))?;
        let vomma_val = vomma(&option)
            .map_err(|e| Error::greeks(format!("vomma calculation failed: {}", e)))?;
        let veta_val =
            veta(&option).map_err(|e| Error::greeks(format!("veta calculation failed: {}", e)))?;
        let charm_val = charm(&option)
            .map_err(|e| Error::greeks(format!("charm calculation failed: {}", e)))?;
        let color_val = color(&option)
            .map_err(|e| Error::greeks(format!("color calculation failed: {}", e)))?;

        // rho_d (dividend rho) is not directly available, use zero
        let rho_d_val = Decimal::ZERO;
        // alpha is not a standard Greek, use zero
        let alpha_val = Decimal::ZERO;

        Ok(Greek {
            delta: delta_val,
            gamma: gamma_val,
            theta: theta_val,
            vega: vega_val,
            rho: rho_val,
            rho_d: rho_d_val,
            alpha: alpha_val,
            vanna: vanna_val,
            vomma: vomma_val,
            veta: veta_val,
            charm: charm_val,
            color: color_val,
        })
    }

    /// Calculates Greeks for both call and put at a given strike.
    ///
    /// # Arguments
    ///
    /// * `spot` - Underlying spot price
    /// * `strike` - Strike price
    /// * `tte_years` - Time to expiry in years
    /// * `risk_free_rate` - Risk-free interest rate
    /// * `call_iv` - Call implied volatility
    /// * `put_iv` - Put implied volatility
    /// * `dividend_yield` - Dividend yield
    ///
    /// # Returns
    ///
    /// Tuple of (call_greeks, put_greeks) or an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::GreeksError`] if either the call or the put leg fails
    /// to compute. Each leg delegates to [`calculate_greeks`](Self::calculate_greeks),
    /// so the same input-validation conditions apply: non-positive `spot`,
    /// `strike`, `tte_years`, `call_iv`, or `put_iv`; non-finite `risk_free_rate`;
    /// non-finite or negative `dividend_yield`; a failed `Positive`/`Decimal`
    /// conversion; or a failure inside an individual `optionstratlib` Greek.
    /// The call leg is evaluated first, so its error short-circuits before the
    /// put leg is computed.
    pub fn calculate_strike_greeks(
        spot: f64,
        strike: f64,
        tte_years: f64,
        risk_free_rate: f64,
        call_iv: f64,
        put_iv: f64,
        dividend_yield: f64,
    ) -> Result<(Greek, Greek)> {
        let call_greeks = Self::calculate_greeks(
            spot,
            strike,
            tte_years,
            risk_free_rate,
            call_iv,
            OptionStyle::Call,
            dividend_yield,
        )?;

        let put_greeks = Self::calculate_greeks(
            spot,
            strike,
            tte_years,
            risk_free_rate,
            put_iv,
            OptionStyle::Put,
            dividend_yield,
        )?;

        Ok((call_greeks, put_greeks))
    }

    /// Notifies all registered listeners of a Greeks update.
    ///
    /// The listener list is snapshotted under the lock (cheap `Arc` clones)
    /// and the guard is dropped **before** any listener runs. This guarantees
    /// the listeners mutex is never held across a callback, so:
    ///
    /// - A listener may re-enter the engine (e.g. call
    ///   [`GreeksEngine::subscribe`]) without deadlocking the non-reentrant
    ///   `std::sync::Mutex`.
    /// - An expensive callback (e.g. a NATS publish) never blocks concurrent
    ///   subscribers.
    /// - A panicking listener unwinds outside the lock, so it cannot poison
    ///   the mutex; a later notification still reaches healthy listeners.
    ///
    /// A poisoned lock (from a panic at some other lock site) is recovered
    /// rather than silently swallowed.
    fn notify_listeners(&self, update: &GreeksUpdate) {
        // Snapshot under the lock, then drop the guard before invoking anyone.
        // Only the listener callbacks are needed for delivery; the snapshot
        // preserves the stored (id) order, so notification order stays
        // deterministic.
        let listeners: Vec<GreeksUpdateListener> = {
            let guard = self.listeners.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("greeks listeners lock poisoned; recovering");
                poisoned.into_inner()
            });
            guard
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect()
        };

        for listener in &listeners {
            listener(update);
        }
    }

    /// Records the current timestamp and notifies listeners.
    ///
    /// # Arguments
    ///
    /// * `strike` - Strike price
    /// * `call_greeks` - Updated call Greeks
    /// * `put_greeks` - Updated put Greeks
    /// * `trigger` - Recalculation trigger reason
    pub fn record_and_notify(
        &self,
        strike: u64,
        call_greeks: Greek,
        put_greeks: Greek,
        trigger: GreeksRecalcTrigger,
    ) {
        let now_ns = current_timestamp_ns();
        self.last_recalc_ns.store(now_ns, Ordering::Release);

        // Cold path: the throttled recalc tick, not the per-quote read.
        tracing::debug!(strike, trigger = %trigger, "greeks recalculated");

        let update = GreeksUpdate {
            strike,
            call_greeks,
            put_greeks,
            trigger,
            timestamp_ns: now_ns,
        };

        self.notify_listeners(&update);
    }

    /// Checks if enough time has passed since the last recalculation.
    ///
    /// # Returns
    ///
    /// `true` if throttle interval has elapsed, `false` otherwise.
    #[must_use]
    pub fn can_recalculate(&self) -> bool {
        let last = self.last_recalc_ns.load(Ordering::Acquire);
        let now = current_timestamp_ns();
        now.saturating_sub(last) >= self.throttle_interval_ns
    }

    /// Returns the throttle interval.
    #[must_use]
    pub const fn throttle_interval(&self) -> Duration {
        Duration::from_nanos(self.throttle_interval_ns)
    }

    /// Returns the timestamp of the last recalculation in nanoseconds.
    #[must_use]
    pub fn last_recalc_timestamp_ns(&self) -> u64 {
        self.last_recalc_ns.load(Ordering::Acquire)
    }
}

impl Default for GreeksEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GreeksEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GreeksEngine")
            .field("throttle_interval_ns", &self.throttle_interval_ns)
            .field(
                "last_recalc_ns",
                &self.last_recalc_ns.load(Ordering::Relaxed),
            )
            .field(
                "listener_count",
                &self
                    .listeners
                    .lock()
                    .map(|l| l.len())
                    .unwrap_or_else(|p| p.into_inner().len()),
            )
            .finish()
    }
}

// ─── Time Helpers ────────────────────────────────────────────────────────────

/// Returns the current timestamp in nanoseconds since Unix epoch.
#[inline]
fn current_timestamp_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Calculates time to expiry in years from an expiration date.
///
/// # Arguments
///
/// * `expiration` - The expiration date
///
/// # Returns
///
/// Time to expiry in years, or 0.0 if already expired.
#[must_use]
pub fn calculate_tte_years(expiration: &optionstratlib::ExpirationDate) -> f64 {
    let now = Utc::now();

    match expiration {
        ExpirationDate::Days(days) => {
            let days_f64 = days.to_f64();
            (days_f64 / 365.0).max(0.0)
        }
        ExpirationDate::DateTime(dt) => {
            let expiry: DateTime<Utc> = *dt;
            let duration = expiry.signed_duration_since(now);
            let days = duration.num_seconds() as f64 / 86400.0;
            (days / 365.0).max(0.0)
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// Builds a throwaway `Greek` value for notification tests.
    fn sample_greek() -> Greek {
        Greek {
            delta: dec!(0.5),
            gamma: dec!(0.01),
            theta: dec!(-0.05),
            vega: dec!(0.1),
            rho: dec!(0.02),
            rho_d: Decimal::ZERO,
            alpha: Decimal::ZERO,
            vanna: Decimal::ZERO,
            vomma: Decimal::ZERO,
            veta: Decimal::ZERO,
            charm: Decimal::ZERO,
            color: Decimal::ZERO,
        }
    }

    #[test]
    fn test_flat_vol_surface() {
        let surface = FlatVolSurface::new(0.25);
        assert!((surface.get_iv(50000, OptionStyle::Call) - 0.25).abs() < 0.001);
        assert!((surface.get_iv(60000, OptionStyle::Put) - 0.25).abs() < 0.001);
        assert!((surface.iv() - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_greeks_recalc_trigger_display() {
        assert_eq!(GreeksRecalcTrigger::PriceChange.to_string(), "price_change");
        assert_eq!(GreeksRecalcTrigger::VolChange.to_string(), "vol_change");
        assert_eq!(GreeksRecalcTrigger::TimeDecay.to_string(), "time_decay");
        assert_eq!(GreeksRecalcTrigger::Manual.to_string(), "manual");
    }

    #[test]
    fn test_engine_default_throttle() {
        let engine = GreeksEngine::new();
        assert_eq!(engine.throttle_interval(), Duration::from_millis(100));
    }

    #[test]
    fn test_engine_custom_throttle() {
        let engine = GreeksEngine::with_throttle(Duration::from_millis(50));
        assert_eq!(engine.throttle_interval(), Duration::from_millis(50));
    }

    #[test]
    fn test_listener_receives_updates() {
        let engine = GreeksEngine::new();
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = Arc::clone(&call_count);

        engine.subscribe(Arc::new(move |_update| {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        let greeks = Greek {
            delta: dec!(0.5),
            gamma: dec!(0.01),
            theta: dec!(-0.05),
            vega: dec!(0.1),
            rho: dec!(0.02),
            rho_d: Decimal::ZERO,
            alpha: Decimal::ZERO,
            vanna: Decimal::ZERO,
            vomma: Decimal::ZERO,
            veta: Decimal::ZERO,
            charm: Decimal::ZERO,
            color: Decimal::ZERO,
        };

        engine.record_and_notify(50000, greeks.clone(), greeks, GreeksRecalcTrigger::Manual);

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_unsubscribe_stops_one_listener_keeps_other() {
        let engine = GreeksEngine::new();

        let count_a = Arc::new(AtomicUsize::new(0));
        let count_b = Arc::new(AtomicUsize::new(0));

        let count_a_clone = Arc::clone(&count_a);
        let id_a = engine.subscribe(Arc::new(move |_update| {
            count_a_clone.fetch_add(1, Ordering::SeqCst);
        }));

        let count_b_clone = Arc::clone(&count_b);
        let _id_b = engine.subscribe(Arc::new(move |_update| {
            count_b_clone.fetch_add(1, Ordering::SeqCst);
        }));

        let greeks = sample_greek();

        // First notify reaches both listeners.
        engine.record_and_notify(
            50000,
            greeks.clone(),
            greeks.clone(),
            GreeksRecalcTrigger::Manual,
        );
        assert_eq!(count_a.load(Ordering::SeqCst), 1);
        assert_eq!(count_b.load(Ordering::SeqCst), 1);

        // Unsubscribe A — synchronous, so no further delivery can reach it.
        assert!(engine.unsubscribe(id_a));

        // Second notify: only B keeps incrementing; A is frozen.
        engine.record_and_notify(50000, greeks.clone(), greeks, GreeksRecalcTrigger::Manual);
        assert_eq!(
            count_a.load(Ordering::SeqCst),
            1,
            "unsubscribed listener must stop"
        );
        assert_eq!(
            count_b.load(Ordering::SeqCst),
            2,
            "remaining listener must keep firing"
        );

        // Unsubscribing an unknown id is a no-op returning false.
        assert!(!engine.unsubscribe(SubscriptionId(9999)));
        // Re-unsubscribing the already-removed id also returns false.
        assert!(!engine.unsubscribe(id_a));
    }

    #[test]
    fn test_calculate_greeks_call() {
        let result = GreeksEngine::calculate_greeks(
            100.0, // spot
            100.0, // strike (ATM)
            0.25,  // 3 months
            0.05,  // 5% rate
            0.20,  // 20% IV
            OptionStyle::Call,
            0.01, // 1% dividend
        );

        assert!(result.is_ok());
        let greeks = result.unwrap();

        // ATM call delta should be around 0.5
        let delta_f64 = greeks.delta.to_string().parse::<f64>().unwrap_or(0.0);
        assert!(
            delta_f64 > 0.4 && delta_f64 < 0.7,
            "Delta was {}",
            delta_f64
        );

        // Gamma should be positive
        let gamma_f64 = greeks.gamma.to_string().parse::<f64>().unwrap_or(0.0);
        assert!(gamma_f64 > 0.0, "Gamma should be positive");
    }

    #[test]
    fn test_calculate_greeks_put() {
        let result =
            GreeksEngine::calculate_greeks(100.0, 100.0, 0.25, 0.05, 0.20, OptionStyle::Put, 0.01);

        assert!(result.is_ok());
        let greeks = result.unwrap();

        // ATM put delta should be around -0.5
        let delta_f64 = greeks.delta.to_string().parse::<f64>().unwrap_or(0.0);
        assert!(
            delta_f64 < -0.3 && delta_f64 > -0.7,
            "Delta was {}",
            delta_f64
        );
    }

    #[test]
    fn test_calculate_greeks_zero_dividend_accepted() {
        // A true 0.0 dividend yield is the normal crypto-options case and must
        // be priced as 0.0 (no clamp to 0.0001 injecting bias).
        let result =
            GreeksEngine::calculate_greeks(100.0, 100.0, 0.25, 0.05, 0.20, OptionStyle::Call, 0.0);
        assert!(
            result.is_ok(),
            "0.0 dividend must be accepted: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_calculate_greeks_nonfinite_rate_rejected() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let result = GreeksEngine::calculate_greeks(
                100.0,
                100.0,
                0.25,
                bad,
                0.20,
                OptionStyle::Call,
                0.01,
            );
            assert!(result.is_err(), "non-finite rate {bad} must be rejected");
        }
    }

    #[test]
    fn test_calculate_greeks_invalid_dividend_rejected() {
        for bad in [-0.01, f64::NAN, f64::INFINITY] {
            let result = GreeksEngine::calculate_greeks(
                100.0,
                100.0,
                0.25,
                0.05,
                0.20,
                OptionStyle::Call,
                bad,
            );
            assert!(
                result.is_err(),
                "negative/non-finite dividend {bad} must be rejected"
            );
        }
    }

    #[test]
    fn test_calculate_strike_greeks() {
        let result =
            GreeksEngine::calculate_strike_greeks(100.0, 100.0, 0.25, 0.05, 0.20, 0.22, 0.01);

        assert!(result.is_ok());
        let (call, put) = result.unwrap();

        // Call delta positive, put delta negative
        let call_delta: f64 = call.delta.to_string().parse().unwrap_or(0.0);
        let put_delta: f64 = put.delta.to_string().parse().unwrap_or(0.0);
        assert!(call_delta > 0.0);
        assert!(put_delta < 0.0);
    }

    #[test]
    fn test_calculate_greeks_invalid_inputs() {
        // Zero spot
        let result =
            GreeksEngine::calculate_greeks(0.0, 100.0, 0.25, 0.05, 0.20, OptionStyle::Call, 0.01);
        assert!(result.is_err());

        // Negative IV
        let result = GreeksEngine::calculate_greeks(
            100.0,
            100.0,
            0.25,
            0.05,
            -0.20,
            OptionStyle::Call,
            0.01,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_can_recalculate_respects_throttle() {
        let engine = GreeksEngine::with_throttle(Duration::from_millis(100));

        // Initially should be able to recalculate
        assert!(engine.can_recalculate());

        // After recording, should respect throttle
        let greeks = Greek {
            delta: Decimal::ZERO,
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
        engine.record_and_notify(50000, greeks.clone(), greeks, GreeksRecalcTrigger::Manual);

        // Immediately after, should not be able to recalculate
        assert!(!engine.can_recalculate());
    }

    #[test]
    fn test_tte_calculation_days() {
        use optionstratlib::prelude::Positive;
        let expiration = ExpirationDate::Days(Positive::new(30.0).unwrap());
        let tte = calculate_tte_years(&expiration);
        // 30 days / 365 ≈ 0.082
        assert!(tte > 0.07 && tte < 0.10, "TTE was {}", tte);
    }

    #[test]
    fn test_engine_debug() {
        let engine = GreeksEngine::new();
        let debug_str = format!("{:?}", engine);
        assert!(debug_str.contains("GreeksEngine"));
        assert!(debug_str.contains("throttle_interval_ns"));
    }

    #[test]
    fn test_greeks_update_fields() {
        let update = GreeksUpdate {
            strike: 50000,
            call_greeks: Greek {
                delta: dec!(0.5),
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
            },
            put_greeks: Greek {
                delta: dec!(-0.5),
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
            },
            trigger: GreeksRecalcTrigger::PriceChange,
            timestamp_ns: 1234567890,
        };

        assert_eq!(update.strike, 50000);
        assert_eq!(update.trigger, GreeksRecalcTrigger::PriceChange);
        assert_eq!(update.timestamp_ns, 1234567890);
    }

    #[test]
    fn test_notify_listeners_reentrant_subscribe_does_not_deadlock() {
        // A listener that re-enters the engine via `subscribe` during a
        // notification must NOT deadlock the non-reentrant `std::sync::Mutex`.
        // With the snapshot-then-invoke pattern the guard is dropped before
        // the listener runs, so this completes; with a lock-across-callback
        // design it would hang forever (and time out the suite).
        let engine = Arc::new(GreeksEngine::new());
        let reentered = Arc::new(AtomicBool::new(false));

        let engine_weak = Arc::downgrade(&engine);
        let reentered_clone = Arc::clone(&reentered);
        engine.subscribe(Arc::new(move |_update| {
            if let Some(engine) = engine_weak.upgrade() {
                // Re-enter: register another listener mid-notification.
                engine.subscribe(Arc::new(|_| {}));
                reentered_clone.store(true, Ordering::SeqCst);
            }
        }));

        let greeks = sample_greek();
        engine.record_and_notify(50000, greeks.clone(), greeks, GreeksRecalcTrigger::Manual);

        assert!(
            reentered.load(Ordering::SeqCst),
            "reentrant subscribe must complete without deadlock"
        );
    }

    #[test]
    fn test_notify_listeners_panicking_listener_does_not_poison_lock() {
        // A panicking listener must not permanently disable the engine: the
        // panic unwinds outside the lock (snapshot-then-invoke), so the mutex
        // is never poisoned, and poison recovery in `subscribe`/`notify` keeps
        // later notifications working.
        let engine = GreeksEngine::new();

        // Panics on its first invocation, benign afterwards.
        let armed = Arc::new(AtomicBool::new(true));
        let armed_clone = Arc::clone(&armed);
        engine.subscribe(Arc::new(move |_update| {
            if armed_clone.swap(false, Ordering::SeqCst) {
                panic!("listener boom");
            }
        }));

        let greeks = sample_greek();

        // First notify: the listener panics. Suppress the backtrace to keep
        // test output clean and catch the unwind so the suite stays green.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let first = std::panic::catch_unwind(AssertUnwindSafe(|| {
            engine.record_and_notify(
                50000,
                greeks.clone(),
                greeks.clone(),
                GreeksRecalcTrigger::Manual,
            );
        }));
        std::panic::set_hook(prev_hook);
        assert!(
            first.is_err(),
            "the panicking listener should have unwound out of notify"
        );

        // The lock must NOT be poison-disabled: subscribe still registers ...
        let healthy_count = Arc::new(AtomicUsize::new(0));
        let healthy_clone = Arc::clone(&healthy_count);
        engine.subscribe(Arc::new(move |_update| {
            healthy_clone.fetch_add(1, Ordering::SeqCst);
        }));

        // ... and a later notification still reaches the healthy listener
        // (the previously-panicking listener is now benign).
        engine.record_and_notify(50000, greeks.clone(), greeks, GreeksRecalcTrigger::Manual);

        assert_eq!(
            healthy_count.load(Ordering::SeqCst),
            1,
            "a later notify must reach the healthy listener after a prior panic"
        );
    }
}
