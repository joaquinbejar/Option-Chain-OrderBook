//! Utility functions for the Option-Chain-OrderBook library.

use crate::error::{Error, Result};
use chrono::{NaiveDate, TimeZone, Utc};
use optionstratlib::{ExpirationDate, OptionStyle};
use std::sync::Once;

/// Formats an `ExpirationDate` as a string in `YYYYMMDD` format.
///
/// # Arguments
///
/// * `expiration` - The expiration date to format
///
/// # Returns
///
/// A string in `YYYYMMDD` format (e.g., "20251222")
///
/// # Errors
///
/// Returns an error if the date cannot be retrieved from the `ExpirationDate`.
///
/// # Examples
///
/// ```rust
/// use option_chain_orderbook::utils::format_expiration_yyyymmdd;
/// use optionstratlib::prelude::pos_or_panic;
/// use optionstratlib::ExpirationDate;
///
/// let expiration = ExpirationDate::Days(pos_or_panic!(30.0));
/// let formatted = format_expiration_yyyymmdd(&expiration)
///     .expect("format should succeed");
/// assert_eq!(formatted.len(), 8); // YYYYMMDD format
/// ```
pub fn format_expiration_yyyymmdd(expiration: &ExpirationDate) -> Result<String> {
    let date = expiration.get_date()?;
    Ok(date.format("%Y%m%d").to_string())
}

/// Parses a `YYYYMMDD` string into a canonical [`ExpirationDate`].
///
/// Thin wrapper that delegates to [`SymbolParser::parse_yyyymmdd`], the single
/// source of truth for the symbol-expiry grammar and its canonical time-of-day.
/// See that method for the exact instant assigned to the date.
///
/// # Arguments
///
/// * `date_str` - The date string in `YYYYMMDD` format
/// * `symbol` - The original symbol (for error messages)
///
/// # Errors
///
/// Returns [`Error::InvalidSymbol`] if the date format is invalid.
pub fn parse_yyyymmdd(date_str: &str, symbol: &str) -> Result<ExpirationDate> {
    SymbolParser::parse_yyyymmdd(date_str, symbol)
}

/// Returns the current wall-clock time as nanoseconds since Unix epoch.
///
/// Falls back to `0` if the system clock is unavailable or before the epoch.
#[inline]
pub(crate) fn nanos_since_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// Accumulates a `usize` subtree-stat counter with checked addition.
///
/// Hierarchy stats (expiration / strike / order counts) are tallied through
/// this helper so the per-counter arithmetic uses `checked_add` rather than a
/// `saturating_*` / `wrapping_*` method (per the crate's counter-arithmetic
/// rule). On the structurally unreachable overflow of a `usize` count it logs
/// once via the [`cold`](cold_count_overflow) path and caps at `usize::MAX`,
/// keeping the tally monotonic without panicking or wrapping. The explicit
/// `match` (rather than `checked_add(..).unwrap_or(usize::MAX)`) deliberately
/// keeps the checked form instead of collapsing to manual saturating
/// arithmetic.
#[inline]
#[must_use]
pub(crate) fn checked_accumulate(acc: usize, add: usize) -> usize {
    match acc.checked_add(add) {
        Some(sum) => sum,
        None => {
            cold_count_overflow();
            usize::MAX
        }
    }
}

/// Logs the structurally unreachable subtree-stat counter overflow exactly once
/// per process. The cap keeps the tally at `usize::MAX`, so every subsequent
/// accumulation also overflows; the [`Once`] guard prevents that from spamming
/// the log.
#[cold]
#[inline(never)]
fn cold_count_overflow() {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!("subtree stats counter overflowed usize; capping at usize::MAX");
    });
}

/// Parsed components of an option symbol.
///
/// Represents a decomposed option symbol like `BTC-20260130-50000-C` with all
/// its components extracted and validated.
///
/// # Invariants
///
/// All fields are private and the type can only be constructed through
/// [`ParsedSymbol::try_new`] (or [`SymbolParser::parse`], which delegates to
/// it). Construction guarantees:
///
/// * `underlying` is non-empty,
/// * `strike` is strictly positive,
/// * `expiration` is the canonical parse of `expiration_str` (so the two can
///   never disagree).
///
/// Together these guarantee the [`ParsedSymbol::to_symbol`] round-trip: the
/// reconstructed string always re-parses to an equal `ParsedSymbol`.
///
/// # Breaking
///
/// The fields are private; construct via [`ParsedSymbol::try_new`] and read
/// them through the [`ParsedSymbol::underlying`], [`ParsedSymbol::expiration`],
/// [`ParsedSymbol::expiration_str`], [`ParsedSymbol::strike`], and
/// [`ParsedSymbol::option_style`] accessors.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSymbol {
    /// Underlying asset (e.g., "BTC").
    underlying: String,
    /// Expiration date (canonical parse of `expiration_str`).
    expiration: ExpirationDate,
    /// Original expiration string (YYYYMMDD format).
    expiration_str: String,
    /// Strike price.
    strike: u64,
    /// Option type (Call or Put).
    option_style: OptionStyle,
}

