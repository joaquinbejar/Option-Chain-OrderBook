//! Shared `proptest` strategies and fixtures for the invariant suites.
//!
//! Everything here goes through the crate's **public API** and the validated
//! `pricelevel` / `orderbook_rs` newtypes (`OrderId`, `Side`, `Price`,
//! `Quantity`) and `optionstratlib` types (`ExpirationDate`, `OptionStyle`).
//!
//! ## Why the input space is deliberately tiny
//!
//! * **Small underlying / expiration / strike pools** — so `get_or_create`
//!   collisions and cross-strike aggregation actually happen on short streams. A
//!   wide pool produces vacuous tests (every key unique, nothing to aggregate).
//! * **Tight price band (`99..=101`)** — so submitted orders frequently *cross*
//!   and match, exercising the quote / matching path instead of a flat book that
//!   never trades.
//! * **Side bias toward `Buy` (2:1)** — so the book is frequently left
//!   *one-sided* (bid-only) after crossings clear the asks, exercising the
//!   one-sided quote path as well as the two-sided one.
//! * **Small `OrderId` pool (`0..16`)** — so a generated `CancelById` actually
//!   hits a resting order often enough to matter.
//!
//! ## Expirations are always absolute (`DateTime`)
//!
//! Every expiration is an [`ExpirationDate::DateTime`] drawn from a fixed pool.
//! The `Days` variant is wall-clock-relative (it re-resolves against "now"), so
//! it is never used in inputs that feed symbol parsing, stats aggregation, or
//! `get_or_create` keys — those must be replay-stable.

use chrono::{TimeZone, Utc};
use option_chain_orderbook::orderbook::OptionOrderBook;
use option_chain_orderbook::{OrderId, Side};
use optionstratlib::{ExpirationDate, OptionStyle};
use proptest::collection::vec;
use proptest::prelude::*;

/// Fixed, tiny underlying pool. Kept short so `get_or_create` collisions are
/// common on short generated streams.
pub const UNDERLYINGS: &[&str] = &["BTC", "ETH", "SOL"];

/// Fixed, tiny strike pool. Distinct strikes are distinct leaf books, so cancel
/// scoping at one strike can never touch another — handy for out-of-scope
/// witnesses.
pub const STRIKES: &[u64] = &[100, 200, 300];

/// Fixed absolute-date expiration pool, as `(year, month, day)`. All days are
/// `<= 28` so every `(y, m, d)` is a valid calendar date for any month.
const EXP_DATES: &[(i32, u32, u32)] = &[(2026, 1, 30), (2026, 6, 26), (2026, 12, 18)];

/// Number of distinct expirations in the fixed pool.
pub const EXP_POOL_LEN: usize = EXP_DATES.len();

/// Returns the `i`th fixed-pool expiration as an absolute
/// [`ExpirationDate::DateTime`] (`23:59:59 UTC`, matching the crate's canonical
/// symbol-expiry time-of-day). Index wraps modulo the pool length.
// `ExpirationDate` is itself `#[must_use]`, so no attribute here (double_must_use).
pub fn nth_expiration(i: usize) -> ExpirationDate {
    let (y, m, d) = EXP_DATES[i % EXP_DATES.len()];
    let dt = match Utc.with_ymd_and_hms(y, m, d, 23, 59, 59) {
        chrono::LocalResult::Single(dt) => dt,
        _ => panic!("fixed expiration pool date must be unambiguous"),
    };
    ExpirationDate::DateTime(dt)
}

/// Strategy yielding a [`Side`], biased 2:1 toward `Buy` so crossings tend to
/// clear the asks and leave a one-sided (bid-only) book.
fn side() -> impl Strategy<Value = Side> {
    prop_oneof![2 => Just(Side::Buy), 1 => Just(Side::Sell)]
}

/// Strategy yielding an [`OptionStyle`].
pub fn any_style() -> impl Strategy<Value = OptionStyle> {
    prop_oneof![Just(OptionStyle::Call), Just(OptionStyle::Put)]
}

