//! Expiry cycle configuration module.
//!
//! This module provides [`ExpiryCycleConfig`] and [`CycleRule`] for defining
//! which expiration dates to auto-create for each underlying. The configuration
//! specifies forward-looking cycles per [`ExpiryType`] and generates concrete
//! [`ExpirationDate`] values from a reference datetime.
//!
//! Configurations are stored at the
//! [`UnderlyingOrderBook`](super::underlying::UnderlyingOrderBook) level.
//!
//! ## Architecture spec defaults
//!
//! - Daily: next 2 calendar days
//! - Weekly: next 4 Fridays
//! - Monthly: last Friday of next 3 months
//! - Quarterly: last Friday of next 4 quarter-end months (Mar/Jun/Sep/Dec)
//!
//! All expiration times default to 08:00 UTC; settlement to 08:30 UTC.

use super::strike_range::ExpiryType;
use crate::error::{Error, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use optionstratlib::ExpirationDate;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

// ─── CycleRule ───────────────────────────────────────────────────────────────

/// Rule for a single expiry cycle type.
///
/// Specifies how many forward expiration dates to generate for a given
/// [`ExpiryType`].
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::{CycleRule, ExpiryType};
///
/// let rule = CycleRule { cycle_type: ExpiryType::Weekly, count: 4 };
/// assert_eq!(rule.count, 4);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleRule {
    /// The expiry classification for which to generate dates.
    pub cycle_type: ExpiryType,
    /// Number of forward cycles to create.
    ///
    /// Must be ≥ 1 and ≤
    /// [`ExpiryCycleConfig::MAX_CYCLE_COUNT`](ExpiryCycleConfig::MAX_CYCLE_COUNT);
    /// values outside that range are rejected by
    /// [`ExpiryCycleConfig::validate`](ExpiryCycleConfig::validate).
    pub count: usize,
}

// ─── ExpiryCycleConfig ───────────────────────────────────────────────────────

/// Configures which expiration dates to auto-create for an underlying.
///
/// Defines a set of [`CycleRule`]s and the UTC times at which options expire
/// and settle. The [`generate_dates`](ExpiryCycleConfig::generate_dates) method
/// turns these rules into concrete [`ExpirationDate`] values relative to a
/// reference datetime.
///
/// # Examples
///
/// ```
/// use option_chain_orderbook::orderbook::ExpiryCycleConfig;
/// use chrono::Utc;
///
/// let config = ExpiryCycleConfig::default();
/// let dates = config.generate_dates(Utc::now()).expect("should succeed");
/// assert!(!dates.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpiryCycleConfig {
    /// The set of cycle rules to apply when generating expiration dates.
    pub cycles: Vec<CycleRule>,
    /// UTC time of day at which options expire.
    ///
    /// Modeled as a [`NaiveTime`] so an out-of-range hour/minute is
    /// unrepresentable; serialized as an `"HH:MM:SS"` string.
    pub expiry_time_utc: NaiveTime,
    /// UTC time of day at which options settle.
    ///
    /// Modeled as a [`NaiveTime`] (serialized as `"HH:MM:SS"`). Must be at or
    /// after [`expiry_time_utc`](Self::expiry_time_utc).
    pub settlement_time_utc: NaiveTime,
}

impl Default for ExpiryCycleConfig {
    fn default() -> Self {
        Self {
            cycles: vec![
                CycleRule {
                    cycle_type: ExpiryType::Daily,
                    count: 2,
                },
                CycleRule {
                    cycle_type: ExpiryType::Weekly,
                    count: 4,
                },
                CycleRule {
                    cycle_type: ExpiryType::Monthly,
                    count: 3,
                },
                CycleRule {
                    cycle_type: ExpiryType::Quarterly,
                    count: 4,
                },
            ],
            // 08:00:00 / 08:30:00 are statically valid; `unwrap_or` only guards
            // the unreachable `None` and never panics (no `expect`/`unwrap`).
            expiry_time_utc: NaiveTime::from_hms_opt(8, 0, 0).unwrap_or(NaiveTime::MIN),
            settlement_time_utc: NaiveTime::from_hms_opt(8, 30, 0).unwrap_or(NaiveTime::MIN),
        }
    }
}

