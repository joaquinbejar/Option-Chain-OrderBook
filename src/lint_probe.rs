//! Self test of the bans in `clippy.toml`: every function below calls one
//! banned item, directly, through an alias, by UFCS or as a function value.
//! `scripts/lint-selftest.sh` builds this module (cfg `ocob_lint_probe`,
//! never set in a normal build) and requires a `disallowed_methods` hit on
//! every line marked `// ban:` and on no other line.

// The probes are never called: they only have to compile and be linted.
#![allow(dead_code)]

use crate::orderbook::{Clock, MonotonicClock, STPMode};
use optionstratlib::ExpirationDate;
use orderbook_rs::{OrderBook, PriceLevelChangedListener, TradeListener};
use std::sync::Arc;
use uuid::Uuid;

type Book = OrderBook<()>;

fn clock() -> Arc<dyn Clock> {
    Arc::new(MonotonicClock)
}

pub(crate) fn chrono_now() {
    let _ = chrono::Utc::now(); // ban: chrono::Utc::now
    let _ = chrono::Local::now(); // ban: chrono::Local::now
}

pub(crate) fn expiration_reads(expiration: &ExpirationDate) {
    let _ = expiration.get_date(); // ban: ExpirationDate::get_date
    let _ = expiration.get_date_with_options(false); // ban: ExpirationDate::get_date_with_options
    let _ = expiration.get_days(); // ban: ExpirationDate::get_days
    let _ = ExpirationDate::get_days(expiration); // ban: ExpirationDate::get_days (UFCS)
}

pub(crate) fn book_constructors(trades: TradeListener, levels: PriceLevelChangedListener) {
    let _ = Book::new("P"); // ban: OrderBook::new (alias)
    let _ = <OrderBook<()>>::new("P"); // ban: OrderBook::new (UFCS)
    let _ = Book::with_clock("P", clock()); // ban: OrderBook::with_clock
    let _ = Book::with_stp_mode("P", STPMode::CancelTaker); // ban: OrderBook::with_stp_mode
    let _ = Book::with_trade_listener("P", Arc::clone(&trades)); // ban: OrderBook::with_trade_listener
    let _ = Book::with_trade_and_price_level_listener("P", trades, levels); // ban: OrderBook::with_trade_and_price_level_listener
    let build: fn(&str) -> Book = Book::new; // ban: OrderBook::new (function value)
    let _ = build("P");
}

pub(crate) fn random_ids() {
    let _ = Uuid::new_v4(); // ban: Uuid::new_v4
    let make: fn() -> Uuid = Uuid::new_v4; // ban: Uuid::new_v4 (function value)
    let _ = make();
}

/// The one constructor that is not banned.
pub(crate) fn allowed_constructor() {
    let _ = Book::with_clock_and_namespace("P", clock(), Uuid::nil());
}
