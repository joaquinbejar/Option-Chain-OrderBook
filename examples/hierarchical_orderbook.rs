//! Hierarchical Order Book Example
//!
//! This example demonstrates the full hierarchical order book structure:
//!
//! ```text
//! UnderlyingOrderBookManager (manages all underlyings: BTC, ETH, SPX, etc.)
//!   └── UnderlyingOrderBook (per underlying, all expirations for one asset)
//!         └── ExpirationOrderBookManager (manages all expirations for underlying)
//!               └── ExpirationOrderBook (per expiry date)
//!                     └── OptionChainOrderBook (per expiration, option chain)
//!                           └── StrikeOrderBookManager (manages call/put pair)
//!                                 └── StrikeOrderBook (per strike price)
//!                                       └── OptionOrderBook (call or put)
//!                                             └── OrderBook<T> (from OrderBook-rs)
//! ```
//!
//! Run with: `cargo run --example hierarchical_orderbook`

use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
use optionstratlib::prelude::Positive;
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::{OrderId, Side};
use tracing::info;
use uuid::Uuid;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Hierarchical Order Book Example ===");

    // Create the top-level manager for all underlyings
    let manager = UnderlyingOrderBookManager::new();
    info!("Created UnderlyingOrderBookManager");

    // Define expirations
    let exp_mar = ExpirationDate::Days(Positive::THIRTY);
    let exp_jun = ExpirationDate::Days(Positive::NINETY);

    // ========================================
    // Create BTC option chain
    // ========================================
    info!("\n--- Creating BTC Option Chain ---");

    {
        let btc = manager.get_or_create("BTC");
        info!("Created UnderlyingOrderBook for BTC");

        // Add March 2024 expiration
        let exp = btc.get_or_create_expiration(exp_mar);
        info!("Created ExpirationOrderBook for March expiration");

        // Add strikes to March expiration
        for strike_price in [45000, 50000, 55000, 60000] {
            let strike_book = exp.get_or_create_strike(strike_price);
            info!("  Created StrikeOrderBook for strike {}", strike_price);

            // Add orders to call
            strike_book
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                .unwrap();
            strike_book
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 105, 8)
                .unwrap();

            // Add orders to put
            strike_book
                .put()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 15)
                .unwrap();
            strike_book
                .put()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 55, 12)
                .unwrap();
        }

        // Add June 2024 expiration
        let exp = btc.get_or_create_expiration(exp_jun);
        info!("Created ExpirationOrderBook for June expiration");

        for strike_price in [40000, 50000, 60000] {
            let strike_book = exp.get_or_create_strike(strike_price);
            strike_book
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 200, 5)
                .unwrap();
            strike_book
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 210, 5)
                .unwrap();
        }
    }

    // ========================================
    // Create ETH option chain
    // ========================================
    info!("\n--- Creating ETH Option Chain ---");

    {
        let eth = manager.get_or_create("ETH");
        let exp = eth.get_or_create_expiration(exp_mar);

        for strike_price in [3000, 3500, 4000] {
            let strike_book = exp.get_or_create_strike(strike_price);
            strike_book
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 80, 20)
                .unwrap();
            strike_book
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 85, 15)
                .unwrap();
        }
    }

    // ========================================
    // Query the hierarchy
    // ========================================
    info!("\n--- Global Statistics ---");
    let stats = manager.stats();
    info!("Underlyings: {}", stats.underlying_count);
    info!("Total expirations: {}", stats.total_expirations);
    info!("Total strikes: {}", stats.total_strikes);
    info!("Total orders: {}", stats.total_orders);

    // ========================================
    // Direct access to specific option
    // ========================================
    info!("\n--- Direct Access to BTC Strike ---");

    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp) = btc.get_expiration(&exp_mar)
        && let Ok(strike) = exp.get_strike(50000)
    {
        let call_quote = strike.call_quote().expect("call quote");
        info!(
            "Call quote: {} @ {:?} / {} @ {:?}",
            call_quote.bid_size(),
            call_quote.bid_price(),
            call_quote.ask_size(),
            call_quote.ask_price()
        );

        let put_quote = strike.put_quote().expect("put quote");
        info!(
            "Put quote: {} @ {:?} / {} @ {:?}",
            put_quote.bid_size(),
            put_quote.bid_price(),
            put_quote.ask_size(),
            put_quote.ask_price()
        );

        info!(
            "Is fully quoted (both call and put): {}",
            strike.is_fully_quoted()
        );
    }

    // ========================================
    // Access by option style
    // ========================================
    info!("\n--- Access by Option Style ---");
    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp) = btc.get_expiration(&exp_mar)
        && let Ok(strike) = exp.get_strike(55000)
    {
        let call = strike.get(OptionStyle::Call);
        let put = strike.get(OptionStyle::Put);

        info!("BTC-55000-C orders: {}", call.order_count());
        info!("BTC-55000-P orders: {}", put.order_count());
    }

    // ========================================
    // Find ATM strike
    // ========================================
    info!("\n--- ATM Strike Lookup ---");
    let spot_price = 52000u64;
    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp) = btc.get_expiration(&exp_mar)
        && let Ok(atm) = exp.atm_strike(spot_price)
    {
        info!("Spot: {}, ATM strike: {}", spot_price, atm);
    }

    info!("\n=== Example Complete ===");
}