impl ExpiryCycleConfig {
    /// Maximum number of forward cycles a single [`CycleRule`] may request.
    ///
    /// A deserialized `count` flows straight into `Vec::with_capacity(count)`
    /// during date generation, so an unbounded value (for example `usize::MAX`
    /// from a corrupt or hostile config) would request an allocation large
    /// enough to abort the process. This ceiling sits far above any realistic
    /// listed-expiry horizon: even an aggressive daily-expiry listing spans at
    /// most ~366 calendar days a year, and weekly/monthly/quarterly cycles are
    /// an order of magnitude smaller, so 512 leaves generous headroom while
    /// keeping the pre-allocation safely bounded.
    pub const MAX_CYCLE_COUNT: usize = 512;

    /// Validates the configuration.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if:
    /// - `cycles` is empty
    /// - Any `CycleRule.count` is zero
    /// - Any `CycleRule.count` exceeds
    ///   [`MAX_CYCLE_COUNT`](Self::MAX_CYCLE_COUNT)
    /// - Duplicate `cycle_type` values exist
    /// - `settlement_time_utc` is before `expiry_time_utc` (settlement must be
    ///   at or after expiry)
    pub fn validate(&self) -> Result<()> {
        if self.cycles.is_empty() {
            return Err(Error::configuration("cycles must not be empty"));
        }
        let mut seen = std::collections::HashSet::new();
        for rule in &self.cycles {
            if rule.count == 0 {
                return Err(Error::configuration(format!(
                    "cycle count for {:?} must be at least 1",
                    rule.cycle_type
                )));
            }
            if rule.count > Self::MAX_CYCLE_COUNT {
                return Err(Error::configuration(format!(
                    "cycle count {} for {:?} exceeds the maximum of {}",
                    rule.count,
                    rule.cycle_type,
                    Self::MAX_CYCLE_COUNT
                )));
            }
            if !seen.insert(rule.cycle_type) {
                return Err(Error::configuration(format!(
                    "duplicate cycle_type {:?} in config",
                    rule.cycle_type
                )));
            }
        }
        // Settlement must be at or after expiry on the same calendar day.
        // `NaiveTime` ordering makes this a total comparison; out-of-range
        // hour/minute values are already unrepresentable.
        if self.settlement_time_utc < self.expiry_time_utc {
            return Err(Error::configuration(format!(
                "settlement_time_utc ({}) must be at or after expiry_time_utc ({})",
                self.settlement_time_utc, self.expiry_time_utc
            )));
        }
        Ok(())
    }

    /// Generates all expiration dates for the configured cycles.
    ///
    /// Each [`CycleRule`] contributes `count` future dates of its type,
    /// computed relative to `from`. The combined list is sorted by date and
    /// deduplicated (dates that appear in multiple cycle types are kept once).
    ///
    /// # Arguments
    ///
    /// * `from` - The reference datetime. All generated dates are strictly after this.
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigurationError` if validation fails or a date
    /// computation overflows.
    ///
    /// # Examples
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::{CycleRule, ExpiryType, ExpiryCycleConfig};
    /// use chrono::{NaiveTime, Utc};
    ///
    /// let config = ExpiryCycleConfig {
    ///     cycles: vec![CycleRule { cycle_type: ExpiryType::Daily, count: 3 }],
    ///     expiry_time_utc: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
    ///     settlement_time_utc: NaiveTime::from_hms_opt(8, 30, 0).unwrap(),
    /// };
    /// let dates = config.generate_dates(Utc::now()).expect("should succeed");
    /// assert_eq!(dates.len(), 3);
    /// ```
    pub fn generate_dates(&self, from: DateTime<Utc>) -> Result<Vec<ExpirationDate>> {
        self.validate()?;

        let time = self.expiry_time_utc;

        let mut dates: Vec<ExpirationDate> = Vec::new();

        for rule in &self.cycles {
            let cycle_dates = generate_cycle_dates(from, rule, time)?;
            dates.extend(cycle_dates);
        }

        // Sort by date, then deduplicate by date (a date from one cycle
        // type may coincide with another, e.g. last-Friday-of-month = weekly Friday).
        dates.sort_unstable_by_key(|d| d.get_date().ok());
        dates.dedup_by_key(|d| d.get_date().ok());

        Ok(dates)
    }
}

// ─── Date generation helpers ─────────────────────────────────────────────────

/// Generates `rule.count` expiration dates of `rule.cycle_type` after `from`.
fn generate_cycle_dates(
    from: DateTime<Utc>,
    rule: &CycleRule,
    time: NaiveTime,
) -> Result<Vec<ExpirationDate>> {
    match rule.cycle_type {
        ExpiryType::Daily => generate_daily(from, rule.count, time),
        ExpiryType::Weekly => generate_weekly(from, rule.count, time),
        ExpiryType::Monthly => generate_monthly(from, rule.count, time),
        ExpiryType::Quarterly => generate_quarterly(from, rule.count, time),
    }
}

