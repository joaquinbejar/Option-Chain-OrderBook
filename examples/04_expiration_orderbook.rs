//! Example: ExpirationOrderBook - Expiration Level
//!
//! This example demonstrates the expiration level of the hierarchy:
//! managing all strikes for a single expiration with additional metadata.
//!
//! Run with: `cargo run --example 04_expiration_orderbook`

use option_chain_orderbook::orderbook::{ExpirationOrderBook, ExpirationOrderBookManager};
use optionstratlib::{ExpirationDate, pos};
use orderbook_rs::{OrderId, Side};
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== ExpirationOrderBook Example ===\n");
    info!("This level wraps OptionChainOrderBook with expiration-specific features.\n");

    let expiration = ExpirationDate::Days(pos!(30.0));

    // === Single Expiration Order Book ===
    info!("--- Creating ExpirationOrderBook ---");
    let exp_book = ExpirationOrderBook::new("BTC", expiration);

    info!("Underlying: {}", exp_book.underlying());
    info!("Expiration: {:?}", exp_book.expiration());

    // === Creating Strikes ===
    info!("\n--- Creating Strikes ---");
    let strikes = [48000, 49000, 50000, 51000, 52000];

    for &strike_price in &strikes {
        let strike = exp_book.get_or_create_strike(strike_price);

        // Simulate market maker quoting
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 450, 20)
            .unwrap();
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 480, 15)
            .unwrap();
        strike
            .put()
            .add_limit_order(OrderId::new(), Side::Buy, 200, 25)
            .unwrap();
        strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 230, 18)
            .unwrap();

        info!("Strike {}: 4 orders (2 call, 2 put)", strike_price);
    }

    // === Expiration Statistics ===
    info!("\n--- Expiration Statistics ---");
    info!("Strike count: {}", exp_book.strike_count());
    info!("Is empty: {}", exp_book.is_empty());
    info!("Total order count: {}", exp_book.total_order_count());

    // === Strike Prices ===
    info!("\n--- Available Strikes ---");
    info!("Strikes: {:?}", exp_book.strike_prices());

    // === ATM Strike ===
    info!("\n--- ATM Strike Lookup ---");
    let spot = 50500u64;
    match exp_book.atm_strike(spot) {
        Ok(atm) => {
            info!("Spot price: {}", spot);
            info!("ATM strike: {}", atm);

            // Get the ATM strike details
            if let Ok(strike) = exp_book.get_strike(atm) {
                let call_quote = strike.call_quote();
                let put_quote = strike.put_quote();
                info!("\nATM Call Quote:");
                info!(
                    "  Bid: {} @ {:?}",
                    call_quote.bid_size(),
                    call_quote.bid_price()
                );
                info!(
                    "  Ask: {} @ {:?}",
                    call_quote.ask_size(),
                    call_quote.ask_price()
                );
                info!("\nATM Put Quote:");
                info!(
                    "  Bid: {} @ {:?}",
                    put_quote.bid_size(),
                    put_quote.bid_price()
                );
                info!(
                    "  Ask: {} @ {:?}",
                    put_quote.ask_size(),
                    put_quote.ask_price()
                );
            }
        }
        Err(e) => info!("Error finding ATM: {}", e),
    }

    // === Get Specific Strike ===
    info!("\n--- Accessing Specific Strike ---");
    match exp_book.get_strike(50000) {
        Ok(strike) => {
            info!("Strike 50000 found:");
            info!("  Total orders: {}", strike.order_count());
            info!("  Is fully quoted: {}", strike.is_fully_quoted());
        }
        Err(e) => info!("Error: {}", e),
    }

    // =========================================
    // ExpirationOrderBookManager
    // =========================================
    info!("\n\n=== ExpirationOrderBookManager Example ===\n");
    info!("The manager handles all expirations for one underlying.\n");

    let manager = ExpirationOrderBookManager::new("SPX");
    info!("Created manager for: {}", manager.underlying());

    // === Creating Multiple Expirations ===
    info!("\n--- Creating Expirations (Term Structure) ---");

    // Create a realistic term structure
    let term_structure = [
        (7, "Weekly"),
        (14, "Bi-weekly"),
        (30, "Monthly"),
        (60, "2-Month"),
        (90, "Quarterly"),
        (180, "6-Month"),
        (365, "Annual"),
    ];

    for (days, name) in term_structure {
        let exp = ExpirationDate::Days(pos!(days as f64));
        let exp_book = manager.get_or_create(exp);

        // Add strikes around ATM (assume SPX at 4500)
        for strike in [4400, 4450, 4500, 4550, 4600] {
            let s = exp_book.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
            s.call()
                .add_limit_order(OrderId::new(), Side::Sell, 110, 8)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 80, 10)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Sell, 90, 8)
                .unwrap();
        }
        info!("{} ({} days): 5 strikes, 20 orders", name, days);
    }

    // === Manager Statistics ===
    info!("\n--- Manager Statistics ---");
    info!("Number of expirations: {}", manager.len());
    info!("Total strike count: {}", manager.total_strike_count());
    info!("Total order count: {}", manager.total_order_count());

    // === Manager Stats Summary ===
    info!("\n--- Stats Summary ---");
    let stats = manager.stats();
    info!("Underlying: {}", stats.underlying);
    info!("Expiration count: {}", stats.expiration_count);
    info!("Total strikes: {}", stats.total_strikes);
    info!("Total orders: {}", stats.total_orders);

    // === Get Specific Expiration ===
    info!("\n--- Accessing Monthly Expiration ---");
    let exp_30 = ExpirationDate::Days(pos!(30.0));
    match manager.get(&exp_30) {
        Ok(exp_book) => {
            info!("Monthly expiration found:");
            info!("  Strike count: {}", exp_book.strike_count());
            info!("  Total orders: {}", exp_book.total_order_count());
            info!("  Strikes: {:?}", exp_book.strike_prices());
        }
        Err(e) => info!("Error: {}", e),
    }

    // === Contains Check ===
    info!("\n--- Contains Check ---");
    let exp_30 = ExpirationDate::Days(pos!(30.0));
    let exp_45 = ExpirationDate::Days(pos!(45.0));
    info!("Contains 30-day: {}", manager.contains(&exp_30));
    info!("Contains 45-day: {}", manager.contains(&exp_45));

    // === Remove Expiration ===
    info!("\n--- Removing Weekly Expiration ---");
    let exp_7 = ExpirationDate::Days(pos!(7.0));
    let removed = manager.remove(&exp_7);
    info!("Removed 7-day expiration: {}", removed);
    info!("Expirations after removal: {}", manager.len());
    info!(
        "Total orders after removal: {}",
        manager.total_order_count()
    );

    info!("\n=== Example Complete ===");
}