impl ParsedSymbol {
    /// Builds a validated `ParsedSymbol` from its components.
    ///
    /// The `expiration` field is derived from `expiration_str` via the canonical
    /// [`SymbolParser::parse_yyyymmdd`], so the two can never diverge and the
    /// [`ParsedSymbol::to_symbol`] round-trip is guaranteed by construction.
    ///
    /// # Arguments
    ///
    /// * `underlying` - Underlying asset symbol (must be non-empty)
    /// * `expiration_str` - Expiration date in `YYYYMMDD` format
    /// * `strike` - Strike price (must be strictly positive)
    /// * `option_style` - Call or Put
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSymbol`] if `underlying` is empty, `strike` is
    /// zero, or `expiration_str` is not a valid `YYYYMMDD` date.
    pub fn try_new(
        underlying: impl Into<String>,
        expiration_str: impl Into<String>,
        strike: u64,
        option_style: OptionStyle,
    ) -> Result<Self> {
        let underlying = underlying.into();
        let expiration_str = expiration_str.into();
        let option_char = match option_style {
            OptionStyle::Call => "C",
            OptionStyle::Put => "P",
        };
        // Reconstruct the symbol only for error context — the happy path (run
        // per routed order) must not allocate it.
        let symbol = || format!("{underlying}-{expiration_str}-{strike}-{option_char}");

        if underlying.is_empty() {
            return Err(Error::invalid_symbol(
                symbol(),
                "underlying cannot be empty",
            ));
        }
        if strike == 0 {
            return Err(Error::invalid_symbol(
                symbol(),
                "strike price must be positive, got 0",
            ));
        }

        // `parse_yyyymmdd` only reads its `symbol` argument on the (cold) error
        // path, so pass an empty placeholder and rebuild the full context with
        // the original reason if it fails.
        let expiration =
            SymbolParser::parse_yyyymmdd(&expiration_str, "").map_err(|e| match e {
                Error::InvalidSymbol { reason, .. } => Error::invalid_symbol(symbol(), reason),
                other => other,
            })?;

        Ok(Self {
            underlying,
            expiration,
            expiration_str,
            strike,
            option_style,
        })
    }

    /// Returns the underlying asset symbol (e.g., `"BTC"`).
    #[must_use]
    #[inline]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the canonical expiration date.
    // Note: `ExpirationDate` is itself `#[must_use]`, so no attribute here
    // (clippy::double_must_use).
    #[inline]
    pub const fn expiration(&self) -> &ExpirationDate {
        &self.expiration
    }

    /// Returns the original expiration string in `YYYYMMDD` format.
    #[must_use]
    #[inline]
    pub fn expiration_str(&self) -> &str {
        &self.expiration_str
    }

    /// Returns the strike price.
    #[must_use]
    #[inline]
    pub const fn strike(&self) -> u64 {
        self.strike
    }

    /// Returns the option style (Call or Put).
    #[must_use]
    #[inline]
    pub const fn option_style(&self) -> OptionStyle {
        self.option_style
    }

    /// Reconstructs the symbol string from parsed components.
    ///
    /// This enables round-trip verification: parse → to_symbol → compare. The
    /// type invariants guarantee the result always re-parses to an equal
    /// `ParsedSymbol`.
    #[must_use]
    pub fn to_symbol(&self) -> String {
        let option_char = match self.option_style {
            OptionStyle::Call => "C",
            OptionStyle::Put => "P",
        };
        format!(
            "{}-{}-{}-{}",
            self.underlying, self.expiration_str, self.strike, option_char
        )
    }
}