/// Generates `count` daily expiration dates starting the calendar day after `from`.
fn generate_daily(
    from: DateTime<Utc>,
    count: usize,
    time: NaiveTime,
) -> Result<Vec<ExpirationDate>> {
    let base = from.date_naive();
    let mut dates = Vec::with_capacity(count);
    for i in 1..=count {
        let day = base
            .checked_add_signed(Duration::days(i as i64))
            .ok_or_else(|| Error::configuration("date overflow generating daily expiry"))?;
        dates.push(to_expiration(day, time));
    }
    Ok(dates)
}

/// Generates `count` weekly expiration dates (next N Fridays after `from`).
///
/// If `from` is on a Friday before the expiry time, that Friday is included.
fn generate_weekly(
    from: DateTime<Utc>,
    count: usize,
    time: NaiveTime,
) -> Result<Vec<ExpirationDate>> {
    let mut dates = Vec::with_capacity(count);
    let mut friday = next_friday_on_or_after(from, time)?;
    for _ in 0..count {
        dates.push(to_expiration(friday, time));
        friday = friday
            .checked_add_signed(Duration::days(7))
            .ok_or_else(|| Error::configuration("date overflow advancing weekly Friday"))?;
    }
    Ok(dates)
}

/// Generates `count` monthly expiration dates (last Friday of each of the next
/// `count` months whose expiration datetime is after `from`).
fn generate_monthly(
    from: DateTime<Utc>,
    count: usize,
    time: NaiveTime,
) -> Result<Vec<ExpirationDate>> {
    let mut dates = Vec::with_capacity(count);
    let base = from.date_naive();
    let mut year = base.year();
    let mut month = base.month();

    while dates.len() < count {
        let last_fri = last_friday_of_month(year, month)?;
        let candidate_dt = to_datetime(last_fri, time);
        if candidate_dt > from {
            dates.push(to_expiration(last_fri, time));
        }
        (year, month) = advance_month(year, month)?;
    }
    Ok(dates)
}

/// Generates `count` quarterly expiration dates (last Friday of the next
/// `count` quarter-end months Mar/Jun/Sep/Dec whose expiration datetime is after `from`).
fn generate_quarterly(
    from: DateTime<Utc>,
    count: usize,
    time: NaiveTime,
) -> Result<Vec<ExpirationDate>> {
    let mut dates = Vec::with_capacity(count);
    let base = from.date_naive();
    let mut year = base.year();
    let mut q_month = quarter_end_month(base.month());

    while dates.len() < count {
        let last_fri = last_friday_of_month(year, q_month)?;
        let candidate_dt = to_datetime(last_fri, time);
        if candidate_dt > from {
            dates.push(to_expiration(last_fri, time));
        }
        (year, q_month) = advance_quarter(year, q_month)?;
    }
    Ok(dates)
}

// ─── Date arithmetic helpers ──────────────────────────────────────────────────

/// Returns the next Friday on or after `from`, considering expiry time.
///
/// If `from` is on a Friday and before the expiry time, returns that Friday.
/// Otherwise, returns the next Friday.
fn next_friday_on_or_after(from: DateTime<Utc>, time: NaiveTime) -> Result<NaiveDate> {
    let date = from.date_naive();
    let weekday_num = date.weekday().num_days_from_monday() as i64;

    // If today is Friday (4), check if we're before expiry time
    if weekday_num == 4 {
        let expiry_dt = to_datetime(date, time);
        if from < expiry_dt {
            return Ok(date);
        }
    }

    // Days until the *next* Friday (strictly after today):
    let days = (4 - weekday_num + 7) % 7;
    let days = if days == 0 { 7 } else { days };
    date.checked_add_signed(Duration::days(days))
        .ok_or_else(|| Error::configuration("date overflow finding next Friday"))
}

/// Returns the last Friday of the given `year`/`month`.
fn last_friday_of_month(year: i32, month: u32) -> Result<NaiveDate> {
    let last_day = last_day_of_month(year, month)?;
    let last_date = NaiveDate::from_ymd_opt(year, month, last_day)
        .ok_or_else(|| Error::configuration("invalid last date of month"))?;

    // Days past the last Friday: walk back at most 6 days.
    // num_days_from_monday() for Fri is 4.
    let weekday_num = last_date.weekday().num_days_from_monday() as i64;
    let days_back = (weekday_num - 4 + 7) % 7;

    last_date
        .checked_sub_signed(Duration::days(days_back))
        .ok_or_else(|| Error::configuration("date underflow finding last Friday of month"))
}

