//! Benchmarks for order book manager operations.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::orderbook::OptionOrderBookManager;
use orderbook_rs::{OrderId, Side};

/// Benchmarks for order book manager operations.
pub fn manager_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("manager_operations");

    // Benchmark get_or_create for new symbol
    group.bench_function("get_or_create_new", |b| {
        b.iter_batched(
            OptionOrderBookManager::new,
            |mut manager| {
                manager.get_or_create("BTC-20240329-50000-C");
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Benchmark get_or_create for existing symbol
    group.bench_function("get_or_create_existing", |b| {
        let mut manager = OptionOrderBookManager::new();
        manager.get_or_create("BTC-20240329-50000-C");
        b.iter(|| {
            manager.get_or_create("BTC-20240329-50000-C");
        });
    });

    // Benchmark get by symbol
    group.bench_function("get_by_symbol", |b| {
        let mut manager = OptionOrderBookManager::new();
        for i in 0..100 {
            manager.get_or_create(format!("BTC-20240329-{}-C", 40000 + i * 1000));
        }
        b.iter(|| manager.get("BTC-20240329-50000-C"));
    });

    // Benchmark stats calculation
    group.bench_function("stats", |b| {
        let mut manager = OptionOrderBookManager::new();
        for i in 0..50 {
            let book = manager.get_or_create(format!("BTC-20240329-{}-C", 40000 + i * 1000));
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
            book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
                .unwrap();
        }
        b.iter(|| manager.stats());
    });

    // Benchmark all_quotes
    group.bench_function("all_quotes", |b| {
        let mut manager = OptionOrderBookManager::new();
        for i in 0..50 {
            let book = manager.get_or_create(format!("BTC-20240329-{}-C", 40000 + i * 1000));
            book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                .unwrap();
            book.add_limit_order(OrderId::new(), Side::Sell, 101, 5)
                .unwrap();
        }
        b.iter(|| manager.all_quotes());
    });

    group.finish();
}

/// Benchmarks for manager scaling with number of books.
pub fn manager_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("manager_scaling");

    for num_books in [10, 100, 500].iter() {
        group.throughput(Throughput::Elements(*num_books as u64));

        group.bench_with_input(
            BenchmarkId::new("create_books", num_books),
            num_books,
            |b, &num_books| {
                b.iter_batched(
                    || OptionOrderBookManager::with_capacity(num_books),
                    |mut manager| {
                        for i in 0..num_books {
                            manager.get_or_create(format!("BTC-20240329-{}-C", 40000 + i * 100));
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("stats_with_books", num_books),
            num_books,
            |b, &num_books| {
                let mut manager = OptionOrderBookManager::with_capacity(num_books);
                for i in 0..num_books {
                    let book = manager.get_or_create(format!("BTC-20240329-{}-C", 40000 + i * 100));
                    book.add_limit_order(OrderId::new(), Side::Buy, 100, 10)
                        .unwrap();
                }
                b.iter(|| manager.stats());
            },
        );
    }

    group.finish();
}
