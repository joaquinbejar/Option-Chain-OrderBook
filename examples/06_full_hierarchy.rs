//! Example: Full Hierarchy - Complete Order Book System
//!
//! This example demonstrates the complete hierarchy working together,
//! simulating a realistic options trading environment.
//!
//! Run with: `cargo run --example 06_full_hierarchy`

use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
use optionstratlib::{ExpirationDate, OptionStyle, pos};
use orderbook_rs::{OrderId, Side};
use tracing::info;

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Full Hierarchy Example ===\n");
    info!("Demonstrating the complete order book hierarchy.\n");

    // === Create Global Manager ===
    let manager = UnderlyingOrderBookManager::new();

    // =========================================
    // Setup: Create a realistic market structure
    // =========================================
    info!("--- Setting Up Market Structure ---\n");

    setup_btc_options(&manager);
    setup_eth_options(&manager);

    // === Global Overview ===
    info!("\n--- Global Market Overview ---");
    let stats = manager.stats();
    info!("Total underlyings: {}", stats.underlying_count);
    info!("Total expirations: {}", stats.total_expirations);
    info!("Total strikes: {}", stats.total_strikes);
    info!("Total orders: {}", stats.total_orders);

    // =========================================
    // Trading Scenario 1: Market Maker Quoting
    // =========================================
    info!("\n\n=== Scenario 1: Market Maker Quoting ===\n");

    let exp_30 = ExpirationDate::Days(pos!(30.0));

    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp_book) = btc.get_expiration(&exp_30)
    {
        // Find ATM strike
        let spot = 50000u64;
        if let Ok(atm) = exp_book.atm_strike(spot) {
            info!("BTC Spot: {}", spot);
            info!("ATM Strike: {}", atm);

            // Quote around ATM
            let strikes_to_quote = [atm - 2000, atm - 1000, atm, atm + 1000, atm + 2000];

            info!("\nQuoting strikes: {:?}", strikes_to_quote);

            for &strike in &strikes_to_quote {
                if let Ok(s) = exp_book.get_strike(strike) {
                    // Update call quotes
                    s.call()
                        .add_limit_order(OrderId::new(), Side::Buy, 450, 50)
                        .unwrap();
                    s.call()
                        .add_limit_order(OrderId::new(), Side::Sell, 480, 50)
                        .unwrap();

                    // Update put quotes
                    s.put()
                        .add_limit_order(OrderId::new(), Side::Buy, 200, 50)
                        .unwrap();
                    s.put()
                        .add_limit_order(OrderId::new(), Side::Sell, 230, 50)
                        .unwrap();
                }
            }

            info!("Added market maker quotes to 5 strikes");
        }
    }

    // =========================================
    // Trading Scenario 2: Quote Retrieval
    // =========================================
    info!("\n\n=== Scenario 2: Quote Retrieval ===\n");

    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp_book) = btc.get_expiration(&exp_30)
    {
        info!("BTC 30-day Option Chain Quotes:\n");
        info!(
            "{:<10} {:>12} {:>12} {:>12} {:>12}",
            "Strike", "Call Bid", "Call Ask", "Put Bid", "Put Ask"
        );
        info!("{}", "-".repeat(60));

        for strike in exp_book.strike_prices() {
            if let Ok(s) = exp_book.get_strike(strike) {
                let call_quote = s.call_quote();
                let put_quote = s.put_quote();

                info!(
                    "{:<10} {:>12} {:>12} {:>12} {:>12}",
                    strike,
                    format!("{:?}", call_quote.bid_price()),
                    format!("{:?}", call_quote.ask_price()),
                    format!("{:?}", put_quote.bid_price()),
                    format!("{:?}", put_quote.ask_price()),
                );
            }
        }
    }

    // =========================================
    // Trading Scenario 3: Order Execution
    // =========================================
    info!("\n\n=== Scenario 3: Simulated Order Flow ===\n");

    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp_book) = btc.get_expiration(&exp_30)
        && let Ok(strike) = exp_book.get_strike(50000)
    {
        info!("Simulating order flow on BTC-50000 Call:\n");

        // Get initial state
        let initial_quote = strike.call_quote();
        info!(
            "Initial: {} @ {:?} / {} @ {:?}",
            initial_quote.bid_size(),
            initial_quote.bid_price(),
            initial_quote.ask_size(),
            initial_quote.ask_price()
        );

        // Simulate aggressive buyer
        info!("\n[Buyer] Lifting offer - buying 30 contracts");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 480, 30)
            .unwrap();

        // Simulate aggressive seller
        info!("[Seller] Hitting bid - selling 20 contracts");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 450, 20)
            .unwrap();

        // Check new state
        let new_quote = strike.call_quote();
        info!(
            "\nAfter flow: {} @ {:?} / {} @ {:?}",
            new_quote.bid_size(),
            new_quote.bid_price(),
            new_quote.ask_size(),
            new_quote.ask_price()
        );

        info!("\nTotal orders on strike: {}", strike.order_count());
    }

    // =========================================
    // Trading Scenario 4: Risk Analysis
    // =========================================
    info!("\n\n=== Scenario 4: Portfolio Risk View ===\n");

    if let Ok(btc) = manager.get("BTC") {
        let stats = btc.stats();
        info!("BTC Options Portfolio:");
        info!("  Expirations: {}", stats.expiration_count);
        info!("  Total strikes: {}", stats.total_strikes);
        info!("  Total orders: {}", stats.total_orders);

        // Check liquidity across expirations
        info!("\nLiquidity by Expiration:");
        for days in [7, 14, 21, 30, 60, 90] {
            let exp = ExpirationDate::Days(pos!(days as f64));
            if let Ok(exp_book) = btc.get_expiration(&exp) {
                info!(
                    "  {}-day: {} strikes, {} orders",
                    days,
                    exp_book.strike_count(),
                    exp_book.total_order_count()
                );
            }
        }
    }

    // =========================================
    // Trading Scenario 5: Cross-Asset View
    // =========================================
    info!("\n\n=== Scenario 5: Cross-Asset Analysis ===\n");

    info!("Market Summary by Underlying:\n");
    info!(
        "{:<10} {:>12} {:>12} {:>12}",
        "Symbol", "Expirations", "Strikes", "Orders"
    );
    info!("{}", "-".repeat(50));

    for symbol in manager.underlying_symbols() {
        if let Ok(underlying) = manager.get(&symbol) {
            let stats = underlying.stats();
            info!(
                "{:<10} {:>12} {:>12} {:>12}",
                stats.underlying, stats.expiration_count, stats.total_strikes, stats.total_orders
            );
        }
    }

    // =========================================
    // Trading Scenario 6: Option Style Access
    // =========================================
    info!("\n\n=== Scenario 6: Call vs Put Analysis ===\n");

    if let Ok(btc) = manager.get("BTC")
        && let Ok(exp_book) = btc.get_expiration(&exp_30)
        && let Ok(strike) = exp_book.get_strike(50000)
    {
        info!("BTC 50000 Strike Analysis:\n");

        // Access by option style
        let call = strike.get(OptionStyle::Call);
        let put = strike.get(OptionStyle::Put);

        info!("\nCall Option:");
        info!("  Orders: {}", call.order_count());
        let bid_depth = call.total_bid_depth();
        let ask_depth = call.total_ask_depth();
        info!("  Bid depth: {}", bid_depth);
        info!("  Ask depth: {}", ask_depth);
        let imbalance = call.imbalance(5);
        info!("  Imbalance: {:.1}%", imbalance * 100.0);

        info!("\nPut Option:");
        info!("  Orders: {}", put.order_count());
        let bid_depth = put.total_bid_depth();
        let ask_depth = put.total_ask_depth();
        info!("  Bid depth: {}", bid_depth);
        info!("  Ask depth: {}", ask_depth);
        let imbalance = put.imbalance(5);
        info!("  Imbalance: {:.1}%", imbalance * 100.0);

        info!(
            "\nPut/Call Ratio (by order count): {:.2}",
            put.order_count() as f64 / call.order_count() as f64
        );
    }

    // === Final Statistics ===
    info!("\n\n=== Final Market Statistics ===\n");
    let final_stats = manager.stats();
    info!("Underlyings: {}", final_stats.underlying_count);
    info!("Expirations: {}", final_stats.total_expirations);
    info!("Strikes: {}", final_stats.total_strikes);
    info!("Orders: {}", final_stats.total_orders);

    info!("\n=== Example Complete ===");
}

