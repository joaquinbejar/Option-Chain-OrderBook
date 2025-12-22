//! Benchmarks for spread calculation.

use criterion::{BenchmarkId, Criterion};
use option_chain_orderbook::quoting::{QuoteParams, SpreadCalculator};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Benchmarks for spread calculation.
pub fn spread_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("spread_calculation");

    // Benchmark SpreadCalculator creation
    group.bench_function("calculator_new", |b| {
        b.iter(SpreadCalculator::new);
    });

    // Benchmark optimal spread calculation
    group.bench_function("optimal_spread", |b| {
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(0), dec!(0.30), dec!(0.25));
        b.iter(|| calc.optimal_spread(&params));
    });

    // Benchmark inventory skew calculation
    group.bench_function("inventory_skew", |b| {
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(100), dec!(0.30), dec!(0.25));
        b.iter(|| calc.inventory_skew(&params));
    });

    // Benchmark full quote generation
    group.bench_function("generate_quote", |b| {
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(50), dec!(0.30), dec!(0.25));
        b.iter(|| calc.generate_quote(&params, 1234567890));
    });

    // Benchmark with different volatilities
    for vol in [10, 30, 80].iter() {
        let vol_decimal = Decimal::from(*vol) / dec!(100);
        group.bench_with_input(
            BenchmarkId::new("optimal_spread_vol", vol),
            &vol_decimal,
            |b, vol| {
                let calc = SpreadCalculator::new().with_max_spread(dec!(1.0));
                let params = QuoteParams::new(dec!(5.50), dec!(0), *vol, dec!(0.25));
                b.iter(|| calc.optimal_spread(&params));
            },
        );
    }

    group.finish();
}
