//! Example: OptionChainOrderBook - Option Chain Level
//!
//! This example demonstrates the option chain level of the hierarchy:
//! managing all strikes for a single expiration date.
//!
//! Run with: `cargo run --example 03_chain_orderbook`

use option_chain_orderbook::orderbook::{OptionChainOrderBook, OptionChainOrderBookManager};
use optionstratlib::ExpirationDate;
use optionstratlib::prelude::{Positive, pos_or_panic};
use orderbook_rs::{OrderId, Side};
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    info!("=== OptionChainOrderBook Example ===\n");
    info!("This level manages all strikes for a single expiration.\n");

    let expiration = ExpirationDate::Days(Positive::THIRTY);

    // === Single Option Chain ===
    info!("--- Creating OptionChainOrderBook ---");
    let chain = OptionChainOrderBook::new("BTC", expiration);

    info!("Underlying: {}", chain.underlying());
    info!("Expiration: {:?}", chain.expiration());

    // === Creating Strikes ===
    info!("\n--- Creating Strikes with Orders ---");
    let strikes = [45000, 47500, 50000, 52500, 55000, 57500, 60000];

    for &strike_price in &strikes {
        let strike = chain.get_or_create_strike(strike_price);

        // Add call orders
        strike
            .call()
            .add_limit_order(
                OrderId::new(),
                Side::Buy,
                (100 + strike_price / 1000) as u128,
                10,
            )
            .unwrap();
        strike
            .call()
            .add_limit_order(
                OrderId::new(),
                Side::Sell,
                (120 + strike_price / 1000) as u128,
                8,
            )
            .unwrap();

        // Add put orders
        strike
            .put()
            .add_limit_order(
                OrderId::new(),
                Side::Buy,
                (50 + strike_price / 2000) as u128,
                12,
            )
            .unwrap();
        strike
            .put()
            .add_limit_order(
                OrderId::new(),
                Side::Sell,
                (70 + strike_price / 2000) as u128,
                6,
            )
            .unwrap();

        info!("Created strike {} with 4 orders", strike_price);
    }

    // === Chain Statistics ===
    info!("\n--- Chain Statistics ---");
    info!("Strike count: {}", chain.strike_count());
    info!("Is empty: {}", chain.is_empty());
    info!("Total order count: {}", chain.total_order_count());

    // === Strike Prices ===
    info!("\n--- Available Strikes ---");
    let strike_list = chain.strike_prices();
    info!("Strikes: {:?}", strike_list);

    // === ATM Strike ===
    info!("\n--- ATM Strike Lookup ---");
    for spot in [48000, 50000, 53000, 58000] {
        match chain.atm_strike(spot) {
            Ok(atm) => info!("Spot: {} -> ATM: {}", spot, atm),
            Err(e) => info!("Spot: {} -> Error: {}", spot, e),
        }
    }

    // === Get Specific Strike ===
    info!("\n--- Accessing Specific Strike ---");
    match chain.get_strike(50000) {
        Ok(strike) => {
            info!("Strike 50000:");
            let call_quote = strike.call_quote();
            let put_quote = strike.put_quote();
            info!(
                "  Call: {} @ {:?} / {} @ {:?}",
                call_quote.bid_size(),
                call_quote.bid_price(),
                call_quote.ask_size(),
                call_quote.ask_price()
            );
            info!(
                "  Put:  {} @ {:?} / {} @ {:?}",
                put_quote.bid_size(),
                put_quote.bid_price(),
                put_quote.ask_size(),
                put_quote.ask_price()
            );
        }
        Err(e) => info!("Error: {}", e),
    }

    // === Chain Stats ===
    info!("\n--- Chain Stats Summary ---");
    let stats = chain.stats();
    info!("Expiration: {:?}", stats.expiration);
    info!("Strike count: {}", stats.strike_count);
    info!("Total orders: {}", stats.total_orders);

    // =========================================
    // OptionChainOrderBookManager
    // =========================================
    info!("\n\n=== OptionChainOrderBookManager Example ===\n");
    info!("The manager handles multiple expirations for one underlying.\n");

    let manager = OptionChainOrderBookManager::new("ETH");
    info!("Created manager for: {}", manager.underlying());

    // === Creating Multiple Expirations ===
    info!("\n--- Creating Multiple Expirations ---");
    let expirations = [
        ExpirationDate::Days(Positive::SEVEN),     // Weekly
        ExpirationDate::Days(pos_or_panic!(14.0)), // Bi-weekly
        ExpirationDate::Days(Positive::THIRTY),    // Monthly
        ExpirationDate::Days(Positive::SIXTY),     // Bi-monthly
        ExpirationDate::Days(Positive::NINETY),    // Quarterly
    ];

    for exp in &expirations {
        let chain = manager.get_or_create(*exp);

        // Add some strikes
        for strike in [3000, 3200, 3400, 3600, 3800] {
            let s = chain.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 50, 10)
                .unwrap();
        }
        info!("Created chain for {:?} with 5 strikes", exp);
    }

    // === Manager Statistics ===
    info!("\n--- Manager Statistics ---");
    info!("Number of expirations: {}", manager.len());
    info!("Is empty: {}", manager.is_empty());
    info!("Total order count: {}", manager.total_order_count());

    // === Get Specific Expiration ===
    info!("\n--- Accessing Specific Expiration ---");
    let exp_30 = ExpirationDate::Days(Positive::THIRTY);
    match manager.get(&exp_30) {
        Ok(chain) => {
            info!("Found 30-day expiration:");
            info!("  Strike count: {}", chain.strike_count());
            info!("  Total orders: {}", chain.total_order_count());
        }
        Err(e) => info!("Error: {}", e),
    }

    // === Contains Check ===
    info!("\n--- Contains Check ---");
    let exp_30 = ExpirationDate::Days(Positive::THIRTY);
    let exp_365 = ExpirationDate::Days(pos_or_panic!(365.0));
    info!("Contains 30-day: {}", manager.contains(&exp_30));
    info!("Contains 365-day: {}", manager.contains(&exp_365));

    // === Remove Expiration ===
    info!("\n--- Removing Expiration ---");
    let exp_7 = ExpirationDate::Days(Positive::SEVEN);
    let removed = manager.remove(&exp_7);
    info!("Removed 7-day expiration: {}", removed);
    info!("Expirations after removal: {}", manager.len());

    info!("\n=== Example Complete ===");
}
