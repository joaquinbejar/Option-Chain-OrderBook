//! Benchmarks for option chain order book operations.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::orderbook::{OptionChainOrderBook, OptionChainOrderBookManager};
use optionstratlib::prelude::{ExpirationDate, Positive, pos_or_panic};
use orderbook_rs::{OrderId, Side};

/// Creates a test expiration date.
fn test_expiration() -> ExpirationDate {
    ExpirationDate::Days(Positive::THIRTY)
}

/// Benchmarks for OptionChainOrderBook operations.
pub fn chain_orderbook_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("chain_orderbook");

    // Benchmark creating a new option chain order book
    group.bench_function("new", |b| {
        b.iter(|| OptionChainOrderBook::new("BTC", test_expiration()));
    });

    // Benchmark get_or_create_strike
    group.bench_function("get_or_create_strike", |b| {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        let mut strike = 50000u64;
        b.iter(|| {
            chain.get_or_create_strike(strike);
            strike += 1000;
        });
    });

    // Benchmark get_strike existing
    group.bench_function("get_strike_existing", |b| {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        chain.get_or_create_strike(50000);
        b.iter(|| chain.get_strike(50000));
    });

    // Benchmark strike_count
    group.bench_function("strike_count", |b| {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            chain.get_or_create_strike(strike);
        }
        b.iter(|| chain.strike_count());
    });

    // Benchmark strike_prices
    group.bench_function("strike_prices", |b| {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            chain.get_or_create_strike(strike);
        }
        b.iter(|| chain.strike_prices());
    });

    // Benchmark total_order_count
    group.bench_function("total_order_count", |b| {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            let s = chain.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
            s.put()
                .add_limit_order(OrderId::new(), Side::Buy, 50, 10)
                .unwrap();
        }
        b.iter(|| chain.total_order_count());
    });

    // Benchmark atm_strike
    group.bench_function("atm_strike", |b| {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            chain.get_or_create_strike(strike);
        }
        b.iter(|| chain.atm_strike(50500));
    });

    // Benchmark stats
    group.bench_function("stats", |b| {
        let chain = OptionChainOrderBook::new("BTC", test_expiration());
        for strike in (40000..60000).step_by(1000) {
            let s = chain.get_or_create_strike(strike);
            s.call()
                .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
        }
        b.iter(|| chain.stats());
    });

    // Benchmark a WIDE-chain mass-cancel sweep (issue #81 Part B: the per-child
    // result Vec is pre-sized to the strike count, eliminating the reallocation
    // churn the old `Vec::new()` + push loop incurred). 500 strikes, both legs
    // populated; the tree is rebuilt each iteration because cancel is
    // destructive.
    group.bench_function("cancel_all_wide", |b| {
        b.iter_batched(
            || {
                let chain = OptionChainOrderBook::new("BTC", test_expiration());
                for strike in (10000..60000).step_by(100) {
                    let s = chain.get_or_create_strike(strike);
                    s.call()
                        .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                        .unwrap();
                    s.put()
                        .add_limit_order(OrderId::new(), Side::Sell, 50, 10)
                        .unwrap();
                }
                chain
            },
            |chain| chain.cancel_all().unwrap(),
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmarks for OptionChainOrderBookManager operations.
pub fn chain_manager_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("chain_manager");

    // Benchmark creating a new manager
    group.bench_function("new", |b| {
        b.iter(|| OptionChainOrderBookManager::new("BTC"));
    });

    // Benchmark get_or_create
    group.bench_function("get_or_create", |b| {
        let manager = OptionChainOrderBookManager::new("BTC");
        let mut days = 30.0;
        b.iter(|| {
            let exp = ExpirationDate::Days(pos_or_panic!(days));
            manager.get_or_create(exp);
            days += 7.0;
        });
    });

    // Benchmark get existing
    group.bench_function("get_existing", |b| {
        let manager = OptionChainOrderBookManager::new("BTC");
        let exp = test_expiration();
        manager.get_or_create(exp);
        b.iter(|| manager.get(&exp));
    });

    // Benchmark contains
    group.bench_function("contains", |b| {
        let manager = OptionChainOrderBookManager::new("BTC");
        let exp = test_expiration();
        manager.get_or_create(exp);
        b.iter(|| manager.contains(&exp));
    });

    // Benchmark total_order_count
    group.bench_function("total_order_count", |b| {
        let manager = OptionChainOrderBookManager::new("BTC");
        for days in [30, 60, 90] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let chain = manager.get_or_create(exp);
            for strike in (40000..60000).step_by(5000) {
                let s = chain.get_or_create_strike(strike);
                s.call()
                    .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                    .unwrap();
            }
        }
        b.iter(|| manager.total_order_count());
    });

    group.finish();
}

/// Benchmarks for chain manager scaling.
pub fn chain_manager_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("chain_manager_scaling");

    for num_expirations in [3, 6, 12, 24].iter() {
        group.throughput(Throughput::Elements(*num_expirations as u64));

        group.bench_with_input(
            BenchmarkId::new("create_expirations", num_expirations),
            num_expirations,
            |b, &num_expirations| {
                b.iter_batched(
                    || OptionChainOrderBookManager::new("BTC"),
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

        group.bench_with_input(
            BenchmarkId::new("total_order_count_with_n_expirations", num_expirations),
            num_expirations,
            |b, &num_expirations| {
                let manager = OptionChainOrderBookManager::new("BTC");
                for i in 0..num_expirations {
                    let exp = ExpirationDate::Days(pos_or_panic!((30 + i * 7) as f64));
                    let chain = manager.get_or_create(exp);
                    for strike in (40000..60000).step_by(5000) {
                        let s = chain.get_or_create_strike(strike);
                        s.call()
                            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                            .unwrap();
                    }
                }
                b.iter(|| manager.total_order_count());
            },
        );
    }

    group.finish();
}
