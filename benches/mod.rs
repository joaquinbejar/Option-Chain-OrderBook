//! Benchmarks for option-chain-orderbook library.

use criterion::{Criterion, criterion_group, criterion_main};
use option_chain_orderbook::orderbook::OptionOrderBook;
use orderbook_rs::{OrderId, Side};

fn orderbook_benchmark(c: &mut Criterion) {
    c.bench_function("add_limit_order", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");
        b.iter(|| {
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
        });
    });

    c.bench_function("best_quote", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");
        book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap();
        book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
            .unwrap();
        b.iter(|| book.best_quote());
    });
}

criterion_group!(benches, orderbook_benchmark);
criterion_main!(benches);