/// Parser for option symbol strings.
///
/// Parses symbols in the format `{UNDERLYING}-{YYYYMMDD}-{STRIKE}-{C|P}`.
///
/// This is the single source of truth for the `{underlying}-{expiry}-{strike}-
/// {type}` grammar across the crate: the sequencer's symbol routing and the
/// NATS subject builder both delegate here so the parsed expiry instant — and
/// therefore the derived `ExpirationKey` used to key the expiration `SkipMap`s
/// — is identical everywhere. The option type is accepted case-insensitively
/// (`C`/`c`, `P`/`p`) and normalized to the canonical uppercase form.
///
/// # Examples
///
/// ```rust
/// use option_chain_orderbook::utils::SymbolParser;
/// use optionstratlib::OptionStyle;
///
/// let parsed = SymbolParser::parse("BTC-20260130-50000-C")
///     .expect("valid symbol");
/// assert_eq!(parsed.underlying(), "BTC");
/// assert_eq!(parsed.strike(), 50000);
/// assert_eq!(parsed.option_style(), OptionStyle::Call);
/// assert_eq!(parsed.to_symbol(), "BTC-20260130-50000-C");
/// ```
pub struct SymbolParser;

impl SymbolParser {
    /// Canonical UTC time-of-day, as `(hour, minute, second)`, assigned to the
    /// `{YYYYMMDD}` segment of a symbol.
    ///
    /// A symbol's date segment names a calendar day, not an instant. The
    /// contract is considered alive through the whole UTC trading day, so the
    /// canonical expiry instant is the **end** of that day, `23:59:59 UTC`.
    ///
    /// This single choice is load-bearing: the expiration managers key their
    /// `SkipMap`s on an `ExpirationKey` derived from the absolute
    /// `DateTime<Utc>`, so two parsers disagreeing on the time-of-day would
    /// produce different keys and silently split one chain across two map slots
    /// (an order routed via one parser could not resolve a chain created via
    /// the other). Keep every parser routed through this constant.
    const CANONICAL_EXPIRY_HMS: (u32, u32, u32) = (23, 59, 59);