/// Strategy yielding an [`OrderId`] from a tiny pool so `CancelById` hits real
/// resting orders often.
fn order_id() -> impl Strategy<Value = OrderId> {
    (0u64..16).prop_map(OrderId::from_u64)
}

/// A single generated operation against one leaf [`OptionOrderBook`].
///
/// Local to the test crate so it is never re-exported by accident.
#[derive(Clone, Debug)]
pub enum Op {
    /// Submit a GTC limit order through the validated newtypes.
    Submit {
        /// Order identity (small pool).
        id: OrderId,
        /// Buy or Sell.
        side: Side,
        /// Limit price in the tight crossing band.
        price: u128,
        /// Order quantity (always positive).
        qty: u64,
    },
    /// Cancel one order by id (often a no-op — exercises the miss path too).
    CancelById(OrderId),
    /// Cancel every resting order on the book.
    CancelAll,
}

fn submit() -> impl Strategy<Value = Op> {
    // Tight price band forces crossings; qty is always >= 1 so the engine never
    // rejects on a zero quantity.
    (order_id(), side(), 99u128..=101, 1u64..=100).prop_map(|(id, side, price, qty)| Op::Submit {
        id,
        side,
        price,
        qty,
    })
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        7 => submit(),
        2 => order_id().prop_map(Op::CancelById),
        1 => Just(Op::CancelAll),
    ]
}

/// A stream of leaf-book operations of length within `len`.
pub fn op_stream(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<Op>> {
    vec(op(), len)
}

/// Applies one [`Op`] to a leaf [`OptionOrderBook`], ignoring the (expected)
/// errors from duplicate ids, cancel misses, and self-trade matches — the test
/// asserts on the *resulting state*, not on each op's success.
pub fn apply_op(book: &OptionOrderBook, op: &Op) {
    match op {
        Op::Submit {
            id,
            side,
            price,
            qty,
        } => {
            let _ = book.add_limit_order(*id, *side, *price, *qty);
        }
        Op::CancelById(id) => {
            let _ = book.cancel_order(*id);
        }
        Op::CancelAll => {
            let _ = book.cancel_all();
        }
    }
}

/// A generated operation routed to a `(strike, leg)` within one chain.
#[derive(Clone, Debug)]
pub struct ChainOp {
    /// Index into [`STRIKES`].
    pub strike_idx: usize,
    /// Call or Put leg.
    pub style: OptionStyle,
    /// The leaf operation.
    pub op: Op,
}

fn chain_op() -> impl Strategy<Value = ChainOp> {
    (0usize..STRIKES.len(), any_style(), op()).prop_map(|(strike_idx, style, op)| ChainOp {
        strike_idx,
        style,
        op,
    })
}

/// A stream of chain-routed operations across the fixed strike pool.
pub fn chain_op_stream(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<ChainOp>> {
    vec(chain_op(), len)
}

/// Resolves the strike price a [`ChainOp`] targets.
#[must_use]
pub fn chain_op_strike(routed: &ChainOp) -> u64 {
    STRIKES[routed.strike_idx % STRIKES.len()]
}

/// A generated operation routed to a full `(underlying, expiration, strike, leg)`
/// coordinate in the hierarchy.
#[derive(Clone, Debug)]
pub struct HierOp {
    /// Index into [`UNDERLYINGS`].
    pub underlying_idx: usize,
    /// Index into the fixed expiration pool.
    pub expiration_idx: usize,
    /// Index into [`STRIKES`].
    pub strike_idx: usize,
    /// Call or Put leg.
    pub style: OptionStyle,
    /// The leaf operation.
    pub op: Op,
}

fn hier_op() -> impl Strategy<Value = HierOp> {
    (
        0usize..UNDERLYINGS.len(),
        0usize..EXP_POOL_LEN,
        0usize..STRIKES.len(),
        any_style(),
        op(),
    )
        .prop_map(
            |(underlying_idx, expiration_idx, strike_idx, style, op)| HierOp {
                underlying_idx,
                expiration_idx,
                strike_idx,
                style,
                op,
            },
        )
}

/// A stream of hierarchy-routed operations across the fixed pools.
pub fn hier_op_stream(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<HierOp>> {
    vec(hier_op(), len)
}
