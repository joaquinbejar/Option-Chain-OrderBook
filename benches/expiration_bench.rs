//! Benchmarks for expiration order book operations.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::orderbook::{ExpirationOrderBook, ExpirationOrderBookManager};
use optionstratlib::prelude::{ExpirationDate, Positive, pos_or_panic};
use orderbook_rs::{OrderId, Side};
use uuid::Uuid;

/// Creates a test expiration date.
fn test_expiration() -> ExpirationDate {
    ExpirationDate::Days(Positive::THIRTY)
}

/// Benchmarks for ExpirationOrderBook operations.
pub fn expiration_orderbook_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("expiration_orderbook");

    // Benchmark creating a new expiration order book
    group.bench_function("new", |b| {
        b.iter(|| ExpirationOrderBook::new("BTC", test_expiration()));
    });

    // Benchmark get_or_create_strike
    group.bench_function("get_or_create_strike", |b| {
        let exp_book = ExpirationOrderBook::new("BTC", test_expiration());
        let mut strike = 50000u64;
        b.iter(|| {
            exp_book.get_or_create_strike(strike);
            strike += 1000;
        });
    });

    // Benchmark get_strike existing
    group.bench_function("get_strike_existing", |b| {
        let exp_book = ExpirationOrderBook::new("BTC", test_expiration());
        exp_book.get_or_create_strike(50000);
        b.iter(|| exp_book.get_strike(50000));
    });

    // Benchmark strike_count
    group.bench_function("strike_count", |b| {
        let exp_book = ExpirationOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            exp_book.get_or_create_strike(strike);
        }
        b.iter(|| exp_book.strike_count());
    });

    // Benchmark strike_prices
    group.bench_function("strike_prices", |b| {
        let exp_book = ExpirationOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            exp_book.get_or_create_strike(strike);
        }
        b.iter(|| exp_book.strike_prices());
    });

    // Benchmark total_order_count
    group.bench_function("total_order_count", |b| {
        let exp_book = ExpirationOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            let s = exp_book.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 50, 10)
                .unwrap();
        }
        b.iter(|| exp_book.total_order_count());
    });

    // Benchmark atm_strike
    group.bench_function("atm_strike", |b| {
        let exp_book = ExpirationOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            exp_book.get_or_create_strike(strike);
        }
        b.iter(|| exp_book.atm_strike(50500));
    });

    group.finish();
}

/// Benchmarks for ExpirationOrderBookManager operations.
pub fn expiration_manager_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("expiration_manager");

    // Benchmark creating a new manager
    group.bench_function("new", |b| {
        b.iter(|| ExpirationOrderBookManager::new("BTC"));
    });

    // Benchmark get_or_create
    group.bench_function("get_or_create", |b| {
        let manager = ExpirationOrderBookManager::new("BTC");
        let mut days = 30.0;
        b.iter(|| {
            let exp = ExpirationDate::Days(pos_or_panic!(days));
            manager.get_or_create(exp);
            days += 7.0;
        });
    });

    // Benchmark get existing
    group.bench_function("get_existing", |b| {
        let manager = ExpirationOrderBookManager::new("BTC");
        let exp = test_expiration();
        manager.get_or_create(exp);
        b.iter(|| manager.get(&exp));
    });

    // Benchmark contains
    group.bench_function("contains", |b| {
        let manager = ExpirationOrderBookManager::new("BTC");
        let exp = test_expiration();
        manager.get_or_create(exp);
        b.iter(|| manager.contains(&exp));
    });

    // Benchmark total_order_count
    group.bench_function("total_order_count", |b| {
        let manager = ExpirationOrderBookManager::new("BTC");
        for days in [30, 60, 90] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let exp_book = manager.get_or_create(exp);
            for strike in (40000..60000).step_by(5000) {
                let s = exp_book.get_or_create_strike(strike);
                s.call()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                    .unwrap();
            }
        }
        b.iter(|| manager.total_order_count());
    });

    // Benchmark total_strike_count
    group.bench_function("total_strike_count", |b| {
        let manager = ExpirationOrderBookManager::new("BTC");
        for days in [30, 60, 90] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let exp_book = manager.get_or_create(exp);
            for strike in (40000..60000).step_by(5000) {
                exp_book.get_or_create_strike(strike);
            }
        }
        b.iter(|| manager.total_strike_count());
    });

    // Benchmark stats
    group.bench_function("stats", |b| {
        let manager = ExpirationOrderBookManager::new("BTC");
        for days in [30, 60, 90] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let exp_book = manager.get_or_create(exp);
            for strike in (40000..60000).step_by(5000) {
                let s = exp_book.get_or_create_strike(strike);
                s.call()
                    .add_limit_order(OrderId::from_uuid(Uuid::new_v4()), Side::Buy, 100, 10)
                    .unwrap();
            }
        }
        b.iter(|| manager.stats());
    });

    group.finish();
}

/// Benchmarks for expiration manager scaling.
pub fn expiration_manager_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("expiration_manager_scaling");

    for num_expirations in [3, 6, 12, 24].iter() {
        group.throughput(Throughput::Elements(*num_expirations as u64));

        group.bench_with_input(
            BenchmarkId::new("create_expirations", num_expirations),
            num_expirations,
            |b, &num_expirations| {
                b.iter_batched(
                    || ExpirationOrderBookManager::new("BTC"),
                    |manager| {
                        for i in 0..num_expirations {
                            let exp = ExpirationDate::Days(pos_or_panic!((30 + i * 7) as f64));
                            manager.get_or_create(exp);
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}
