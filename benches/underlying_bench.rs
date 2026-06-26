//! Benchmarks for underlying order book operations.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::orderbook::{UnderlyingOrderBook, UnderlyingOrderBookManager};
use optionstratlib::prelude::{ExpirationDate, Positive, pos_or_panic};
use orderbook_rs::{OrderId, Side};

/// Creates a test expiration date.
fn test_expiration() -> ExpirationDate {
    ExpirationDate::Days(Positive::THIRTY)
}

/// Benchmarks for UnderlyingOrderBook operations.
pub fn underlying_orderbook_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("underlying_orderbook");

    // Benchmark creating a new underlying order book
    group.bench_function("new", |b| {
        b.iter(|| UnderlyingOrderBook::new("BTC"));
    });

    // Benchmark get_or_create_expiration
    group.bench_function("get_or_create_expiration", |b| {
        let underlying = UnderlyingOrderBook::new("BTC");
        let mut days = 30.0;
        b.iter(|| {
            let exp = ExpirationDate::Days(pos_or_panic!(days));
            underlying.get_or_create_expiration(exp);
            days += 7.0;
        });
    });

    // Benchmark get_expiration existing
    group.bench_function("get_expiration_existing", |b| {
        let underlying = UnderlyingOrderBook::new("BTC");
        let exp = test_expiration();
        underlying.get_or_create_expiration(exp);
        b.iter(|| underlying.get_expiration(&exp));
    });

    // Benchmark expiration_count
    group.bench_function("expiration_count", |b| {
        let underlying = UnderlyingOrderBook::new("BTC");
        for days in [30, 60, 90, 120, 150, 180] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            underlying.get_or_create_expiration(exp);
        }
        b.iter(|| underlying.expiration_count());
    });

    // Benchmark total_strike_count
    group.bench_function("total_strike_count", |b| {
        let underlying = UnderlyingOrderBook::new("BTC");
        for days in [30, 60, 90] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let exp_book = underlying.get_or_create_expiration(exp);
            for strike in (40000..60000).step_by(5000) {
                exp_book.get_or_create_strike(strike);
            }
        }
        b.iter(|| underlying.total_strike_count());
    });

    // Benchmark total_order_count
    group.bench_function("total_order_count", |b| {
        let underlying = UnderlyingOrderBook::new("BTC");
        for days in [30, 60, 90] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let exp_book = underlying.get_or_create_expiration(exp);
            for strike in (40000..60000).step_by(5000) {
                let s = exp_book.get_or_create_strike(strike);
                s.call()
                    .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                    .unwrap();
            }
        }
        b.iter(|| underlying.total_order_count());
    });

    // Benchmark stats
    group.bench_function("stats", |b| {
        let underlying = UnderlyingOrderBook::new("BTC");
        for days in [30, 60, 90] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let exp_book = underlying.get_or_create_expiration(exp);
            for strike in (40000..60000).step_by(5000) {
                let s = exp_book.get_or_create_strike(strike);
                s.call()
                    .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                    .unwrap();
            }
        }
        b.iter(|| underlying.stats());
    });

    // Benchmark a WIDE-chain `stats()` (issue #81 Part A: the single-pass
    // accumulator replaces the three independent subtree walks
    // expiration_count() + total_strike_count() + total_order_count() with one
    // coherent traversal). 4 expirations x 250 strikes = 1000 strikes, with the
    // put leg populated on every other strike so the order tally is non-trivial.
    group.bench_function("stats_wide_chain", |b| {
        let underlying = UnderlyingOrderBook::new("BTC");
        for days in [30, 60, 90, 120] {
            let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
            let exp_book = underlying.get_or_create_expiration(exp);
            for i in 0..250u64 {
                let s = exp_book.get_or_create_strike(10000 + i * 100);
                s.call()
                    .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                    .unwrap();
                if i % 2 == 0 {
                    s.put()
                        .add_limit_order(OrderId::new(), Side::Sell, 50, 10)
                        .unwrap();
                }
            }
        }
        b.iter(|| underlying.stats());
    });

    group.finish();
}

