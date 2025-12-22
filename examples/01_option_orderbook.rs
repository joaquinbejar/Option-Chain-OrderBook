//! Example: OptionOrderBook - Single Option Order Book
//!
//! This example demonstrates the lowest level of the hierarchy:
//! a single option order book for one specific option contract.
//!
//! Run with: `cargo run --example 01_option_orderbook`

use option_chain_orderbook::orderbook::OptionOrderBook;
use optionstratlib::OptionStyle;
use orderbook_rs::{OrderId, Side};
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== OptionOrderBook Example ===\n");
    info!("This is the lowest level: a single option contract order book.\n");

    // Create an order book for a BTC call option
    let book = OptionOrderBook::new("BTC-20251222-50000-C", OptionStyle::Call);
    info!("Created order book for: {}", book.symbol());
    info!("Option style: {:?}", book.option_style());
    info!("Symbol hash: {}\n", book.symbol_hash());

    // === Adding Orders ===
    info!("--- Adding Orders ---");

    // Add buy orders (bids)
    let bid1 = OrderId::new();
    let bid2 = OrderId::new();
    let bid3 = OrderId::new();

    book.add_limit_order(bid1, Side::Buy, 500, 10).unwrap();
    info!("Added BID: price=500, size=10");

    book.add_limit_order(bid2, Side::Buy, 495, 20).unwrap();
    info!("Added BID: price=495, size=20");

    book.add_limit_order(bid3, Side::Buy, 490, 15).unwrap();
    info!("Added BID: price=490, size=15");

    // Add sell orders (asks)
    let ask1 = OrderId::new();
    let ask2 = OrderId::new();
    let ask3 = OrderId::new();

    book.add_limit_order(ask1, Side::Sell, 510, 8).unwrap();
    info!("Added ASK: price=510, size=8");

    book.add_limit_order(ask2, Side::Sell, 515, 12).unwrap();
    info!("Added ASK: price=515, size=12");

    book.add_limit_order(ask3, Side::Sell, 520, 25).unwrap();
    info!("Added ASK: price=520, size=25");

    info!("\nTotal orders: {}", book.order_count());

    // === Quote Information ===
    info!("\n--- Best Quote (Top of Book) ---");
    let quote = book.best_quote();
    info!("Best Bid: {} @ {:?}", quote.bid_size(), quote.bid_price());
    info!("Best Ask: {} @ {:?}", quote.ask_size(), quote.ask_price());
    info!("Spread: {:?}", quote.spread());
    info!("Is two-sided: {}", quote.is_two_sided());

    // === Market Metrics ===
    info!("\n--- Market Metrics ---");
    if let Some(mid) = book.mid_price() {
        info!("Mid price: {:.2}", mid);
    }
    if let Some(spread) = book.spread() {
        info!("Spread: {}", spread);
    }
    if let Some(spread_bps) = book.spread_bps() {
        info!("Spread (bps): {:.2}", spread_bps);
    }
    if let Some(micro) = book.micro_price() {
        info!("Micro price: {:.2}", micro);
    }

    // === Order Book Depth ===
    info!("\n--- Order Book Depth ---");
    let bid_depth = book.total_bid_depth();
    let ask_depth = book.total_ask_depth();
    info!("Total bid depth: {}", bid_depth);
    info!("Total ask depth: {}", ask_depth);

    // === Imbalance ===
    info!("\n--- Order Imbalance ---");
    let imbalance = book.imbalance(5);
    info!(
        "Imbalance (5 levels): {:.2}% (positive = more bids)",
        imbalance * 100.0
    );

    // === Snapshot ===
    info!("\n--- Order Book Snapshot (3 levels) ---");
    let snapshot = book.snapshot(3);
    info!("Bids:");
    for (i, level) in snapshot.bids.iter().enumerate() {
        info!(
            "  Level {}: price={}, size={}",
            i + 1,
            level.price,
            level.visible_quantity
        );
    }
    info!("Asks:");
    for (i, level) in snapshot.asks.iter().enumerate() {
        info!(
            "  Level {}: price={}, size={}",
            i + 1,
            level.price,
            level.visible_quantity
        );
    }

    // === VWAP ===
    info!("\n--- VWAP Calculation ---");
    if let Some(vwap_buy) = book.vwap(20, Side::Buy) {
        info!("VWAP to buy 20 contracts: {:.2}", vwap_buy);
    }
    if let Some(vwap_sell) = book.vwap(20, Side::Sell) {
        info!("VWAP to sell 20 contracts: {:.2}", vwap_sell);
    }

    // === Cancel Order ===
    info!("\n--- Canceling Order ---");
    info!("Canceling bid at price 495...");
    let cancelled = book.cancel_order(bid2);
    info!("Cancel result: {:?}", cancelled);
    info!("Orders after cancel: {}", book.order_count());

    // Verify quote changed
    let quote = book.best_quote();
    info!(
        "Best Bid after cancel: {} @ {:?}",
        quote.bid_size(),
        quote.bid_price()
    );

    info!("\n=== Example Complete ===");
}
