//! Benchmarks for full hierarchy operations.
//!
//! These benchmarks measure the performance of operations that span
//! the entire order book hierarchy.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::orderbook::UnderlyingOrderBookManager;
use optionstratlib::prelude::{ExpirationDate, Positive, pos_or_panic};
use orderbook_rs::{OrderId, Side};
use uuid::Uuid;

/// Benchmarks for full hierarchy traversal operations.
pub fn hierarchy_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("hierarchy");

    // Benchmark full path creation: underlying -> expiration -> strike -> order
    group.bench_function("full_path_creation", |b| {
        let manager = UnderlyingOrderBookManager::new();
        let mut counter = 0u64;
        b.iter(|| {
            let underlying = manager.get_or_create("BTC");
            let exp = ExpirationDate::Days(Positive::THIRTY);
            let exp_book = underlying.get_or_create_expiration(exp);
            let strike = exp_book.get_or_create_strike(50000 + counter * 100);
            strike
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                .unwrap();
            counter += 1;
        });
    });

    // Benchmark full path lookup: underlying -> expiration -> strike
    group.bench_function("full_path_lookup", |b| {
        let manager = UnderlyingOrderBookManager::new();
        let exp = ExpirationDate::Days(Positive::THIRTY);
        {
            let underlying = manager.get_or_create("BTC");
            let exp_book = underlying.get_or_create_expiration(exp);
            exp_book.get_or_create_strike(50000);
        }
        b.iter(|| {
            let underlying = manager.get("BTC").unwrap();
            let exp_book = underlying.get_expiration(&exp).unwrap();
            let _strike = exp_book.get_strike(50000).unwrap();
        });
    });

    // Benchmark adding orders across multiple strikes
    group.bench_function("add_orders_multi_strike", |b| {
        let manager = UnderlyingOrderBookManager::new();
        let exp = ExpirationDate::Days(Positive::THIRTY);
        {
            let underlying = manager.get_or_create("BTC");
            let exp_book = underlying.get_or_create_expiration(exp);
            for strike in (40000..60000).step_by(1000) {
                exp_book.get_or_create_strike(strike);
            }
        }
        b.iter(|| {
            let underlying = manager.get("BTC").unwrap();
            let exp_book = underlying.get_expiration(&exp).unwrap();
            for strike in (40000..60000).step_by(1000) {
                let s = exp_book.get_strike(strike).unwrap();
                s.call()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                    .unwrap();
            }
        });
    });

    // Benchmark global stats aggregation
    group.bench_function("global_stats_aggregation", |b| {
        let manager = UnderlyingOrderBookManager::new();
        // Setup: create a realistic structure
        for symbol in ["BTC", "ETH", "SPX"] {
            let underlying = manager.get_or_create(symbol);
            for days in [30, 60, 90] {
                let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
                let exp_book = underlying.get_or_create_expiration(exp);
                for strike in (40000..60000).step_by(5000) {
                    let s = exp_book.get_or_create_strike(strike);
                    s.call()
                        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                        .unwrap();
                    s.put()
                        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
                        .unwrap();
                }
            }
        }
        b.iter(|| manager.stats());
    });

    group.finish();
}

