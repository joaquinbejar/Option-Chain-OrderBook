//! Example 08: Order Lifecycle Tracking
//!
//! This example demonstrates order state tracking and lifecycle queries across
//! the option chain hierarchy.
//!
//! Topics covered:
//! - Querying order status (`get_order_status`)
//! - Viewing order history (`get_order_history`)
//! - Active and terminal order counts
//! - Finding orders across hierarchy levels (`find_order`)
//! - Querying orders by user (`orders_by_user`)
//! - Terminal order summaries (`terminal_order_summary`)
//! - Purging old terminal states (`purge_terminal_states`)

use option_chain_orderbook::orderbook::{OptionOrderBook, UnderlyingOrderBookManager};
use optionstratlib::prelude::pos_or_panic;
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::{OrderId, Side};
use pricelevel::Hash32;
use std::time::Duration;
use uuid::Uuid;

fn main() {
    println!("=== Order Lifecycle Tracking Example ===\n");

    // ── Part 1: Single OptionOrderBook lifecycle ───────────────────────────
    println!("--- Part 1: Single OptionOrderBook Lifecycle ---\n");

    let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

    // Add some orders
    let order1 = OrderId::from_uuid(Uuid::new_v4());
    let order2 = OrderId::from_uuid(Uuid::new_v4());
    let order3 = OrderId::from_uuid(Uuid::new_v4());

    book.add_limit_order(order1, Side::Buy, 100, 10)
        .expect("add order1");
    book.add_limit_order(order2, Side::Buy, 99, 5)
        .expect("add order2");
    book.add_limit_order(order3, Side::Sell, 105, 15)
        .expect("add order3");

    println!("Added 3 orders to the book");
    println!("Active order count: {}", book.active_order_count());
    println!("Terminal order count: {}", book.terminal_order_count());

    // Query order status
    if let Some(status) = book.get_order_status(order1) {
        println!("\nOrder 1 status: {:?}", status);
    }

    // Query order history
    if let Some(history) = book.get_order_history(order1) {
        println!("Order 1 history ({} transitions):", history.len());
        for (ts, status) in &history {
            println!("  {} ns -> {:?}", ts, status);
        }
    }

    // Match some orders
    println!("\n--- Matching orders ---");
    let taker = OrderId::from_uuid(Uuid::new_v4());
    book.add_limit_order(taker, Side::Sell, 99, 15)
        .expect("add taker");

    println!("After match:");
    println!("  Active orders: {}", book.active_order_count());
    println!("  Terminal orders: {}", book.terminal_order_count());

    // Check terminal order summary
    let summary = book.terminal_order_summary();
    println!(
        "  Terminal summary: {} filled, {} cancelled, {} rejected",
        summary.filled, summary.cancelled, summary.rejected
    );

    // Check order1 status after match
    if let Some(status) = book.get_order_status(order1) {
        println!("\nOrder 1 status after match: {:?}", status);
    }

    // ── Part 2: Orders by user ─────────────────────────────────────────────
    println!("\n--- Part 2: Orders By User ---\n");

    let book2 = OptionOrderBook::new("ETH-20240329-3000-C", OptionStyle::Call);
    let user_alice = Hash32::from([1u8; 32]);
    let user_bob = Hash32::from([2u8; 32]);

    // Alice places 3 orders
    book2
        .add_limit_order_with_user(
            OrderId::from_uuid(Uuid::new_v4()),
            Side::Buy,
            100,
            10,
            user_alice,
        )
        .expect("alice1");
    book2
        .add_limit_order_with_user(
            OrderId::from_uuid(Uuid::new_v4()),
            Side::Buy,
            99,
            5,
            user_alice,
        )
        .expect("alice2");
    book2
        .add_limit_order_with_user(
            OrderId::from_uuid(Uuid::new_v4()),
            Side::Sell,
            110,
            8,
            user_alice,
        )
        .expect("alice3");

    // Bob places 2 orders
    book2
        .add_limit_order_with_user(
            OrderId::from_uuid(Uuid::new_v4()),
            Side::Buy,
            98,
            20,
            user_bob,
        )
        .expect("bob1");
    book2
        .add_limit_order_with_user(
            OrderId::from_uuid(Uuid::new_v4()),
            Side::Sell,
            112,
            15,
            user_bob,
        )
        .expect("bob2");

    let alice_orders = book2.orders_by_user(user_alice);
    let bob_orders = book2.orders_by_user(user_bob);

    println!("Alice has {} active orders", alice_orders.len());
    println!("Bob has {} active orders", bob_orders.len());

    // ── Part 3: Hierarchy-level find_order ─────────────────────────────────
    println!("\n--- Part 3: Hierarchical Order Search ---\n");

    let manager = UnderlyingOrderBookManager::new();
    let btc = manager.get_or_create("BTC");
    let exp = btc.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(30.0)));
    let strike = exp.get_or_create_strike(50000);

    // Place an order deep in the hierarchy
    let deep_order = OrderId::from_uuid(Uuid::new_v4());
    strike
        .call()
        .add_limit_order(deep_order, Side::Buy, 500, 10)
        .expect("deep order");

    println!("Placed order in BTC/30d/50000/Call");

    // Find it from different levels
    if let Some((sym, status)) = strike.find_order(deep_order) {
        println!("Found at strike level: {} -> {:?}", sym, status);
    }

    if let Some((sym, status)) = exp.find_order(deep_order) {
        println!("Found at expiration level: {} -> {:?}", sym, status);
    }

    if let Some((sym, status)) = btc.find_order(deep_order) {
        println!("Found at underlying level: {} -> {:?}", sym, status);
    }

    if let Some((sym, status)) = manager.find_order_across_underlyings(deep_order) {
        println!("Found at manager level: {} -> {:?}", sym, status);
    }

    // Unknown order returns None
    let unknown = OrderId::from_uuid(Uuid::new_v4());
    if manager.find_order_across_underlyings(unknown).is_none() {
        println!("Unknown order correctly returns None");
    }

    // ── Part 4: Aggregate queries ──────────────────────────────────────────
    println!("\n--- Part 4: Aggregate Queries ---\n");

    // Add more orders across the hierarchy
    let strike2 = exp.get_or_create_strike(55000);
    strike2
        .call()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 400, 5)
        .expect("s2c1");
    strike2
        .put()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 300, 8)
        .expect("s2p1");

    println!(
        "Total active orders across manager: {}",
        manager.total_active_orders_across_underlyings()
    );
    println!("Total active orders in BTC: {}", btc.total_active_orders());
    println!(
        "Total active orders in expiration: {}",
        exp.total_active_orders()
    );

    // Terminal summary at manager level
    let global_summary = manager.terminal_order_summary_across_underlyings();
    println!(
        "Global terminal summary: {} filled, {} cancelled, {} rejected",
        global_summary.filled, global_summary.cancelled, global_summary.rejected
    );

    // ── Part 5: Purging terminal states ────────────────────────────────────
    println!("\n--- Part 5: Purging Terminal States ---\n");

    // Create and fill some orders
    let fill_strike = exp.get_or_create_strike(60000);
    fill_strike
        .call()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 200, 10)
        .expect("maker");
    fill_strike
        .call()
        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 200, 10)
        .expect("taker");

    println!(
        "Before purge - terminal count at strike: {} + {}",
        fill_strike.call().terminal_order_count(),
        fill_strike.put().terminal_order_count()
    );

    // Wait briefly and purge
    std::thread::sleep(Duration::from_millis(10));
    let purged = manager.purge_terminal_states_across_underlyings(Duration::from_millis(1));
    println!("Purged {} terminal entries", purged);

    println!(
        "After purge - terminal count at strike: {} + {}",
        fill_strike.call().terminal_order_count(),
        fill_strike.put().terminal_order_count()
    );

    println!("\n=== Example Complete ===");
}
