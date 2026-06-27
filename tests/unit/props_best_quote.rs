//! Property: `best_quote()` matches the book's top-of-book on each side, and a
//! quote is two-sided iff orders rest on both sides.
//!
//! Invariant asserted, after applying a generated op stream to one leaf
//! [`OptionOrderBook`]:
//!
//! * `quote.bid_price()` equals `book.best_bid()` and `quote.ask_price()` equals
//!   `book.best_ask()` (top-of-book on each side);
//! * `quote.is_two_sided()` is true iff both sides rest (and equals
//!   `book.has_both_sides()`);
//! * a missing side carries `None` price and `Quantity::ZERO`, a present side
//!   carries the exact best-level depth (cross-checked against the order-book
//!   snapshot's top level, an independent read path from `best_quote`).
//!
//! The strategy's tight price band forces crossings and its `Buy`-biased side
//! leaves one-sided books frequently. The three deterministic examples below
//! pin the two-sided, one-sided, and crossing-clears-a-side cases so they are
//! exercised regardless of the random seed.

use crate::common::strategies::{apply_op, op_stream};
use option_chain_orderbook::orderbook::OptionOrderBook;
use option_chain_orderbook::{OrderId, Quantity, Side};
use optionstratlib::OptionStyle;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn best_quote_reflects_top_of_book(stream in op_stream(1..200)) {
        let book = OptionOrderBook::new("BTC-20260130-50000-C", OptionStyle::Call);
        for op in &stream {
            apply_op(&book, op);
        }

        let quote = book.best_quote();
        let bid = book.best_bid();
        let ask = book.best_ask();

        // Top-of-book price per side, wrapped through the `Price` newtype.
        prop_assert_eq!(quote.bid_price().map(|p| p.as_u128()), bid);
        prop_assert_eq!(quote.ask_price().map(|p| p.as_u128()), ask);

        // Two-sided iff both sides rest — consistent with the cheap predicate.
        prop_assert_eq!(quote.is_two_sided(), bid.is_some() && ask.is_some());
        prop_assert_eq!(quote.is_two_sided(), book.has_both_sides());

        // A missing side has no price and zero size; a present side carries the
        // *exact* best-level depth. The oracle is the order-book snapshot's best
        // level — an independent read path from `best_quote`'s own
        // `total_depth_at_levels(1)`, so a wrong-but-nonzero size (e.g. summing
        // every level instead of only the top) is caught, not just a zero fill.
        // The test stream submits only plain limit orders, so hidden quantity is
        // always zero and `visible_quantity()` equals the level's total depth.
        let snap = book.snapshot(1);
        match bid {
            Some(_) => {
                let top_bid_depth = snap.bids.first().expect("bid level present").visible_quantity();
                prop_assert!(quote.bid_size().as_u64() > 0);
                prop_assert_eq!(quote.bid_size(), top_bid_depth);
            }
            None => {
                prop_assert!(quote.bid_price().is_none());
                prop_assert_eq!(quote.bid_size(), Quantity::ZERO);
            }
        }
        match ask {
            Some(_) => {
                let top_ask_depth = snap.asks.first().expect("ask level present").visible_quantity();
                prop_assert!(quote.ask_size().as_u64() > 0);
                prop_assert_eq!(quote.ask_size(), top_ask_depth);
            }
            None => {
                prop_assert!(quote.ask_price().is_none());
                prop_assert_eq!(quote.ask_size(), Quantity::ZERO);
            }
        }
    }
}

#[test]
fn two_sided_book_yields_two_sided_quote() {
    let book = OptionOrderBook::new("BTC-20260130-50000-C", OptionStyle::Call);
    // Non-crossing: bid below ask, both rest.
    book.add_limit_order(OrderId::from_u64(1), Side::Buy, 99, 10)
        .expect("add buy");
    book.add_limit_order(OrderId::from_u64(2), Side::Sell, 101, 5)
        .expect("add sell");

    let quote = book.best_quote();
    assert!(quote.is_two_sided());
    assert_eq!(quote.bid_price().map(|p| p.as_u128()), Some(99));
    assert_eq!(quote.ask_price().map(|p| p.as_u128()), Some(101));
    assert_eq!(quote.bid_price().map(|p| p.as_u128()), book.best_bid());
    assert_eq!(quote.ask_price().map(|p| p.as_u128()), book.best_ask());
}

#[test]
fn one_sided_book_yields_one_sided_quote() {
    let book = OptionOrderBook::new("BTC-20260130-50000-C", OptionStyle::Call);
    // Only a bid rests — the ask side is empty.
    book.add_limit_order(OrderId::from_u64(1), Side::Buy, 100, 7)
        .expect("add buy");

    let quote = book.best_quote();
    assert!(!quote.is_two_sided());
    assert!(!book.has_both_sides());
    assert_eq!(quote.bid_price().map(|p| p.as_u128()), Some(100));
    assert!(quote.ask_price().is_none());
    assert_eq!(quote.ask_size(), Quantity::ZERO);
}

#[test]
fn crossing_orders_clear_a_side() {
    let book = OptionOrderBook::new("BTC-20260130-50000-C", OptionStyle::Call);
    // Resting bid, then an aggressing sell that crosses it (99 <= 101) and
    // fully matches — clearing the ask side and leaving a bid-only book.
    book.add_limit_order(OrderId::from_u64(1), Side::Buy, 101, 10)
        .expect("add buy");
    book.add_limit_order(OrderId::from_u64(2), Side::Sell, 99, 4)
        .expect("add crossing sell");

    let quote = book.best_quote();
    // The aggressing sell matched against the bid and did not rest.
    assert!(book.best_ask().is_none());
    assert!(!quote.is_two_sided());
    // Residual bid quantity (10 - 4 = 6) rests at 101.
    assert_eq!(quote.bid_price().map(|p| p.as_u128()), Some(101));
    assert_eq!(quote.bid_size(), Quantity::new(6));
}
