//! Internal ordered key for `ExpirationDate`-keyed `SkipMap`s.
//!
//! This module exists solely to give the expiration-level managers a *correct*
//! ordered map key. It is dependency-light (only `chrono` and `optionstratlib`)
//! and sits below the hierarchy layers that use it, so both `chain.rs` and
//! `expiration.rs` can depend on it without inverting the layering.

use chrono::{DateTime, Utc};
use optionstratlib::ExpirationDate;
use optionstratlib::prelude::Positive;

/// Total-order key derived from an [`ExpirationDate`] for use as a `SkipMap` key.
///
/// # Why this exists
///
/// `optionstratlib`'s `ExpirationDate` (re-exported from the `expiration_date`
/// crate) has a wall-clock-relative `Ord`/`Eq`: comparisons route both sides
/// through `ExpirationDate::get_days()`, which for the `DateTime` variant
/// computes `dt.signed_duration_since(Utc::now())` and **clamps every instant
/// in the past to zero**. As a result:
///
/// * every already-expired `DateTime` collapses to the same comparison value
///   (zero days) and collides into a single ordered-map slot — silently
///   overwriting the previously inserted expiration and dropping its strikes
///   and orders;
/// * ordering is non-deterministic because it depends on `Utc::now()` at the
///   moment of comparison (a hazard for replay and event-emission order); and
/// * `get_days()` mutates process-global thread-local state on every call,
///   which is an unacceptable side effect for a map-key comparison.
///
/// `ExpirationKey` mirrors the two `ExpirationDate` representations with a
/// correct, deterministic, clock-independent total order:
///
/// * [`ExpirationKey::Days`] keeps the relative fractional-day count as a
///   [`Positive`] and orders by it. `Positive` already excludes `NaN` by
///   construction, so no `f64`→key conversion and no NaN can leak in here.
/// * [`ExpirationKey::DateTime`] keeps the absolute UTC instant and orders by
///   it via `chrono`'s correct, integral `Ord`.
///
/// # Ordering
///
/// The two variants occupy disjoint key spaces — a relative day-count and an
/// absolute instant are not comparable on a single axis — so the derived
/// lexicographic order places all `Days` keys before all `DateTime` keys.
/// Within each variant the order is strictly chronological (more days / later
/// instant sorts later). This matches the way the hierarchy already treats the
/// two representations: lifecycle transitions operate only on `DateTime`
/// expirations and skip `Days` ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ExpirationKey {
    /// Relative expiration measured in fractional days.
    Days(Positive),
    /// Absolute expiration instant in UTC.
    DateTime(DateTime<Utc>),
}

impl From<&ExpirationDate> for ExpirationKey {
    /// Builds the deterministic ordered key for an [`ExpirationDate`].
    ///
    /// The conversion is structural and clock-independent: it never calls
    /// `Utc::now()` and never touches `ExpirationDate::get_days()`.
    #[inline]
    fn from(expiration: &ExpirationDate) -> Self {
        match expiration {
            ExpirationDate::Days(days) => Self::Days(*days),
            ExpirationDate::DateTime(dt) => Self::DateTime(*dt),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use optionstratlib::prelude::pos_or_panic;

    fn datetime(year: i32, month: u32, day: u32) -> ExpirationDate {
        let dt = match Utc.with_ymd_and_hms(year, month, day, 8, 0, 0) {
            chrono::offset::LocalResult::Single(dt) => dt,
            _ => panic!("invalid fixture datetime"),
        };
        ExpirationDate::DateTime(dt)
    }

    #[test]
    fn test_expiration_key_same_day_of_month_distinct_months_distinct_keys() {
        // Regression: the upstream `Ord` collapses these toward a common
        // day-count scale; the key must keep them distinct.
        let jan = ExpirationKey::from(&datetime(2026, 1, 15));
        let feb = ExpirationKey::from(&datetime(2026, 2, 15));
        assert_ne!(jan, feb);
        assert!(jan < feb);
    }

    #[test]
    fn test_expiration_key_past_datetimes_distinct_keys() {
        // Both are in the past; the upstream `get_days()` would clamp both to
        // zero and collide them. The key must keep them distinct.
        let a = ExpirationKey::from(&datetime(2000, 1, 1));
        let b = ExpirationKey::from(&datetime(2001, 1, 1));
        assert_ne!(a, b);
        assert!(a < b);
    }

    #[test]
    fn test_expiration_key_days_orders_chronologically() {
        let d30 = ExpirationKey::from(&ExpirationDate::Days(pos_or_panic!(30.0)));
        let d60 = ExpirationKey::from(&ExpirationDate::Days(pos_or_panic!(60.0)));
        assert_ne!(d30, d60);
        assert!(d30 < d60);
    }

    #[test]
    fn test_expiration_key_days_sort_before_datetime() {
        let days = ExpirationKey::from(&ExpirationDate::Days(pos_or_panic!(9999.0)));
        let dt = ExpirationKey::from(&datetime(2000, 1, 1));
        assert!(days < dt);
    }

    #[test]
    fn test_expiration_key_same_expiration_equal_keys() {
        let a = ExpirationKey::from(&datetime(2026, 3, 21));
        let b = ExpirationKey::from(&datetime(2026, 3, 21));
        assert_eq!(a, b);
    }
}