    /// Parses a `YYYYMMDD` string into a canonical [`ExpirationDate`].
    ///
    /// The returned value is `ExpirationDate::DateTime` set to the canonical
    /// expiry time-of-day (`23:59:59 UTC`, see `CANONICAL_EXPIRY_HMS`) on the
    /// parsed date. This is the single source of truth for the symbol-expiry
    /// time-of-day across the crate.
    ///
    /// # Arguments
    ///
    /// * `date_str` - The date string in `YYYYMMDD` format
    /// * `symbol` - The original symbol (for error messages)
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSymbol`] if `date_str` is not 8 digits, is
    /// non-numeric, or is not a valid calendar date.
    pub fn parse_yyyymmdd(date_str: &str, symbol: &str) -> Result<ExpirationDate> {
        if date_str.len() != 8 {
            return Err(Error::invalid_symbol(
                symbol,
                format!("expiration must be 8 digits (YYYYMMDD), got '{}'", date_str),
            ));
        }

        if !date_str.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::invalid_symbol(
                symbol,
                format!("expiration must be numeric, got '{}'", date_str),
            ));
        }

        let naive_date = NaiveDate::parse_from_str(date_str, "%Y%m%d")
            .map_err(|_| Error::invalid_symbol(symbol, format!("invalid date '{}'", date_str)))?;

        let (hour, minute, second) = Self::CANONICAL_EXPIRY_HMS;
        let naive_datetime = naive_date
            .and_hms_opt(hour, minute, second)
            .ok_or_else(|| {
                Error::invalid_symbol(symbol, "failed to construct canonical expiration time")
            })?;
        let datetime = Utc.from_utc_datetime(&naive_datetime);

        Ok(ExpirationDate::DateTime(datetime))
    }

    /// Parses a symbol string into its components.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The symbol string to parse (e.g., "BTC-20260130-50000-C")
    ///
    /// # Returns
    ///
    /// A `ParsedSymbol` containing the extracted components.
    ///
    /// # Errors
    ///
    /// Returns `Error::InvalidSymbol` if:
    /// - The symbol doesn't have exactly 4 parts separated by `-`
    /// - The underlying is empty
    /// - The expiration is not a valid YYYYMMDD date
    /// - The strike is not a valid positive integer
    /// - The option type is not `C`/`c` or `P`/`p`
    pub fn parse(symbol: &str) -> Result<ParsedSymbol> {
        let parts: Vec<&str> = symbol.split('-').collect();

        if parts.len() != 4 {
            return Err(Error::invalid_symbol(
                symbol,
                format!(
                    "expected format UNDERLYING-YYYYMMDD-STRIKE-C|P, got {} parts",
                    parts.len()
                ),
            ));
        }

        let underlying = parts[0];
        let expiration_str = parts[1];

        let strike: u64 = parts[2].parse().map_err(|_| {
            Error::invalid_symbol(
                symbol,
                format!(
                    "invalid strike price '{}', expected positive integer",
                    parts[2]
                ),
            )
        })?;

        let option_style = match parts[3] {
            "C" | "c" => OptionStyle::Call,
            "P" | "p" => OptionStyle::Put,
            other => {
                return Err(Error::invalid_symbol(
                    symbol,
                    format!("invalid option type '{}', expected C or P", other),
                ));
            }
        };

        // `try_new` enforces the type invariants (non-empty underlying, positive
        // strike, canonical expiration derived from `expiration_str`).
        ParsedSymbol::try_new(underlying, expiration_str, strike, option_style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Utc};
    use optionstratlib::prelude::pos_or_panic;

    #[test]
    fn test_format_expiration_yyyymmdd_days() {
        let expiration = ExpirationDate::Days(pos_or_panic!(30.0));
        let formatted = match format_expiration_yyyymmdd(&expiration) {
            Ok(f) => f,
            Err(err) => panic!("format failed: {}", err),
        };
        assert_eq!(formatted.len(), 8);
        // Should be numeric only
        assert!(formatted.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn test_format_expiration_yyyymmdd_datetime() {
        let specific_date = match Utc.with_ymd_and_hms(2025, 12, 22, 18, 30, 0) {
            chrono::LocalResult::Single(dt) => dt,
            _ => panic!("failed to create datetime"),
        };
        let expiration = ExpirationDate::DateTime(specific_date);
        let formatted = match format_expiration_yyyymmdd(&expiration) {
            Ok(f) => f,
            Err(err) => panic!("format failed: {}", err),
        };
        assert_eq!(formatted, "20251222");
    }

    // ── Symbol Parser Tests ────────────────────────────────────────────────

    #[test]
    fn test_parse_valid_call_symbol() {
        let parsed = SymbolParser::parse("BTC-20260130-50000-C").expect("should parse");
        assert_eq!(parsed.underlying(), "BTC");
        assert_eq!(parsed.expiration_str(), "20260130");
        assert_eq!(parsed.strike(), 50000);
        assert_eq!(parsed.option_style(), OptionStyle::Call);
    }

    #[test]
    fn test_parse_valid_put_symbol() {
        let parsed = SymbolParser::parse("ETH-20251222-3000-P").expect("should parse");
        assert_eq!(parsed.underlying(), "ETH");
        assert_eq!(parsed.expiration_str(), "20251222");
        assert_eq!(parsed.strike(), 3000);
        assert_eq!(parsed.option_style(), OptionStyle::Put);
    }

    #[test]
    fn test_parse_single_char_underlying() {
        let parsed = SymbolParser::parse("E-20260101-100-C").expect("should parse");
        assert_eq!(parsed.underlying(), "E");
        assert_eq!(parsed.strike(), 100);
    }

    #[test]
    fn test_parse_large_strike() {
        let parsed = SymbolParser::parse("BTC-20260130-1000000-P").expect("should parse");
        assert_eq!(parsed.strike(), 1_000_000);
    }

    #[test]
    fn test_parse_invalid_too_few_parts() {
        let result = SymbolParser::parse("BTC-20260130-50000");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("3 parts"));
    }

    #[test]
    fn test_parse_invalid_too_many_parts() {
        let result = SymbolParser::parse("BTC-20260130-50000-C-extra");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("5 parts"));
    }

    #[test]
    fn test_parse_invalid_date_format_short() {
        let result = SymbolParser::parse("BTC-2026013-50000-C");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("8 digits"));
    }

    #[test]
    fn test_parse_invalid_date_format_non_numeric() {
        let result = SymbolParser::parse("BTC-2026013X-50000-C");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("numeric"));
    }

    #[test]
    fn test_parse_invalid_date_value() {
        let result = SymbolParser::parse("BTC-20261340-50000-C");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid date"));
    }

    #[test]
    fn test_parse_invalid_strike_not_number() {
        let result = SymbolParser::parse("BTC-20260130-abc-C");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid strike"));
    }

    #[test]
    fn test_parse_invalid_option_type() {
        let result = SymbolParser::parse("BTC-20260130-50000-X");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("expected C or P"));
    }

    #[test]
    fn test_parse_empty_underlying() {
        let result = SymbolParser::parse("-20260130-50000-C");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("underlying cannot be empty"));
    }

    #[test]
    fn test_roundtrip_parse_to_symbol() {
        let original = "BTC-20260130-50000-C";
        let parsed = SymbolParser::parse(original).expect("should parse");
        let reconstructed = parsed.to_symbol();
        assert_eq!(original, reconstructed);
    }

    #[test]
    fn test_roundtrip_put_symbol() {
        let original = "ETH-20251231-2500-P";
        let parsed = SymbolParser::parse(original).expect("should parse");
        assert_eq!(original, parsed.to_symbol());
    }

    #[test]
    fn test_parsed_symbol_expiration_date_correctness() {
        let parsed = SymbolParser::parse("BTC-20260130-50000-C").expect("should parse");
        let date = parsed.expiration().get_date().expect("should get date");
        assert_eq!(date.year(), 2026);
        assert_eq!(date.month(), 1);
        assert_eq!(date.day(), 30);
    }

    #[test]
    fn test_parse_yyyymmdd_valid() {
        let result = parse_yyyymmdd("20260130", "test");
        assert!(result.is_ok());
        let exp = result.expect("should parse");
        let date = exp.get_date().expect("should get date");
        assert_eq!(date.year(), 2026);
        assert_eq!(date.month(), 1);
        assert_eq!(date.day(), 30);
    }

    #[test]
    fn test_parse_yyyymmdd_invalid_length() {
        let result = parse_yyyymmdd("2026013", "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_yyyymmdd_invalid_month() {
        let result = parse_yyyymmdd("20261330", "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_strike_zero() {
        let result = SymbolParser::parse("BTC-20260130-0-C");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("strike price must be positive"));
    }

    #[test]
    fn test_parse_canonical_expiry_time_of_day_end_of_day() {
        use chrono::Timelike;
        let parsed = SymbolParser::parse("BTC-20260130-50000-C").expect("should parse");
        let dt = match parsed.expiration() {
            ExpirationDate::DateTime(dt) => *dt,
            other => panic!("expected DateTime, got {:?}", other),
        };
        assert_eq!((dt.hour(), dt.minute(), dt.second()), (23, 59, 59));
    }

    #[test]
    fn test_parse_lowercase_option_type_normalizes_to_uppercase() {
        let call = SymbolParser::parse("BTC-20260130-50000-c").expect("should parse");
        assert_eq!(call.option_style(), OptionStyle::Call);
        assert_eq!(call.to_symbol(), "BTC-20260130-50000-C");

        let put = SymbolParser::parse("BTC-20260130-50000-p").expect("should parse");
        assert_eq!(put.option_style(), OptionStyle::Put);
        assert_eq!(put.to_symbol(), "BTC-20260130-50000-P");
    }

    #[test]
    fn test_parsed_symbol_try_new_round_trip_reparses_equal() {
        let built = ParsedSymbol::try_new("BTC", "20260130", 50000, OptionStyle::Call)
            .expect("valid components");
        let reparsed = SymbolParser::parse(&built.to_symbol()).expect("to_symbol must re-parse");
        assert_eq!(built, reparsed);
    }

    #[test]
    fn test_parsed_symbol_try_new_rejects_zero_strike() {
        let result = ParsedSymbol::try_new("BTC", "20260130", 0, OptionStyle::Call);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("strike price must be positive"));
    }

    #[test]
    fn test_parsed_symbol_try_new_rejects_empty_underlying() {
        let result = ParsedSymbol::try_new("", "20260130", 50000, OptionStyle::Call);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("underlying cannot be empty"));
    }

    #[test]
    fn test_parsed_symbol_try_new_rejects_invalid_expiration() {
        let result = ParsedSymbol::try_new("BTC", "2026013", 50000, OptionStyle::Call);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("8 digits"));
    }

    #[test]
    fn test_parsed_symbol_try_new_expiration_matches_expiration_str() {
        let built = ParsedSymbol::try_new("ETH", "20251222", 3000, OptionStyle::Put)
            .expect("valid components");
        // The derived expiration must be the canonical parse of the string, so
        // formatting it back yields the same string (no possible divergence).
        let formatted = format_expiration_yyyymmdd(built.expiration()).expect("format");
        assert_eq!(formatted, built.expiration_str());
    }
}