/// Benchmarks for realistic trading scenarios.
pub fn trading_scenarios(c: &mut Criterion) {
    let mut group = c.benchmark_group("trading_scenarios");

    // Scenario: Market maker quoting multiple strikes
    group.bench_function("market_maker_quoting", |b| {
        let manager = UnderlyingOrderBookManager::new();
        let exp = ExpirationDate::Days(Positive::THIRTY);
        {
            let underlying = manager.get_or_create("BTC");
            let exp_book = underlying.get_or_create_expiration(exp);
            for strike in (45000..55000).step_by(1000) {
                exp_book.get_or_create_strike(strike);
            }
        }
        b.iter(|| {
            let underlying = manager.get("BTC").unwrap();
            let exp_book = underlying.get_expiration(&exp).unwrap();
            // Quote all strikes
            for strike in (45000..55000).step_by(1000) {
                let s = exp_book.get_strike(strike).unwrap();
                // Add bid/ask for call
                s.call()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                    .unwrap();
                s.call()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 105, 10)
                    .unwrap();
                // Add bid/ask for put
                s.put()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
                    .unwrap();
                s.put()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 55, 10)
                    .unwrap();
            }
        });
    });

    // Scenario: Quote retrieval for risk calculation
    group.bench_function("quote_retrieval_all_strikes", |b| {
        let manager = UnderlyingOrderBookManager::new();
        let exp = ExpirationDate::Days(Positive::THIRTY);
        {
            let underlying = manager.get_or_create("BTC");
            let exp_book = underlying.get_or_create_expiration(exp);
            for strike in (40000..60000).step_by(1000) {
                let s = exp_book.get_or_create_strike(strike);
                s.call()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                    .unwrap();
                s.call()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 105, 5)
                    .unwrap();
                s.put()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
                    .unwrap();
                s.put()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 55, 5)
                    .unwrap();
            }
        }
        b.iter(|| {
            let underlying = manager.get("BTC").unwrap();
            let exp_book = underlying.get_expiration(&exp).unwrap();
            let mut quotes = Vec::new();
            for strike in (40000..60000).step_by(1000) {
                let s = exp_book.get_strike(strike).unwrap();
                quotes.push((
                    s.call_quote().expect("call quote"),
                    s.put_quote().expect("put quote"),
                ));
            }
            quotes
        });
    });

    // Scenario: ATM strike lookup and order placement
    group.bench_function("atm_order_placement", |b| {
        let manager = UnderlyingOrderBookManager::new();
        let exp = ExpirationDate::Days(Positive::THIRTY);
        {
            let underlying = manager.get_or_create("BTC");
            let exp_book = underlying.get_or_create_expiration(exp);
            for strike in (40000..60000).step_by(1000) {
                exp_book.get_or_create_strike(strike);
            }
        }
        let spot_price = 50500u64;
        b.iter(|| {
            let underlying = manager.get("BTC").unwrap();
            let exp_book = underlying.get_expiration(&exp).unwrap();
            let atm = exp_book.atm_strike(spot_price).unwrap();
            let strike = exp_book.get_strike(atm).unwrap();
            strike
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                .unwrap();
        });
    });

    group.finish();
}

/// Benchmarks for hierarchy scaling.
pub fn hierarchy_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("hierarchy_scaling");

    // Scale by number of underlyings
    for num_underlyings in [1, 3, 5, 10].iter() {
        group.throughput(Throughput::Elements(*num_underlyings as u64));

        group.bench_with_input(
            BenchmarkId::new("stats_by_underlyings", num_underlyings),
            num_underlyings,
            |b, &num_underlyings| {
                let manager = UnderlyingOrderBookManager::new();
                for i in 0..num_underlyings {
                    let underlying = manager.get_or_create(format!("SYM{}", i));
                    let exp = ExpirationDate::Days(Positive::THIRTY);
                    let exp_book = underlying.get_or_create_expiration(exp);
                    for strike in (40000..60000).step_by(5000) {
                        let s = exp_book.get_or_create_strike(strike);
                        s.call()
                            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                            .unwrap();
                    }
                }
                b.iter(|| manager.stats());
            },
        );
    }

    // Scale by number of strikes per expiration
    for num_strikes in [5, 20, 50, 100].iter() {
        group.throughput(Throughput::Elements(*num_strikes));

        group.bench_with_input(
            BenchmarkId::new("total_order_count_by_strikes", num_strikes),
            num_strikes,
            |b, &num_strikes| {
                let manager = UnderlyingOrderBookManager::new();
                let underlying = manager.get_or_create("BTC");
                let exp = ExpirationDate::Days(Positive::THIRTY);
                let exp_book = underlying.get_or_create_expiration(exp);
                for i in 0..num_strikes {
                    let s = exp_book.get_or_create_strike(40000 + i * 100);
                    s.call()
                        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                        .unwrap();
                    s.put()
                        .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
                        .unwrap();
                }
                b.iter(|| manager.total_order_count());
            },
        );
    }

    // Scale by total structure size (underlyings * expirations * strikes)
    for scale in [1, 2, 4, 8].iter() {
        let total_elements = scale * scale * scale * 4; // 4 strikes base
        group.throughput(Throughput::Elements(total_elements));

        group.bench_with_input(
            BenchmarkId::new("full_hierarchy_scale", scale),
            scale,
            |b, &scale| {
                let manager = UnderlyingOrderBookManager::new();
                for u in 0..scale {
                    let underlying = manager.get_or_create(format!("SYM{}", u));
                    for e in 0..scale {
                        let exp = ExpirationDate::Days(pos_or_panic!((30 + e * 30) as f64));
                        let exp_book = underlying.get_or_create_expiration(exp);
                        for s in 0..(scale * 4) {
                            let strike = exp_book.get_or_create_strike(40000 + s * 1000);
                            strike
                                .call()
                                .add_limit_order(
                                    OrderId::from_uuid(Uuid::new_v4()),
                                    Side::Buy,
                                    100,
                                    10,
                                )
                                .unwrap();
                        }
                    }
                }
                b.iter(|| manager.stats());
            },
        );
    }

    group.finish();
}
