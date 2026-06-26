//! Quote types for order book.
//!
//! This module provides the [`Quote`] and [`QuoteUpdate`] types for representing
//! two-sided markets (bid and ask).

use pricelevel::{Price, Quantity, TimestampMs};
use serde::{Deserialize, Serialize};

/// Represents a two-sided quote (bid and ask).
///
/// A quote captures the best bid and ask prices and sizes at a point in time.
/// Prices are in smallest units (e.g., cents, satoshis) carried as
/// [`Price`] (a `u128` newtype; `Option<Price>`, `None` when that side is
/// empty); sizes are [`Quantity`] (a `u64` newtype); the timestamp is a
/// [`TimestampMs`] (a `u64` newtype). The pricelevel newtypes are all
/// `#[serde(transparent)]`, so the JSON wire format is bare numbers — an
/// `Option<Price>` serializes identically to an `Option<u128>` and a
/// [`Quantity`] identically to a `u64`.
///
/// JSON precision note: prices are 128-bit. A price above `2^53` cannot be
/// represented exactly by an IEEE-754 double, so a consumer that deserializes
/// `Quote` JSON MUST use a 128-bit-aware number parser for the price fields —
/// parsing the JSON number as an `f64` silently loses precision above `2^53`.
///
/// Note: Equality comparison excludes `timestamp_ms`, comparing only market
/// data (prices and sizes). The struct carries no synthetic identifier, so its
/// serialized form is fully determined by its market data and timestamp — there
/// is no per-construction clock/RNG cost and serialization is deterministic.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Quote {
    /// Best bid price (None if no bids).
    bid_price: Option<Price>,
    /// Size available at best bid.
    bid_size: Quantity,
    /// Best ask price (None if no asks).
    ask_price: Option<Price>,
    /// Size available at best ask.
    ask_size: Quantity,
    /// Timestamp in milliseconds.
    timestamp_ms: TimestampMs,
}

impl Quote {
    /// Creates a new quote with the given values.
    ///
    /// Taking the pricelevel newtypes (rather than bare integers) makes the
    /// constructor self-documenting and removes the arg-swap footgun of a
    /// positional all-integer signature on a financial type.
    ///
    /// # Arguments
    ///
    /// * `bid_price` - Best bid [`Price`] (None if no bids)
    /// * `bid_size` - [`Quantity`] at best bid
    /// * `ask_price` - Best ask [`Price`] (None if no asks)
    /// * `ask_size` - [`Quantity`] at best ask
    /// * `timestamp_ms` - [`TimestampMs`] (milliseconds)
    #[must_use]
    #[inline]
    pub const fn new(
        bid_price: Option<Price>,
        bid_size: Quantity,
        ask_price: Option<Price>,
        ask_size: Quantity,
        timestamp_ms: TimestampMs,
    ) -> Self {
        Self {
            bid_price,
            bid_size,
            ask_price,
            ask_size,
            timestamp_ms,
        }
    }

    /// Creates an empty quote with no prices.
    #[must_use]
    #[inline]
    pub const fn empty(timestamp_ms: TimestampMs) -> Self {
        Self {
            bid_price: None,
            bid_size: Quantity::ZERO,
            ask_price: None,
            ask_size: Quantity::ZERO,
            timestamp_ms,
        }
    }

    /// Returns the best bid [`Price`] (None if no bids).
    #[must_use]
    #[inline]
    pub const fn bid_price(&self) -> Option<Price> {
        self.bid_price
    }

    /// Returns the [`Quantity`] at best bid.
    #[must_use]
    #[inline]
    pub const fn bid_size(&self) -> Quantity {
        self.bid_size
    }

    /// Returns the best ask [`Price`] (None if no asks).
    #[must_use]
    #[inline]
    pub const fn ask_price(&self) -> Option<Price> {
        self.ask_price
    }

    /// Returns the [`Quantity`] at best ask.
    #[must_use]
    #[inline]
    pub const fn ask_size(&self) -> Quantity {
        self.ask_size
    }

    /// Returns the [`TimestampMs`] (milliseconds).
    #[must_use]
    #[inline]
    pub const fn timestamp_ms(&self) -> TimestampMs {
        self.timestamp_ms
    }

    /// Returns true if the quote has both bid and ask.
    #[must_use]
    #[inline]
    pub const fn is_two_sided(&self) -> bool {
        self.bid_price.is_some() && self.ask_price.is_some()
    }

