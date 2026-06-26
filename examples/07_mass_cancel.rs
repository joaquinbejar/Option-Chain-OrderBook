//! Example: Mass Cancel Operations
//!
//! This example demonstrates the hierarchical mass cancel functionality
//! at every level of the order book hierarchy.
//!
//! Run with: `cargo run --example 07_mass_cancel`

use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
use optionstratlib::ExpirationDate;
use optionstratlib::prelude::pos_or_panic;
use orderbook_rs::{OrderId, Side};
use pricelevel::Hash32;
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Mass Cancel Example ===\n");
    info!("Demonstrating hierarchical mass cancel operations.\n");

    // === Create Global Manager ===
    let manager = UnderlyingOrderBookManager::new();

    // Setup: Create orders across the hierarchy
    setup_orders(&manager);

    info!("Initial state:");
    print_stats(&manager);

    // =========================================
    // Demo 1: Cancel All at Strike Level
    // =========================================
    info!("\n\n=== Demo 1: Cancel All at Strike Level ===\n");

    let exp_30 = ExpirationDate::Days(pos_or_panic!(30.0));
    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp) = btc.get_expiration(&exp_30)
        && let Ok(strike) = exp.get_strike(50000)
    {
        info!(
            "Strike 50000 orders before cancel: {}",
            strike.order_count()
        );

        // Cancel all orders on this strike (both call and put)
        if let Ok(result) = strike.cancel_all() {
            info!(
                "Cancelled {} orders across {} books",
                result.total_cancelled(),
                result.books_affected()
            );
        }

        info!("Strike 50000 orders after cancel: {}", strike.order_count());
    }

    // =========================================
    // Demo 2: Cancel by Side at Chain Level
    // =========================================
    info!("\n\n=== Demo 2: Cancel by Side at Chain Level ===\n");

    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp) = btc.get_expiration(&exp_30)
    {
        let chain = exp.chain();
        info!("Chain orders before cancel: {}", chain.total_order_count());

        // Cancel all sell orders across the chain
        if let Ok(result) = chain.cancel_by_side(Side::Sell) {
            info!(
                "Cancelled {} sell orders across {} contract books",
                result.total_cancelled(),
                result.books_affected()
            );
        }

        info!(
            "Chain orders after cancel (buys remaining): {}",
            chain.total_order_count()
        );
    }

    // =========================================
    // Demo 3: Cancel by User at Expiration Level
    // =========================================
    info!("\n\n=== Demo 3: Cancel by User at Expiration Level ===\n");

    // First, add some user-specific orders
    let user_a = Hash32::from([1u8; 32]);
    let user_b = Hash32::from([2u8; 32]);

    if let Ok(eth) = manager.get("ETH")
        && let Ok(exp) = eth.get_expiration(&exp_30)
    {
        let strike = exp.get_or_create_strike(3000);

        // Add orders for user A
        let _ = strike
            .call()
            .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a);
        let _ =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 110, 10, user_a);

        // Add orders for user B
        let _ = strike
            .put()
            .add_limit_order_with_user(OrderId::new(), Side::Buy, 50, 10, user_b);
        let _ = strike
            .put()
            .add_limit_order_with_user(OrderId::new(), Side::Sell, 60, 10, user_b);

        drop(strike);

        info!(
            "ETH expiration orders before cancel: {}",
            exp.total_order_count()
        );

        // Cancel all orders for user A
        if let Ok(result) = exp.cancel_by_user(user_a) {
            info!("Cancelled {} orders for user A", result.total_cancelled());
        }

        info!(
            "ETH expiration orders after cancel (user B remaining): {}",
            exp.total_order_count()
        );
    }

    // =========================================
    // Demo 4: Cancel All at Underlying Level
    // =========================================
    info!("\n\n=== Demo 4: Cancel All at Underlying Level ===\n");

    if let Ok(btc) = manager.get("BTC") {
        info!(
            "BTC total orders before cancel: {}",
            btc.total_order_count()
        );

        if let Ok(result) = btc.cancel_all() {
            info!(
                "Cancelled {} orders across {} contract books",
                result.total_cancelled(),
                result.books_affected()
            );
        }

        info!("BTC total orders after cancel: {}", btc.total_order_count());
    }

    // =========================================
    // Demo 5: Global Cancel at Manager Level
    // =========================================
    info!("\n\n=== Demo 5: Global Cancel at Manager Level ===\n");

    // Re-add some orders for the global cancel demo
    setup_orders(&manager);
    info!("Re-added orders for global cancel demo");
    print_stats(&manager);

    if let Ok(result) = manager.cancel_all_across_underlyings() {
        info!(
            "\nGlobal cancel: {} orders cancelled across {} contract books",
            result.total_cancelled(),
            result.books_affected()
        );
    }

    info!("\nFinal state:");
    print_stats(&manager);

    info!("\n=== Mass Cancel Demo Complete ===");
}

fn setup_orders(manager: &UnderlyingOrderBookManager) {
    let exp_30 = ExpirationDate::Days(pos_or_panic!(30.0));
    let exp_60 = ExpirationDate::Days(pos_or_panic!(60.0));

    // BTC orders
    {
        let btc = manager.get_or_create("BTC");

        for exp in [exp_30, exp_60] {
            let exp_book = btc.get_or_create_expiration(exp);

            for strike_price in [48000, 50000, 52000] {
                let strike = exp_book.get_or_create_strike(strike_price);

                // Add call orders
                let _ = strike
                    .call()
                    .add_limit_order(OrderId::new(), Side::Buy, 100, 10);
                let _ = strike
                    .call()
                    .add_limit_order(OrderId::new(), Side::Sell, 110, 10);

                // Add put orders
                let _ = strike
                    .put()
                    .add_limit_order(OrderId::new(), Side::Buy, 50, 10);
                let _ = strike
                    .put()
                    .add_limit_order(OrderId::new(), Side::Sell, 60, 10);
            }
        }
    }

    // ETH orders
    {
        let eth = manager.get_or_create("ETH");
        let exp_book = eth.get_or_create_expiration(exp_30);

        for strike_price in [2800, 3000, 3200] {
            let strike = exp_book.get_or_create_strike(strike_price);

            let _ = strike
                .call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10);
            let _ = strike
                .call()
                .add_limit_order(OrderId::new(), Side::Sell, 110, 10);
        }
    }
}

fn print_stats(manager: &UnderlyingOrderBookManager) {
    let stats = manager.stats();
    info!(
        "  Underlyings: {}, Expirations: {}, Strikes: {}, Orders: {}",
        stats.underlying_count, stats.total_expirations, stats.total_strikes, stats.total_orders
    );
}
