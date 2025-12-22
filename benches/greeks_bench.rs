//! Benchmarks for Greeks operations.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::pricing::Greeks;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Benchmarks for Greeks operations.
pub fn greeks_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("greeks_operations");

    // Benchmark Greeks creation
    group.bench_function("new", |b| {
        b.iter(|| Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01)));
    });

    // Benchmark Greeks addition
    group.bench_function("add", |b| {
        let g1 = Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01));
        let g2 = Greeks::new(dec!(0.3), dec!(0.03), dec!(-0.01), dec!(0.10), dec!(0.005));
        b.iter(|| g1 + g2);
    });

    // Benchmark Greeks scaling
    group.bench_function("scale", |b| {
        let greeks = Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01));
        b.iter(|| greeks.scale(dec!(100)));
    });

    // Benchmark Greeks multiplication
    group.bench_function("multiply", |b| {
        let greeks = Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01));
        b.iter(|| greeks * dec!(10));
    });

    // Benchmark is_zero check
    group.bench_function("is_zero", |b| {
        let greeks = Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01));
        b.iter(|| greeks.is_zero());
    });

    // Benchmark dollar_delta calculation
    group.bench_function("dollar_delta", |b| {
        let greeks = Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01));
        b.iter(|| greeks.dollar_delta(dec!(50000), dec!(100)));
    });

    group.finish();
}

/// Benchmarks for Greeks aggregation.
pub fn greeks_aggregation(c: &mut Criterion) {
    let mut group = c.benchmark_group("greeks_aggregation");

    for count in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*count as u64));

        group.bench_with_input(BenchmarkId::new("sum_greeks", count), count, |b, &count| {
            let greeks_vec: Vec<Greeks> = (0..count)
                .map(|i| {
                    Greeks::new(
                        Decimal::from(i) / dec!(1000),
                        dec!(0.05),
                        dec!(-0.02),
                        dec!(0.15),
                        dec!(0.01),
                    )
                })
                .collect();
            b.iter(|| greeks_vec.iter().fold(Greeks::zero(), |acc, g| acc + *g));
        });
    }

    group.finish();
}
