//! Example: UnderlyingOrderBook - Underlying Level
//!
//! This example demonstrates the underlying level of the hierarchy:
//! managing all expirations for a single underlying asset.
//!
//! Run with: `cargo run --example 05_underlying_orderbook`

use option_chain_orderbook::orderbook::{UnderlyingOrderBook, UnderlyingOrderBookManager};
use optionstratlib::{ExpirationDate, pos};
use orderbook_rs::{OrderId, Side};
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== UnderlyingOrderBook Example ===\n");
    info!("This level manages all expirations for a single underlying asset.\n");

    // === Single Underlying Order Book ===
    info!("--- Creating UnderlyingOrderBook for BTC ---");
    let btc = UnderlyingOrderBook::new("BTC");
    info!("Underlying: {}", btc.underlying());

    // === Creating Term Structure ===
    info!("\n--- Building Term Structure ---");

    // Weekly expirations
    for week in 1..=4 {
        let exp = ExpirationDate::Days(pos!((week * 7) as f64));
        let exp_book = btc.get_or_create_expiration(exp);

        // Add strikes around 50000
        for strike in [48000, 49000, 50000, 51000, 52000] {
            let s = exp_book.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 500, 10)
                .unwrap();
            s.call()
                .add_limit_order(OrderId::new(), Side::Sell, 520, 8)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 300, 10)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Sell, 320, 8)
                .unwrap();
        }
        info!("Week {}: 5 strikes, 20 orders", week);
    }

    // Monthly expirations
    for month in [2, 3, 6] {
        let exp = ExpirationDate::Days(pos!((month * 30) as f64));
        let exp_book = btc.get_or_create_expiration(exp);

        // Add more strikes for longer-dated options
        for strike in (40000..=60000).step_by(2500) {
            let s = exp_book.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 600, 15)
                .unwrap();
            s.call()
                .add_limit_order(OrderId::new(), Side::Sell, 650, 12)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 400, 15)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Sell, 450, 12)
                .unwrap();
        }
        info!("Month {}: 9 strikes, 36 orders", month);
    }

    // === Underlying Statistics ===
    info!("\n--- Underlying Statistics ---");
    info!("Expiration count: {}", btc.expiration_count());
    info!("Total strike count: {}", btc.total_strike_count());
    info!("Total order count: {}", btc.total_order_count());
    info!("Is empty: {}", btc.is_empty());

    // === Underlying Stats Summary ===
    info!("\n--- Stats Summary ---");
    let stats = btc.stats();
    info!("Underlying: {}", stats.underlying);
    info!("Expiration count: {}", stats.expiration_count);
    info!("Total strikes: {}", stats.total_strikes);
    info!("Total orders: {}", stats.total_orders);

    // === Get Specific Expiration ===
    info!("\n--- Accessing 30-day Expiration ---");
    let exp_30 = ExpirationDate::Days(pos!(30.0));
    match btc.get_expiration(&exp_30) {
        Ok(exp_book) => {
            info!("Found 30-day expiration:");
            info!("  Strike count: {}", exp_book.strike_count());
            info!("  Total orders: {}", exp_book.total_order_count());

            // Find ATM
            let spot = 50500u64;
            if let Ok(atm) = exp_book.atm_strike(spot) {
                info!("  ATM strike (spot={}): {}", spot, atm);
            }
        }
        Err(e) => info!("Error: {}", e),
    }

    // === Contains Check ===
    info!("\n--- Contains Check ---");
    let exp_7 = ExpirationDate::Days(pos!(7.0));
    let exp_45 = ExpirationDate::Days(pos!(45.0));
    info!("Contains 7-day: {}", btc.get_expiration(&exp_7).is_ok());
    info!("Contains 45-day: {}", btc.get_expiration(&exp_45).is_ok());

    // === Remove Expiration ===
    info!("\n--- Removing 7-day Expiration ---");
    let exp_7 = ExpirationDate::Days(pos!(7.0));
    let removed = btc.expirations().remove(&exp_7);
    info!("Removed: {}", removed);
    info!("Expiration count after removal: {}", btc.expiration_count());

    // =========================================
    // UnderlyingOrderBookManager
    // =========================================
    info!("\n\n=== UnderlyingOrderBookManager Example ===\n");
    info!("The top-level manager handles all underlyings in the system.\n");

    let manager = UnderlyingOrderBookManager::new();
    info!("Created global order book manager");

    // === Creating Multiple Underlyings ===
    info!("\n--- Creating Multiple Underlyings ---");

    // Crypto underlyings
    for symbol in ["BTC", "ETH", "SOL"] {
        let underlying = manager.get_or_create(symbol);
        let exp = ExpirationDate::Days(pos!(30.0));
        let exp_book = underlying.get_or_create_expiration(exp);

        // Add some strikes
        for i in 0..5 {
            let strike = 50000 + i * 1000;
            let s = exp_book.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 50, 10)
                .unwrap();
        }
        info!("Created {}: 1 expiration, 5 strikes, 10 orders", symbol);
    }

    // Equity underlyings
    for symbol in ["SPX", "AAPL", "TSLA", "NVDA"] {
        let underlying = manager.get_or_create(symbol);

        // Multiple expirations
        for days in [7, 30, 90] {
            let exp = ExpirationDate::Days(pos!(days as f64));
            let exp_book = underlying.get_or_create_expiration(exp);

            for i in 0..3 {
                let strike = 100 + i * 10;
                let s = exp_book.get_or_create_strike(strike);
                s.call()
                    .add_limit_order(OrderId::new(), Side::Buy, 5, 10)
                    .unwrap();
                s.put()
                    .add_limit_order(OrderId::new(), Side::Buy, 3, 10)
                    .unwrap();
            }
        }
        info!("Created {}: 3 expirations, 9 strikes, 18 orders", symbol);
    }

    // === Global Statistics ===
    info!("\n--- Global Statistics ---");
    info!("Number of underlyings: {}", manager.len());
    info!("Total order count: {}", manager.total_order_count());

    // === Global Stats Summary ===
    info!("\n--- Global Stats Summary ---");
    let stats = manager.stats();
    info!("Underlying count: {}", stats.underlying_count);
    info!("Total expirations: {}", stats.total_expirations);
    info!("Total strikes: {}", stats.total_strikes);
    info!("Total orders: {}", stats.total_orders);

    // === Underlying Symbols ===
    info!("\n--- Available Underlyings ---");
    let symbols = manager.underlying_symbols();
    info!("Symbols: {:?}", symbols);

    // === Get Specific Underlying ===
    info!("\n--- Accessing BTC ---");
    match manager.get("BTC") {
        Ok(btc) => {
            let stats = btc.stats();
            info!("BTC Statistics:");
            info!("  Expirations: {}", stats.expiration_count);
            info!("  Total strikes: {}", stats.total_strikes);
            info!("  Total orders: {}", stats.total_orders);
        }
        Err(e) => info!("Error: {}", e),
    }

    // === Contains Check ===
    info!("\n--- Contains Check ---");
    info!("Contains BTC: {}", manager.contains("BTC"));
    info!("Contains XRP: {}", manager.contains("XRP"));

    // === Remove Underlying ===
    info!("\n--- Removing SOL ---");
    let removed = manager.remove("SOL");
    info!("Removed: {}", removed);
    info!(
        "Underlyings after removal: {:?}",
        manager.underlying_symbols()
    );
    info!(
        "Total orders after removal: {}",
        manager.total_order_count()
    );

    info!("\n=== Example Complete ===");
}
