//! Benchmarks for strike order book operations.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::orderbook::{StrikeOrderBook, StrikeOrderBookManager};
use optionstratlib::ExpirationDate;
use optionstratlib::prelude::Positive;
use orderbook_rs::{OrderId, Side};
use uuid::Uuid;

/// Creates a test expiration date.
fn test_expiration() -> ExpirationDate {
    ExpirationDate::Days(Positive::THIRTY)
}

/// Benchmarks for StrikeOrderBook operations.
pub fn strike_orderbook_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("strike_orderbook");

    // Benchmark creating a new strike order book
    group.bench_function("new", |b| {
        b.iter(|| StrikeOrderBook::new("BTC", test_expiration(), 50000));
    });

    // Benchmark adding orders to call side
    group.bench_function("add_call_order", |b| {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        b.iter(|| {
            strike
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                .unwrap();
        });
    });

    // Benchmark adding orders to put side
    group.bench_function("add_put_order", |b| {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        b.iter(|| {
            strike
                .put()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
                .unwrap();
        });
    });

    // Benchmark getting call quote
    group.bench_function("call_quote", |b| {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        strike
            .call()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
            .unwrap();
        strike
            .call()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 105, 5)
            .unwrap();
        b.iter(|| strike.call_quote().expect("call quote"));
    });

    // Benchmark getting put quote
    group.bench_function("put_quote", |b| {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        strike
            .put()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
            .unwrap();
        strike
            .put()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 55, 5)
            .unwrap();
        b.iter(|| strike.put_quote().expect("put quote"));
    });

    // Benchmark is_fully_quoted check
    group.bench_function("is_fully_quoted", |b| {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        strike
            .call()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
            .unwrap();
        strike
            .call()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 105, 5)
            .unwrap();
        strike
            .put()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
            .unwrap();
        strike
            .put()
            .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Sell, 55, 5)
            .unwrap();
        b.iter(|| strike.is_fully_quoted());
    });

    // Benchmark order_count
    group.bench_function("order_count", |b| {
        let strike = StrikeOrderBook::new("BTC", test_expiration(), 50000);
        for _ in 0..50 {
            strike
                .call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                .unwrap();
            strike
                .put()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
                .unwrap();
        }
        b.iter(|| strike.order_count());
    });

    group.finish();
}

/// Benchmarks for StrikeOrderBookManager operations.
pub fn strike_manager_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("strike_manager");

    // Benchmark creating a new manager
    group.bench_function("new", |b| {
        b.iter(|| StrikeOrderBookManager::new("BTC", test_expiration()));
    });

    // Benchmark get_or_create
    group.bench_function("get_or_create", |b| {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());
        let mut strike = 50000u64;
        b.iter(|| {
            manager.get_or_create(strike);
            strike += 1000;
        });
    });

    // Benchmark get existing strike
    group.bench_function("get_existing", |b| {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());
        manager.get_or_create(50000);
        b.iter(|| manager.get(50000));
    });

    // Benchmark contains check
    group.bench_function("contains", |b| {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());
        manager.get_or_create(50000);
        b.iter(|| manager.contains(50000));
    });

    // Benchmark atm_strike lookup
    group.bench_function("atm_strike", |b| {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            manager.get_or_create(strike);
        }
        b.iter(|| manager.atm_strike(50500));
    });

    // Benchmark strike_prices
    group.bench_function("strike_prices", |b| {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            manager.get_or_create(strike);
        }
        b.iter(|| manager.strike_prices());
    });

    // Benchmark total_order_count
    group.bench_function("total_order_count", |b| {
        let manager = StrikeOrderBookManager::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            let s = manager.get_or_create(strike);
            s.call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                .unwrap();
        }
        b.iter(|| manager.total_order_count());
    });

    group.finish();
}

/// Benchmarks for strike manager scaling.
pub fn strike_manager_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("strike_manager_scaling");

    for num_strikes in [10, 50, 100, 500].iter() {
        group.throughput(Throughput::Elements(*num_strikes));

        group.bench_with_input(
            BenchmarkId::new("create_strikes", num_strikes),
            num_strikes,
            |b, &num_strikes| {
                b.iter_batched(
                    || StrikeOrderBookManager::new("BTC", test_expiration()),
                    |manager| {
                        for i in 0..num_strikes {
                            manager.get_or_create(40000 + i * 100);
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("atm_strike_with_n_strikes", num_strikes),
            num_strikes,
            |b, &num_strikes| {
                let manager = StrikeOrderBookManager::new("BTC", test_expiration());
                for i in 0..num_strikes {
                    manager.get_or_create(40000 + i * 100);
                }
                b.iter(|| manager.atm_strike(50000));
            },
        );
    }

    group.finish();
}
