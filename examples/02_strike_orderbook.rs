//! Example: StrikeOrderBook - Strike Level Order Book
//!
//! This example demonstrates the strike level of the hierarchy:
//! managing call and put order books for a single strike price.
//!
//! Run with: `cargo run --example 02_strike_orderbook`

use option_chain_orderbook::orderbook::{StrikeOrderBook, StrikeOrderBookManager};
use optionstratlib::prelude::Positive;
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::{OrderId, Side};
use tracing::info;
use uuid::Uuid;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    info!("=== StrikeOrderBook Example ===\n");
    info!("This level manages call/put pairs at a single strike price.\n");

    let expiration = ExpirationDate::Days(Positive::THIRTY);

    // === Single Strike Order Book ===
    info!("--- Creating StrikeOrderBook ---");
    let strike = StrikeOrderBook::new("BTC", expiration, 50000);

    info!("Underlying: {}", strike.underlying());
    info!("Strike: {}", strike.strike());
    info!("Expiration: {:?}", strike.expiration());

    // === Adding Orders to Call Side ===
    info!("\n--- Adding Orders to CALL ---");

    strike
        .call()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 500, 10)
        .unwrap();
    strike
        .call()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 520, 8)
        .unwrap();
    info!("Added call bid: 500 x 10");
    info!("Added call ask: 520 x 8");

    // === Adding Orders to Put Side ===
    info!("\n--- Adding Orders to PUT ---");

    strike
        .put()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 300, 15)
        .unwrap();
    strike
        .put()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 320, 12)
        .unwrap();
    info!("Added put bid: 300 x 15");
    info!("Added put ask: 320 x 12");

    // === Accessing by Option Style ===
    info!("\n--- Accessing by OptionStyle ---");
    let call_book = strike.get(OptionStyle::Call);
    let put_book = strike.get(OptionStyle::Put);
    info!("Call orders: {}", call_book.order_count());
    info!("Put orders: {}", put_book.order_count());

    // === Strike Level Statistics ===
    info!("\n--- Strike Statistics ---");
    info!("Total orders (call + put): {}", strike.order_count());
    info!("Is empty: {}", strike.is_empty());

    // === Call Quote ===
    info!("\n--- Call Quote ---");
    let call_quote = strike.call_quote().expect("call quote");
    info!(
        "Call: {} @ {:?} / {} @ {:?}",
        call_quote.bid_size(),
        call_quote.bid_price(),
        call_quote.ask_size(),
        call_quote.ask_price()
    );
    if let Some(spread) = call_quote.spread() {
        info!("Call spread: {}", spread);
    }

    // === Put Quote ===
    info!("\n--- Put Quote ---");
    let put_quote = strike.put_quote().expect("put quote");
    info!(
        "Put: {} @ {:?} / {} @ {:?}",
        put_quote.bid_size(),
        put_quote.bid_price(),
        put_quote.ask_size(),
        put_quote.ask_price()
    );
    if let Some(spread) = put_quote.spread() {
        info!("Put spread: {}", spread);
    }

    // === Fully Quoted Check ===
    info!("\n--- Market Quality ---");
    info!(
        "Is fully quoted (both call and put two-sided): {}",
        strike.is_fully_quoted()
    );

    // =========================================
    // StrikeOrderBookManager
    // =========================================
    info!("\n\n=== StrikeOrderBookManager Example ===\n");
    info!("The manager handles multiple strikes for one expiration.\n");

    let manager = StrikeOrderBookManager::new("BTC", expiration);
    info!("Created manager for: {}", manager.underlying());

    // === Creating Multiple Strikes ===
    info!("\n--- Creating Strikes ---");
    for strike_price in [45000, 47500, 50000, 52500, 55000] {
        let s = manager.get_or_create(strike_price);
        // Add some orders
        s.call()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
            .unwrap();
        s.call()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 110, 5)
            .unwrap();
        s.put()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
            .unwrap();
        s.put()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 60, 5)
            .unwrap();
        info!("Created strike {} with orders", strike_price);
    }

    // === Manager Statistics ===
    info!("\n--- Manager Statistics ---");
    info!("Number of strikes: {}", manager.len());
    info!("Is empty: {}", manager.is_empty());
    info!("Total order count: {}", manager.total_order_count());

    // === Strike Prices ===
    info!("\n--- Available Strikes (sorted) ---");
    let strikes = manager.strike_prices();
    info!("Strikes: {:?}", strikes);

    // === ATM Strike Lookup ===
    info!("\n--- ATM Strike Lookup ---");
    let spot = 51000u64;
    match manager.atm_strike(spot) {
        Ok(atm) => info!("Spot: {}, ATM strike: {}", spot, atm),
        Err(e) => info!("Error finding ATM: {}", e),
    }

    let spot = 46000u64;
    match manager.atm_strike(spot) {
        Ok(atm) => info!("Spot: {}, ATM strike: {}", spot, atm),
        Err(e) => info!("Error finding ATM: {}", e),
    }

    // === Get Existing Strike ===
    info!("\n--- Accessing Existing Strike ---");
    match manager.get(50000) {
        Ok(s) => {
            info!("Found strike 50000:");
            info!("  Call orders: {}", s.call().order_count());
            info!("  Put orders: {}", s.put().order_count());
        }
        Err(e) => info!("Error: {}", e),
    }

    // === Check Non-Existing Strike ===
    info!("\n--- Checking Non-Existing Strike ---");
    match manager.get(99999) {
        Ok(_) => info!("Found strike 99999"),
        Err(e) => info!("Strike 99999 not found: {}", e),
    }

    // === Contains Check ===
    info!("\n--- Contains Check ---");
    info!("Contains 50000: {}", manager.contains(50000));
    info!("Contains 99999: {}", manager.contains(99999));

    // === Remove Strike ===
    info!("\n--- Removing Strike ---");
    let removed = manager.remove(45000);
    info!("Removed strike 45000: {}", removed);
    info!("Strikes after removal: {:?}", manager.strike_prices());

    info!("\n=== Example Complete ===");
}
