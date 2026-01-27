//! Benchmarks for order book operations.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::orderbook::OptionOrderBook;
use optionstratlib::OptionStyle;
use orderbook_rs::{OrderId, Side};

/// Benchmarks for single order book operations.
pub fn orderbook_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_operations");

    // Benchmark adding limit orders
    group.bench_function("add_limit_order", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        b.iter(|| {
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
        });
    });

    // Benchmark getting best quote from empty book
    group.bench_function("best_quote_empty", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        b.iter(|| book.best_quote());
    });

    // Benchmark getting best quote from populated book
    group.bench_function("best_quote_populated", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        for i in 0..100 {
            book.add_limit_order(OrderId::new(), Side::Buy, 100 - i, 10)
                .unwrap();
            book.add_limit_order(OrderId::new(), Side::Sell, 101 + i, 10)
                .unwrap();
        }
        b.iter(|| book.best_quote());
    });

    // Benchmark cancel order
    group.bench_function("cancel_order", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        b.iter_batched(
            || {
                let id = OrderId::new();
                book.add_limit_order(id, Side::Buy, 100, 10).unwrap();
                id
            },
            |id| book.cancel_order(id),
            criterion::BatchSize::SmallInput,
        );
    });

    // Benchmark update_last_quote
    group.bench_function("update_last_quote", |b| {
        let mut book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();
        b.iter(|| book.update_last_quote());
    });

    // Benchmark snapshot creation
    group.bench_function("snapshot", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
        for i in 0..50 {
            book.add_limit_order(OrderId::new(), Side::Buy, 100 - i, 10)
                .unwrap();
            book.add_limit_order(OrderId::new(), Side::Sell, 101 + i, 10)
                .unwrap();
        }
        b.iter(|| book.snapshot(10));
    });

    group.finish();
}

/// Benchmarks for order book with varying depths.
pub fn orderbook_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_scaling");

    for depth in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*depth as u64));

        group.bench_with_input(BenchmarkId::new("add_orders", depth), depth, |b, &depth| {
            b.iter_batched(
                || OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call),
                |book| {
                    for i in 0..depth {
                        book.add_limit_order(OrderId::new(), Side::Buy, (1000 - i) as u128, 10)
                            .unwrap();
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });

        group.bench_with_input(
            BenchmarkId::new("best_quote_depth", depth),
            depth,
            |b, &depth| {
                let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);
                for i in 0..depth {
                    book.add_limit_order(OrderId::new(), Side::Buy, (1000 - i) as u128, 10)
                        .unwrap();
                    book.add_limit_order(OrderId::new(), Side::Sell, (1001 + i) as u128, 10)
                        .unwrap();
                }
                b.iter(|| book.best_quote());
            },
        );
    }

    group.finish();
}