    /// Returns true if the quote has no prices.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.bid_price.is_none() && self.ask_price.is_none()
    }

    /// Returns the spread (ask − bid) in raw price units if both sides exist
    /// and the book is not crossed.
    ///
    /// A locked book (`ask == bid`) returns `Some(0)`; a crossed book
    /// (`ask < bid`) or a one-sided book returns `None`.
    ///
    /// Returned as a raw `u128` (a price *difference*, not an absolute
    /// [`Price`]), matching the leaf engine's
    /// [`OptionOrderBook::spread`](crate::orderbook::OptionOrderBook::spread).
    #[must_use]
    #[inline]
    pub fn spread(&self) -> Option<u128> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) if ask >= bid => Some(ask.as_u128() - bid.as_u128()),
            _ => None,
        }
    }

    /// Returns the mid price if both sides exist.
    ///
    /// Computed as `(bid + ask) / 2` over the `f64`-widened price values. The
    /// result is guarded against a non-finite `f64` (NaN/inf) and yields `None`
    /// in that case rather than leaking a NaN across the boundary.
    #[must_use]
    #[inline]
    pub fn mid_price(&self) -> Option<f64> {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => {
                let mid = (bid.to_f64_lossy() + ask.to_f64_lossy()) / 2.0;
                mid.is_finite().then_some(mid)
            }
            _ => None,
        }
    }

    /// Returns the spread in basis points relative to mid price.
    ///
    /// Guards the `f64` boundary: requires a positive, finite mid price and a
    /// finite result, otherwise yields `None`.
    #[must_use]
    #[inline]
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.spread(), self.mid_price()) {
            (Some(spread), Some(mid)) if mid > 0.0 => {
                let bps = spread as f64 / mid * 10000.0;
                bps.is_finite().then_some(bps)
            }
            _ => None,
        }
    }
}

impl PartialEq for Quote {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        // Note: timestamp_ms is excluded from comparison
        // to detect only market data changes (prices and sizes)
        self.bid_price == other.bid_price
            && self.bid_size == other.bid_size
            && self.ask_price == other.ask_price
            && self.ask_size == other.ask_size
    }
}

impl Eq for Quote {}

impl std::fmt::Display for Quote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => {
                write!(f, "{}@{} / {}@{}", self.bid_size, bid, self.ask_size, ask)
            }
            (Some(bid), None) => write!(f, "{}@{} / -", self.bid_size, bid),
            (None, Some(ask)) => write!(f, "- / {}@{}", self.ask_size, ask),
            (None, None) => write!(f, "- / -"),
        }
    }
}

/// Represents a change in quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteUpdate {
    /// Previous quote.
    pub previous: Quote,
    /// Current quote.
    pub current: Quote,
}

impl QuoteUpdate {
    /// Creates a new quote update.
    #[must_use]
    pub const fn new(previous: Quote, current: Quote) -> Self {
        Self { previous, current }
    }

    /// Returns true if the bid price changed.
    #[must_use]
    pub fn bid_price_changed(&self) -> bool {
        self.previous.bid_price != self.current.bid_price
    }

    /// Returns true if the ask price changed.
    #[must_use]
    pub fn ask_price_changed(&self) -> bool {
        self.previous.ask_price != self.current.ask_price
    }

    /// Returns true if any price changed.
    #[must_use]
    pub fn price_changed(&self) -> bool {
        self.bid_price_changed() || self.ask_price_changed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_creation() {
        let quote = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(1234567890),
        );

        assert_eq!(quote.bid_price(), Some(Price::new(100)));
        assert_eq!(quote.bid_size(), Quantity::new(10));
        assert_eq!(quote.ask_price(), Some(Price::new(105)));
        assert_eq!(quote.ask_size(), Quantity::new(5));
        assert!(quote.is_two_sided());
    }

    #[test]
    fn test_quote_empty() {
        let quote = Quote::empty(TimestampMs::new(0));

        assert!(quote.is_empty());
        assert!(!quote.is_two_sided());
        assert!(quote.spread().is_none());
        assert!(quote.mid_price().is_none());
    }

    #[test]
    fn test_quote_spread() {
        let quote = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(0),
        );

