//! Basic Order Book Operations Example
//!
//! This example demonstrates the fundamental operations of the OptionOrderBook:
//! - Creating an order book for an option contract
//! - Adding limit orders (buy and sell)
//! - Querying best bid/ask quotes
//! - Canceling orders
//! - Getting order book snapshots
//!
//! Run with: `cargo run --example basic_orderbook`

use option_chain_orderbook::orderbook::OptionOrderBook;
use optionstratlib::OptionStyle;
use orderbook_rs::{OrderId, Side};
use tracing::info;
use uuid::Uuid;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Basic Order Book Operations ===");

    // Create an order book for a BTC call option
    // Symbol format: UNDERLYING-EXPIRY-STRIKE-TYPE
    let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
    info!("Created order book for: {}", book.symbol());

    // Add some buy orders (bids)
    // Prices are in smallest units (e.g., cents, satoshis)
    info!("--- Adding Buy Orders ---");
    let bid1 = OrderId::from_uuid(Uuid::new_v4());
    let bid2 = OrderId::from_uuid(Uuid::new_v4());
    let bid3 = OrderId::from_uuid(Uuid::new_v4());

    book.add_limit_order(bid1, Side::Buy, 500, 10).unwrap();
    info!("Added bid: price=500, size=10, id={:?}", bid1);

    book.add_limit_order(bid2, Side::Buy, 495, 20).unwrap();
    info!("Added bid: price=495, size=20, id={:?}", bid2);

    book.add_limit_order(bid3, Side::Buy, 490, 15).unwrap();
    info!("Added bid: price=490, size=15, id={:?}", bid3);

    // Add some sell orders (asks)
    info!("--- Adding Sell Orders ---");
    let ask1 = OrderId::from_uuid(Uuid::new_v4());
    let ask2 = OrderId::from_uuid(Uuid::new_v4());
    let ask3 = OrderId::from_uuid(Uuid::new_v4());

    book.add_limit_order(ask1, Side::Sell, 510, 8).unwrap();
    info!("Added ask: price=510, size=8, id={:?}", ask1);

    book.add_limit_order(ask2, Side::Sell, 515, 12).unwrap();
    info!("Added ask: price=515, size=12, id={:?}", ask2);

    book.add_limit_order(ask3, Side::Sell, 520, 25).unwrap();
    info!("Added ask: price=520, size=25, id={:?}", ask3);

    // Get the best quote (top of book)
    info!("--- Best Quote ---");
    let quote = book.best_quote().expect("best quote");
    info!("Best bid: {} @ {:?}", quote.bid_size(), quote.bid_price());
    info!("Best ask: {} @ {:?}", quote.ask_size(), quote.ask_price());
    info!("Spread: {:?}", quote.spread());
    info!("Is two-sided: {}", quote.is_two_sided());

    // Get a snapshot of the order book (top 5 levels)
    info!("--- Order Book Snapshot (5 levels) ---");
    let snapshot = book.snapshot(5).expect("snapshot");
    info!("Bids:");
    for (i, level) in snapshot.bids.iter().enumerate() {
        info!(
            "  Level {}: price={}, size={}",
            i + 1,
            level.price(),
            level.visible_quantity()
        );
    }
    info!("Asks:");
    for (i, level) in snapshot.asks.iter().enumerate() {
        info!(
            "  Level {}: price={}, size={}",
            i + 1,
            level.price(),
            level.visible_quantity()
        );
    }

    // Cancel an order
    info!("--- Canceling Order ---");
    info!("Canceling bid at price 495...");
    let cancelled = book.cancel_order(bid2);
    info!("Cancel result: {:?}", cancelled);

    // Check the quote after cancellation
    info!("--- Quote After Cancellation ---");
    let quote = book.best_quote().expect("best quote");
    info!("Best bid: {} @ {:?}", quote.bid_size(), quote.bid_price());
    info!("Best ask: {} @ {:?}", quote.ask_size(), quote.ask_price());

    // Demonstrate order book statistics
    info!("--- Order Book Statistics ---");
    let snapshot = book.snapshot(100).expect("snapshot");
    info!("Total bid levels: {}", snapshot.bids.len());
    info!("Total ask levels: {}", snapshot.asks.len());

    info!("=== Example Complete ===");
}