/// Sets up BTC options with multiple expirations and strikes.
fn setup_btc_options(manager: &UnderlyingOrderBookManager) {
    info!("Setting up BTC options...");

    let btc = manager.get_or_create("BTC");

    // Weekly expirations (1-4 weeks)
    for week in 1..=4 {
        let exp = ExpirationDate::Days(pos!((week * 7) as f64));
        let exp_book = btc.get_or_create_expiration(exp);

        // Strikes around 50000
        for strike in (46000..=54000).step_by(1000) {
            let s = exp_book.get_or_create_strike(strike);

            // Add initial liquidity
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 400, 10)
                .unwrap();
            s.call()
                .add_limit_order(OrderId::new(), Side::Sell, 450, 8)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 200, 10)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Sell, 250, 8)
                .unwrap();
        }
    }

    // Monthly expirations
    for month in [2, 3] {
        let exp = ExpirationDate::Days(pos!((month * 30) as f64));
        let exp_book = btc.get_or_create_expiration(exp);

        // Wider strike range for longer-dated
        for strike in (40000..=60000).step_by(2500) {
            let s = exp_book.get_or_create_strike(strike);

            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 500, 15)
                .unwrap();
            s.call()
                .add_limit_order(OrderId::new(), Side::Sell, 550, 12)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 300, 15)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Sell, 350, 12)
                .unwrap();
        }
    }

    let stats = btc.stats();
    info!(
        "  BTC: {} expirations, {} strikes, {} orders",
        stats.expiration_count, stats.total_strikes, stats.total_orders
    );
}

/// Sets up ETH options with multiple expirations and strikes.
fn setup_eth_options(manager: &UnderlyingOrderBookManager) {
    info!("Setting up ETH options...");

    let eth = manager.get_or_create("ETH");

    // Weekly and monthly expirations
    for days in [7, 14, 30, 60] {
        let exp = ExpirationDate::Days(pos!(days as f64));
        let exp_book = eth.get_or_create_expiration(exp);

        // Strikes around 3000
        for strike in (2600..=3400).step_by(100) {
            let s = exp_book.get_or_create_strike(strike);

            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 50, 20)
                .unwrap();
            s.call()
                .add_limit_order(OrderId::new(), Side::Sell, 60, 15)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 30, 20)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Sell, 40, 15)
                .unwrap();
        }
    }

    let stats = eth.stats();
    info!(
        "  ETH: {} expirations, {} strikes, {} orders",
        stats.expiration_count, stats.total_strikes, stats.total_orders
    );
}