        assert_eq!(quote.spread(), Some(5));
        let mid = match quote.mid_price() {
            Some(m) => m,
            None => panic!("expected mid price"),
        };
        assert!((mid - 102.5).abs() < 0.01);
    }

    #[test]
    fn test_quote_display() {
        let quote = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(0),
        );
        assert_eq!(format!("{}", quote), "10@100 / 5@105");

        let bid_only = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            None,
            Quantity::ZERO,
            TimestampMs::new(0),
        );
        assert_eq!(format!("{}", bid_only), "10@100 / -");

        let ask_only = Quote::new(
            None,
            Quantity::ZERO,
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(0),
        );
        assert_eq!(format!("{}", ask_only), "- / 5@105");

        let empty = Quote::empty(TimestampMs::new(0));
        assert_eq!(format!("{}", empty), "- / -");
    }

    #[test]
    fn test_quote_update() {
        let prev = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(0),
        );
        let curr = Quote::new(
            Some(Price::new(101)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(1),
        );
        let update = QuoteUpdate::new(prev, curr);

        assert!(update.bid_price_changed());
        assert!(!update.ask_price_changed());
        assert!(update.price_changed());
    }

    #[test]
    fn test_quote_timestamp() {
        let quote = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(1234567890),
        );
        assert_eq!(quote.timestamp_ms(), TimestampMs::new(1234567890));
    }

    #[test]
    fn test_quote_spread_bps() {
        let quote = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(0),
        );
        let spread_bps = quote.spread_bps();
        assert!(spread_bps.is_some());
        // Spread = 5, mid = 102.5, bps = 5/102.5 * 10000 ≈ 487.8
        let bps = match spread_bps {
            Some(b) => b,
            None => panic!("expected spread bps"),
        };
        assert!((bps - 487.8).abs() < 1.0);
    }

    #[test]
    fn test_quote_spread_bps_none() {
        let quote = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            None,
            Quantity::ZERO,
            TimestampMs::new(0),
        );
        assert!(quote.spread_bps().is_none());

        let quote2 = Quote::empty(TimestampMs::new(0));
        assert!(quote2.spread_bps().is_none());
    }

    #[test]
    fn test_quote_partial_eq_excludes_timestamp() {
        // Equality compares market data only; the timestamp must not affect it.
        let a = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(1),
        );
        let b = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(999_999),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn test_quote_serde_roundtrip_transparent_shape() {
        // The pricelevel newtypes are all `#[serde(transparent)]`, so the JSON
        // shape is bare numbers — identical to the pre-migration
        // `Option<u128>` / `u64` raw-integer wire format (no wrapper object).
        let quote = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            Some(Price::new(105)),
            Quantity::new(5),
            TimestampMs::new(1_234_567_890),
        );

        let json = match serde_json::to_string(&quote) {
            Ok(s) => s,
            Err(e) => panic!("serialize failed: {e}"),
        };
        // Assert the transparent shape via `serde_json::Value` (robust to field
        // order / whitespace): every field must be a bare JSON number with the
        // expected value — not a wrapper object/array — proving the newtypes
        // serialize identically to the legacy raw-integer wire form.
        let value: serde_json::Value = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => panic!("parse to Value failed: {e}"),
        };
        assert_eq!(value["bid_price"], serde_json::json!(100));
        assert_eq!(value["bid_size"], serde_json::json!(10));
        assert_eq!(value["ask_price"], serde_json::json!(105));
        assert_eq!(value["ask_size"], serde_json::json!(5));
        assert_eq!(value["timestamp_ms"], serde_json::json!(1_234_567_890));
        // Each price/size field is a bare number, not a `{ "0": .. }` wrapper.
        assert!(
            value["bid_price"].is_number() && value["ask_price"].is_number(),
            "transparent prices must be bare numbers, not wrapper objects"
        );
        assert!(
            value["bid_size"].is_number() && value["timestamp_ms"].is_number(),
            "transparent sizes/timestamp must be bare numbers"
        );

        let back: Quote = match serde_json::from_str(&json) {
            Ok(q) => q,
            Err(e) => panic!("deserialize failed: {e}"),
        };
        assert_eq!(back, quote);
        // Round-trip also preserves the timestamp (excluded from PartialEq).
        assert_eq!(back.timestamp_ms(), TimestampMs::new(1_234_567_890));

        // A raw-integer JSON literal (the legacy wire form) still deserializes
        // into the newtype `Quote`, proving the shape is unchanged.
        let legacy = r#"{"bid_price":100,"bid_size":10,"ask_price":105,"ask_size":5,"timestamp_ms":1234567890}"#;
        let from_legacy: Quote = match serde_json::from_str(legacy) {
            Ok(q) => q,
            Err(e) => panic!("legacy deserialize failed: {e}"),
        };
        assert_eq!(from_legacy, quote);
    }

    #[test]
    fn test_quote_serde_one_sided_null_price() {
        // `None` price serializes as JSON `null`, unchanged from `Option<u128>`.
        let bid_only = Quote::new(
            Some(Price::new(100)),
            Quantity::new(10),
            None,
            Quantity::ZERO,
            TimestampMs::new(7),
        );
        let json = match serde_json::to_string(&bid_only) {
            Ok(s) => s,
            Err(e) => panic!("serialize failed: {e}"),
        };
        assert_eq!(
            json,
            r#"{"bid_price":100,"bid_size":10,"ask_price":null,"ask_size":0,"timestamp_ms":7}"#
        );
    }
}
