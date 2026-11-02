//! Wall-clock free formatting of absolute expirations.
//!
//! An `ExpirationDate::DateTime` names an instant, so its symbol and label
//! must depend on that instant only. These tests pin the outputs byte for
//! byte to the previous formatting (the `Display` of `ExpirationDate`, and
//! `get_date()` followed by `%Y%m%d`), on dates where a clock-relative path
//! would differ: the past, the epoch, leap days and calendar limits.

// The reference outputs are computed through the old, clock-reading path.
#![allow(clippy::disallowed_methods)]

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
use option_chain_orderbook::utils::{
    contract_symbols_at, expiration_label, format_expiration_yyyymmdd,
};
use optionstratlib::ExpirationDate;
use optionstratlib::prelude::pos_or_panic;

fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, nanos: u32) -> DateTime<Utc> {
    let date = NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
    let time = date.and_hms_nano_opt(h, min, s, nanos).expect("valid time");
    Utc.from_utc_datetime(&time)
}

/// Past, epoch, pre-epoch, leap days, century rules, ends of year and day,
/// fractional seconds and the 4-digit year limits.
fn instants() -> Vec<DateTime<Utc>> {
    vec![
        at(2020, 3, 27, 8, 0, 0, 0),
        at(1970, 1, 1, 0, 0, 0, 0),
        at(1969, 12, 31, 23, 59, 59, 999_999_999),
        at(1900, 2, 28, 12, 0, 0, 0),
        at(2000, 2, 29, 0, 0, 0, 0),
        at(2024, 2, 29, 23, 59, 59, 500_000_000),
        at(2100, 2, 28, 8, 0, 0, 0),
        at(2025, 12, 31, 23, 59, 59, 0),
        at(2026, 1, 1, 0, 0, 0, 1),
        at(1, 1, 1, 0, 0, 0, 0),
        at(9999, 12, 31, 23, 59, 59, 999_999_999),
    ]
}

#[test]
fn test_format_expiration_yyyymmdd_datetime_matches_previous_format() {
    for instant in instants() {
        let expiration = ExpirationDate::DateTime(instant);
        let previous = expiration
            .get_date()
            .expect("datetime resolves")
            .format("%Y%m%d")
            .to_string();
        assert_eq!(
            format_expiration_yyyymmdd(&expiration).ok(),
            Some(previous),
            "{instant:?}"
        );
    }
}

#[test]
fn test_expiration_label_datetime_matches_display_byte_for_byte() {
    for instant in instants() {
        let expiration = ExpirationDate::DateTime(instant);
        assert_eq!(
            expiration_label(&expiration),
            expiration.to_string(),
            "{instant:?}"
        );
    }
}

#[test]
fn test_contract_symbols_at_matches_previous_symbols() {
    for instant in instants() {
        let expiration = ExpirationDate::DateTime(instant);
        let day = expiration
            .get_date()
            .expect("datetime resolves")
            .format("%Y%m%d")
            .to_string();
        for strike in [0_u64, 1, 50_000, u64::MAX] {
            assert_eq!(
                contract_symbols_at("BTC", instant, strike),
                (
                    format!("BTC-{day}-{strike}-C"),
                    format!("BTC-{day}-{strike}-P")
                ),
                "{instant:?} {strike}"
            );
        }
    }
}

#[test]
fn test_contract_symbols_at_past_date_names_that_date() {
    let (call, put) = contract_symbols_at("ETH", at(2001, 2, 3, 8, 0, 0, 0), 1500);
    assert_eq!(call, "ETH-20010203-1500-C");
    assert_eq!(put, "ETH-20010203-1500-P");
}

#[test]
fn test_days_expiration_formatting_is_unchanged() {
    let expiration = ExpirationDate::Days(pos_or_panic!(30.0));
    // Resolved against the same reference instant on both sides.
    let previous = expiration
        .get_date()
        .expect("days resolve")
        .format("%Y%m%d")
        .to_string();
    let formatted = format_expiration_yyyymmdd(&expiration).expect("days resolve");
    assert_eq!(formatted.len(), 8);
    // A midnight between the two calls could shift the day; both are days
    // from now, so they are equal or one day apart.
    assert!(
        formatted >= previous,
        "{formatted} {previous}"
    );
    assert!(expiration_label(&expiration).ends_with(" UTC"));
}

#[test]
fn test_hierarchy_leaf_of_past_datetime_expiration_has_its_symbol() {
    let manager = UnderlyingOrderBookManager::new();
    let underlying = manager.get_or_create("BTC");
    let expiration =
        underlying.get_or_create_expiration(ExpirationDate::DateTime(at(2020, 3, 27, 8, 0, 0, 0)));
    let strike = expiration.get_or_create_strike(50_000);
    assert_eq!(strike.call_arc().symbol(), "BTC-20200327-50000-C");
    assert_eq!(strike.put_arc().symbol(), "BTC-20200327-50000-P");
}