/// Returns the last day (day number) of the given `year`/`month`.
fn last_day_of_month(year: i32, month: u32) -> Result<u32> {
    let (next_year, next_month) = if month == 12 {
        (
            year.checked_add(1)
                .ok_or_else(|| Error::configuration("year overflow in last_day_of_month"))?,
            1u32,
        )
    } else {
        (year, month + 1)
    };
    let first_of_next = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .ok_or_else(|| Error::configuration("invalid date in last_day_of_month"))?;
    let last_of_month = first_of_next
        .checked_sub_signed(Duration::days(1))
        .ok_or_else(|| Error::configuration("date underflow in last_day_of_month"))?;
    Ok(last_of_month.day())
}

/// Returns the quarter-end month (3, 6, 9, or 12) for the given `month`.
///
/// Q1→3, Q2→6, Q3→9, Q4→12.
fn quarter_end_month(month: u32) -> u32 {
    match month {
        1..=3 => 3,
        4..=6 => 6,
        7..=9 => 9,
        _ => 12,
    }
}

/// Advances to the next month, wrapping year if needed.
fn advance_month(year: i32, month: u32) -> Result<(i32, u32)> {
    if month == 12 {
        Ok((
            year.checked_add(1)
                .ok_or_else(|| Error::configuration("year overflow advancing month"))?,
            1,
        ))
    } else {
        Ok((year, month + 1))
    }
}

/// Advances to the next quarter-end month (Mar→Jun→Sep→Dec→Mar…).
fn advance_quarter(year: i32, q_month: u32) -> Result<(i32, u32)> {
    if q_month == 12 {
        Ok((
            year.checked_add(1)
                .ok_or_else(|| Error::configuration("year overflow advancing quarter"))?,
            3,
        ))
    } else {
        Ok((year, q_month + 3))
    }
}

/// Combines a [`NaiveDate`] and a [`NaiveTime`] into a UTC [`DateTime`].
///
/// Infallible: a [`NaiveTime`] is always a valid time-of-day, so no
/// hour/minute validation is needed.
#[must_use]
pub(crate) fn to_datetime(date: NaiveDate, time: NaiveTime) -> DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_time(time))
}

/// Combines a [`NaiveDate`] and a [`NaiveTime`] into an `ExpirationDate::DateTime`.
fn to_expiration(date: NaiveDate, time: NaiveTime) -> ExpirationDate {
    ExpirationDate::DateTime(to_datetime(date, time))
}

// ─── SharedExpiryCycleConfig ──────────────────────────────────────────────────

/// Thread-safe container for an optional [`ExpiryCycleConfig`].
///
/// Wraps `Option<ExpiryCycleConfig>` in a [`RwLock`] so that
/// [`UnderlyingOrderBook`](super::underlying::UnderlyingOrderBook) can store
/// and update the config without requiring `&mut self`.
pub(crate) struct SharedExpiryCycleConfig {
    /// Inner config, protected by a read-write lock.
    inner: RwLock<Option<ExpiryCycleConfig>>,
}

impl SharedExpiryCycleConfig {
    /// Creates a new empty shared expiry cycle config.
    #[inline]
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Stores a new config, replacing any existing one.
    ///
    /// Recovers from a poisoned lock to ensure the config is always written.
    pub(crate) fn set(&self, config: ExpiryCycleConfig) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(config);
    }

    /// Returns a clone of the stored config, or `None` if unset.
    ///
    /// Recovers from a poisoned lock to avoid silently returning `None`.
    #[must_use]
    pub(crate) fn get(&self) -> Option<ExpiryCycleConfig> {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    }

    /// Clears the stored config.
    pub(crate) fn clear(&self) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = None;
    }
}