/// Benchmarks for UnderlyingOrderBookManager operations.
pub fn underlying_manager_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("underlying_manager");

    // Benchmark creating a new manager
    group.bench_function("new", |b| {
        b.iter(UnderlyingOrderBookManager::new);
    });

    // Benchmark get_or_create
    group.bench_function("get_or_create", |b| {
        let manager = UnderlyingOrderBookManager::new();
        let symbols = ["BTC", "ETH", "SPX", "AAPL", "TSLA", "NVDA"];
        let mut idx = 0;
        b.iter(|| {
            manager.get_or_create(symbols[idx % symbols.len()]);
            idx += 1;
        });
    });

    // Benchmark get existing
    group.bench_function("get_existing", |b| {
        let manager = UnderlyingOrderBookManager::new();
        manager.get_or_create("BTC");
        b.iter(|| manager.get("BTC"));
    });

    // Benchmark contains
    group.bench_function("contains", |b| {
        let manager = UnderlyingOrderBookManager::new();
        manager.get_or_create("BTC");
        b.iter(|| manager.contains("BTC"));
    });

    // Benchmark underlying_symbols
    group.bench_function("underlying_symbols", |b| {
        let manager = UnderlyingOrderBookManager::new();
        for symbol in ["BTC", "ETH", "SPX", "AAPL", "TSLA", "NVDA"] {
            manager.get_or_create(symbol);
        }
        b.iter(|| manager.underlying_symbols());
    });

    // Benchmark total_order_count
    group.bench_function("total_order_count", |b| {
        let manager = UnderlyingOrderBookManager::new();
        for symbol in ["BTC", "ETH"] {
            let underlying = manager.get_or_create(symbol);
            for days in [30, 60] {
                let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
                let exp_book = underlying.get_or_create_expiration(exp);
                for strike in (40000..60000).step_by(10000) {
                    let s = exp_book.get_or_create_strike(strike);
                    s.call()
                        .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                        .unwrap();
                }
            }
        }
        b.iter(|| manager.total_order_count());
    });

    // Benchmark stats
    group.bench_function("stats", |b| {
        let manager = UnderlyingOrderBookManager::new();
        for symbol in ["BTC", "ETH"] {
            let underlying = manager.get_or_create(symbol);
            for days in [30, 60] {
                let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
                let exp_book = underlying.get_or_create_expiration(exp);
                for strike in (40000..60000).step_by(10000) {
                    let s = exp_book.get_or_create_strike(strike);
                    s.call()
                        .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                        .unwrap();
                }
            }
        }
        b.iter(|| manager.stats());
    });

    // Benchmark a HIGH-FANOUT manager `stats()` (issue #81 Part A): many
    // underlyings x many expirations x few strikes each — the shape that most
    // exercises the single-pass accumulator (the manager walks the `underlyings`
    // map ONCE instead of len + total_expiration_count + total_strike_count +
    // total_order_count, and each underlying fuses its expiration and strike
    // walks). Measured perf is neutral vs the old multi-pass path even here
    // (leaf `order_count()` is called once per leaf either way and dominates);
    // the change's value is the coherent single-walk snapshot under concurrency,
    // not throughput. This case documents the non-regression.
    group.bench_function("stats_high_fanout", |b| {
        let manager = UnderlyingOrderBookManager::new();
        for u in 0..20u64 {
            let underlying = manager.get_or_create(format!("SYM{u}"));
            for days in [7, 14, 21, 28, 35, 42, 49, 56, 63, 70, 77, 84] {
                let exp = ExpirationDate::Days(pos_or_panic!(days as f64));
                let exp_book = underlying.get_or_create_expiration(exp);
                for strike in [40000u64, 50000, 60000] {
                    let s = exp_book.get_or_create_strike(strike);
                    s.call()
                        .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                        .unwrap();
                }
            }
        }
        b.iter(|| manager.stats());
    });

    group.finish();
}

/// Benchmarks for underlying manager scaling.
pub fn underlying_manager_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("underlying_manager_scaling");

    for num_underlyings in [2, 5, 10, 20].iter() {
        group.throughput(Throughput::Elements(*num_underlyings as u64));

        group.bench_with_input(
            BenchmarkId::new("create_underlyings", num_underlyings),
            num_underlyings,
            |b, &num_underlyings| {
                b.iter_batched(
                    UnderlyingOrderBookManager::new,
                    |manager| {
                        for i in 0..num_underlyings {
                            manager.get_or_create(format!("SYM{}", i));
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("stats_with_n_underlyings", num_underlyings),
            num_underlyings,
            |b, &num_underlyings| {
                let manager = UnderlyingOrderBookManager::new();
                for i in 0..num_underlyings {
                    let underlying = manager.get_or_create(format!("SYM{}", i));
                    let exp = test_expiration();
                    let exp_book = underlying.get_or_create_expiration(exp);
                    exp_book.get_or_create_strike(50000);
                }
                b.iter(|| manager.stats());
            },
        );
    }

    group.finish();
}