impl Default for SharedExpiryCycleConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::strike_range::ExpiryType;
    use super::*;
    use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Timelike, Utc, Weekday};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid test date")
    }

    fn time(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).expect("valid test time")
    }

    fn dt(y: i32, mo: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(y, mo, d)
                .expect("valid date")
                .and_hms_opt(h, min, 0)
                .expect("valid time"),
        )
    }

    fn single_cycle_config(cycle_type: ExpiryType, count: usize) -> ExpiryCycleConfig {
        ExpiryCycleConfig {
            cycles: vec![CycleRule { cycle_type, count }],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        }
    }

    fn expiration_naive(exp: &ExpirationDate) -> NaiveDate {
        exp.get_date()
            .expect("test date should be valid")
            .date_naive()
    }

    // ── Default ──────────────────────────────────────────────────────────────

    #[test]
    fn test_default_config_values() {
        let config = ExpiryCycleConfig::default();
        assert_eq!(config.cycles.len(), 4);
        assert_eq!(config.expiry_time_utc, time(8, 0));
        assert_eq!(config.settlement_time_utc, time(8, 30));

        let daily = config
            .cycles
            .iter()
            .find(|r| r.cycle_type == ExpiryType::Daily)
            .unwrap();
        let weekly = config
            .cycles
            .iter()
            .find(|r| r.cycle_type == ExpiryType::Weekly)
            .unwrap();
        let monthly = config
            .cycles
            .iter()
            .find(|r| r.cycle_type == ExpiryType::Monthly)
            .unwrap();
        let quarterly = config
            .cycles
            .iter()
            .find(|r| r.cycle_type == ExpiryType::Quarterly)
            .unwrap();

        assert_eq!(daily.count, 2);
        assert_eq!(weekly.count, 4);
        assert_eq!(monthly.count, 3);
        assert_eq!(quarterly.count, 4);
    }

    // ── validate ─────────────────────────────────────────────────────────────

    #[test]
    fn test_validate_ok() {
        let config = ExpiryCycleConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_cycles() {
        let config = ExpiryCycleConfig {
            cycles: vec![],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_zero_count() {
        let config = ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: 0,
            }],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_over_limit_count() {
        // A `count` above `MAX_CYCLE_COUNT` must be rejected before it can flow
        // into `Vec::with_capacity` and request an aborting allocation.
        let config = ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: ExpiryCycleConfig::MAX_CYCLE_COUNT + 1,
            }],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        };
        let err = config.validate().expect_err("over-limit count must fail");
        assert!(matches!(err, Error::ConfigurationError { .. }));
        assert!(err.to_string().contains("exceeds the maximum"));
    }

    #[test]
    fn test_validate_max_limit_count_ok() {
        // Exactly at the ceiling is still valid (the bound is inclusive).
        let config = ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: ExpiryCycleConfig::MAX_CYCLE_COUNT,
            }],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_duplicate_cycle_type() {
        let config = ExpiryCycleConfig {
            cycles: vec![
                CycleRule {
                    cycle_type: ExpiryType::Weekly,
                    count: 2,
                },
                CycleRule {
                    cycle_type: ExpiryType::Weekly,
                    count: 3,
                },
            ],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_validate_settlement_before_expiry() {
        // Settlement strictly before expiry must be rejected with the typed
        // configuration error.
        let config = ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: 1,
            }],
            expiry_time_utc: time(8, 30),
            settlement_time_utc: time(8, 0),
        };
        let err = config
            .validate()
            .expect_err("settlement before expiry must fail");
        assert!(matches!(err, Error::ConfigurationError { .. }));
        assert!(err.to_string().contains("at or after"));
    }

    #[test]
    fn test_validate_settlement_equals_expiry_ok() {
        // Settlement at the same instant as expiry is allowed (at-or-after).
        let config = ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: 1,
            }],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 0),
        };
        assert!(config.validate().is_ok());
    }

    // ── last_day_of_month ────────────────────────────────────────────────────

    #[test]
    fn test_last_day_of_month_january() {
        assert_eq!(last_day_of_month(2026, 1).expect("ok"), 31);
    }

    #[test]
    fn test_last_day_of_month_april() {
        assert_eq!(last_day_of_month(2026, 4).expect("ok"), 30);
    }

    #[test]
    fn test_last_day_of_month_february_non_leap() {
        assert_eq!(last_day_of_month(2025, 2).expect("ok"), 28);
    }

    #[test]
    fn test_last_day_of_month_february_leap() {
        assert_eq!(last_day_of_month(2024, 2).expect("ok"), 29);
    }

    #[test]
    fn test_last_day_of_month_december() {
        assert_eq!(last_day_of_month(2026, 12).expect("ok"), 31);
    }

    // ── last_friday_of_month ─────────────────────────────────────────────────

    #[test]
    fn test_last_friday_of_march_2026() {
        // March 2026: last day is Tue 31; last Friday is 2026-03-27
        let fri = last_friday_of_month(2026, 3).expect("ok");
        assert_eq!(fri.weekday(), Weekday::Fri);
        assert_eq!(fri, date(2026, 3, 27));
    }

    #[test]
    fn test_last_friday_of_june_2026() {
        // June 2026: last day is Tue 30; last Friday is 2026-06-26
        let fri = last_friday_of_month(2026, 6).expect("ok");
        assert_eq!(fri.weekday(), Weekday::Fri);
        assert_eq!(fri, date(2026, 6, 26));
    }

    #[test]
    fn test_last_friday_of_december_2026() {
        // December 2026: last day is Thu 31; last Friday is 2026-12-25
        let fri = last_friday_of_month(2026, 12).expect("ok");
        assert_eq!(fri.weekday(), Weekday::Fri);
        assert_eq!(fri, date(2026, 12, 25));
    }

    #[test]
    fn test_last_friday_of_february_leap_2028() {
        // Feb 2028 has 29 days (Wed); last Friday is 2028-02-25
        let fri = last_friday_of_month(2028, 2).expect("ok");
        assert_eq!(fri.weekday(), Weekday::Fri);
    }

    // ── quarter_end_month ────────────────────────────────────────────────────

    #[test]
    fn test_quarter_end_months() {
        assert_eq!(quarter_end_month(1), 3);
        assert_eq!(quarter_end_month(2), 3);
        assert_eq!(quarter_end_month(3), 3);
        assert_eq!(quarter_end_month(4), 6);
        assert_eq!(quarter_end_month(6), 6);
        assert_eq!(quarter_end_month(7), 9);
        assert_eq!(quarter_end_month(9), 9);
        assert_eq!(quarter_end_month(10), 12);
        assert_eq!(quarter_end_month(12), 12);
    }

    // ── generate_dates — Daily ────────────────────────────────────────────────

    #[test]
    fn test_daily_count_correct() {
        let config = single_cycle_config(ExpiryType::Daily, 3);
        let from = dt(2026, 3, 6, 12, 0);
        let dates = config.generate_dates(from).expect("ok");
        assert_eq!(dates.len(), 3);
    }

    #[test]
    fn test_daily_dates_consecutive() {
        let config = single_cycle_config(ExpiryType::Daily, 3);
        let from = dt(2026, 3, 6, 12, 0);
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        assert_eq!(naive[0], date(2026, 3, 7));
        assert_eq!(naive[1], date(2026, 3, 8));
        assert_eq!(naive[2], date(2026, 3, 9));
    }

    #[test]
    fn test_daily_uses_expiry_time() {
        let config = single_cycle_config(ExpiryType::Daily, 1);
        let from = dt(2026, 3, 6, 12, 0);
        let dates = config.generate_dates(from).expect("ok");
        if let ExpirationDate::DateTime(dt) = dates[0] {
            assert_eq!(dt.hour(), 8);
            assert_eq!(dt.minute(), 0);
        } else {
            panic!("expected DateTime variant");
        }
    }

    #[test]
    fn test_daily_year_boundary() {
        let config = single_cycle_config(ExpiryType::Daily, 3);
        let from = dt(2025, 12, 30, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        assert_eq!(naive[0], date(2025, 12, 31));
        assert_eq!(naive[1], date(2026, 1, 1));
        assert_eq!(naive[2], date(2026, 1, 2));
    }

    // ── generate_dates — Weekly ───────────────────────────────────────────────

    #[test]
    fn test_weekly_count_correct() {
        let config = single_cycle_config(ExpiryType::Weekly, 4);
        let from = dt(2026, 3, 6, 12, 0);
        let dates = config.generate_dates(from).expect("ok");
        assert_eq!(dates.len(), 4);
    }

    #[test]
    fn test_weekly_all_fridays() {
        let config = single_cycle_config(ExpiryType::Weekly, 4);
        let from = dt(2026, 3, 6, 12, 0);
        let dates = config.generate_dates(from).expect("ok");
        for d in &dates {
            assert_eq!(expiration_naive(d).weekday(), Weekday::Fri);
        }
    }

    #[test]
    fn test_weekly_from_friday_skips_to_next() {
        // from is a Friday after expiry time — next weekly expiry should be the following Friday
        let config = single_cycle_config(ExpiryType::Weekly, 1);
        let from = dt(2026, 3, 6, 12, 0); // 2026-03-06 is a Friday, 12:00 > 08:00 expiry
        let dates = config.generate_dates(from).expect("ok");
        assert_eq!(expiration_naive(&dates[0]), date(2026, 3, 13));
    }

    #[test]
    fn test_weekly_from_friday_before_expiry_includes_today() {
        // from is a Friday before expiry time — that Friday should be included
        let config = single_cycle_config(ExpiryType::Weekly, 2);
        let from = dt(2026, 3, 6, 7, 0); // 2026-03-06 is a Friday, 07:00 < 08:00 expiry
        let dates = config.generate_dates(from).expect("ok");
        assert_eq!(expiration_naive(&dates[0]), date(2026, 3, 6)); // same day!
        assert_eq!(expiration_naive(&dates[1]), date(2026, 3, 13));
    }

    #[test]
    fn test_weekly_consecutive_fridays() {
        let config = single_cycle_config(ExpiryType::Weekly, 3);
        let from = dt(2026, 3, 2, 8, 0); // Monday
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        assert_eq!(naive[0], date(2026, 3, 6));
        assert_eq!(naive[1], date(2026, 3, 13));
        assert_eq!(naive[2], date(2026, 3, 20));
    }

    // ── generate_dates — Monthly ──────────────────────────────────────────────

    #[test]
    fn test_monthly_count_correct() {
        let config = single_cycle_config(ExpiryType::Monthly, 3);
        let from = dt(2026, 3, 6, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        assert_eq!(dates.len(), 3);
    }

    #[test]
    fn test_monthly_all_fridays() {
        let config = single_cycle_config(ExpiryType::Monthly, 4);
        let from = dt(2026, 1, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        for d in &dates {
            assert_eq!(expiration_naive(d).weekday(), Weekday::Fri);
        }
    }

    #[test]
    fn test_monthly_skips_past_last_friday() {
        // from is 2026-03-28, which is after the last Friday of March (2026-03-27)
        // so the first monthly date should be April's last Friday
        let config = single_cycle_config(ExpiryType::Monthly, 2);
        let from = dt(2026, 3, 28, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        // First should be last Friday of April 2026
        let apr_last_fri = last_friday_of_month(2026, 4).expect("ok");
        assert_eq!(naive[0], apr_last_fri);
    }

    #[test]
    fn test_monthly_from_last_friday_before_expiry_includes_today() {
        // from is 2026-03-27 (last Friday of March) at 07:00, before 08:00 expiry
        // That same day should be included
        let config = single_cycle_config(ExpiryType::Monthly, 2);
        let from = dt(2026, 3, 27, 7, 0);
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        assert_eq!(naive[0], date(2026, 3, 27)); // same day!
        let apr_last_fri = last_friday_of_month(2026, 4).expect("ok");
        assert_eq!(naive[1], apr_last_fri);
    }

    #[test]
    fn test_monthly_year_boundary() {
        let config = single_cycle_config(ExpiryType::Monthly, 3);
        let from = dt(2026, 11, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        // Should cover Nov 2026, Dec 2026, Jan 2027
        assert_eq!(naive[0].year(), 2026);
        assert_eq!(naive[0].month(), 11);
        assert_eq!(naive[1].year(), 2026);
        assert_eq!(naive[1].month(), 12);
        assert_eq!(naive[2].year(), 2027);
        assert_eq!(naive[2].month(), 1);
    }

    // ── generate_dates — Quarterly ────────────────────────────────────────────

    #[test]
    fn test_quarterly_count_correct() {
        let config = single_cycle_config(ExpiryType::Quarterly, 4);
        let from = dt(2026, 1, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        assert_eq!(dates.len(), 4);
    }

    #[test]
    fn test_quarterly_all_fridays() {
        let config = single_cycle_config(ExpiryType::Quarterly, 4);
        let from = dt(2026, 1, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        for d in &dates {
            assert_eq!(expiration_naive(d).weekday(), Weekday::Fri);
        }
    }

    #[test]
    fn test_quarterly_months_are_quarter_ends() {
        let config = single_cycle_config(ExpiryType::Quarterly, 4);
        let from = dt(2026, 1, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        let quarter_end_months = [3u32, 6, 9, 12];
        for d in &naive {
            assert!(
                quarter_end_months.contains(&d.month()),
                "Expected quarter-end month, got {}",
                d.month()
            );
        }
    }

    #[test]
    fn test_quarterly_starts_from_current_quarter() {
        // from Jan 2026 — first quarter-end is Mar 2026
        let config = single_cycle_config(ExpiryType::Quarterly, 1);
        let from = dt(2026, 1, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        assert_eq!(expiration_naive(&dates[0]).month(), 3);
    }

    #[test]
    fn test_quarterly_year_boundary() {
        let config = single_cycle_config(ExpiryType::Quarterly, 2);
        let from = dt(2026, 12, 26, 8, 0); // after last Friday of Dec 2026
        let dates = config.generate_dates(from).expect("ok");
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        // Should wrap to Mar 2027 and Jun 2027
        assert_eq!(naive[0].year(), 2027);
        assert_eq!(naive[0].month(), 3);
        assert_eq!(naive[1].year(), 2027);
        assert_eq!(naive[1].month(), 6);
    }

    // ── generate_dates — combined ─────────────────────────────────────────────

    #[test]
    fn test_combined_sorted_no_duplicates() {
        let config = ExpiryCycleConfig::default();
        let from = dt(2026, 1, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");

        // Verify sorted order
        let naive: Vec<NaiveDate> = dates.iter().map(expiration_naive).collect();
        for window in naive.windows(2) {
            assert!(
                window[0] <= window[1],
                "dates not sorted: {:?} > {:?}",
                window[0],
                window[1]
            );
        }

        // Verify no duplicate dates
        let unique: std::collections::HashSet<NaiveDate> = naive.iter().cloned().collect();
        assert_eq!(unique.len(), naive.len(), "duplicate dates found");
    }

    #[test]
    fn test_combined_total_count_at_least_max_cycle() {
        let config = ExpiryCycleConfig::default();
        let from = dt(2026, 1, 1, 8, 0);
        let dates = config.generate_dates(from).expect("ok");
        // Default: 2 daily + 4 weekly + 3 monthly + 4 quarterly = 13 minus any overlaps
        // We just verify we have at least the max single-cycle count (4)
        assert!(dates.len() >= 4);
    }

    // ── Serialization ─────────────────────────────────────────────────────────

    #[test]
    fn test_expiry_cycle_config_json_roundtrip() {
        let config = ExpiryCycleConfig::default();
        let json = serde_json::to_string(&config).expect("serialize");
        let decoded: ExpiryCycleConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(config, decoded);
    }

    #[test]
    fn test_expiry_cycle_config_naivetime_serde_shape() {
        // The DTO models the times as `NaiveTime`, which serdes as an
        // "HH:MM:SS" string (not the old `[hour, minute]` tuple).
        let config = ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: 1,
            }],
            expiry_time_utc: time(8, 0),
            settlement_time_utc: time(8, 30),
        };
        let json = serde_json::to_string(&config).expect("serialize");
        assert!(
            json.contains("\"expiry_time_utc\":\"08:00:00\""),
            "expected HH:MM:SS string for expiry_time_utc, got {json}"
        );
        assert!(
            json.contains("\"settlement_time_utc\":\"08:30:00\""),
            "expected HH:MM:SS string for settlement_time_utc, got {json}"
        );
        let decoded: ExpiryCycleConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(config, decoded);
    }

    #[test]
    fn test_cycle_rule_json_roundtrip() {
        let rule = CycleRule {
            cycle_type: ExpiryType::Quarterly,
            count: 4,
        };
        let json = serde_json::to_string(&rule).expect("serialize");
        let decoded: CycleRule = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(rule, decoded);
    }

    // ── SharedExpiryCycleConfig ───────────────────────────────────────────────

    #[test]
    fn test_shared_initially_none() {
        let shared = SharedExpiryCycleConfig::new();
        assert!(shared.get().is_none());
    }

    #[test]
    fn test_shared_set_and_get() {
        let shared = SharedExpiryCycleConfig::new();
        let config = ExpiryCycleConfig::default();
        shared.set(config.clone());
        assert_eq!(shared.get(), Some(config));
    }

    #[test]
    fn test_shared_overwrite() {
        let shared = SharedExpiryCycleConfig::new();
        shared.set(ExpiryCycleConfig::default());
        let custom = ExpiryCycleConfig {
            cycles: vec![CycleRule {
                cycle_type: ExpiryType::Daily,
                count: 5,
            }],
            expiry_time_utc: time(9, 0),
            settlement_time_utc: time(9, 30),
        };
        shared.set(custom.clone());
        assert_eq!(shared.get(), Some(custom));
    }

    #[test]
    fn test_shared_clear() {
        let shared = SharedExpiryCycleConfig::new();
        shared.set(ExpiryCycleConfig::default());
        shared.clear();
        assert!(shared.get().is_none());
    }

    #[test]
    fn test_shared_default_is_none() {
        let shared = SharedExpiryCycleConfig::default();
        assert!(shared.get().is_none());
    }
}
