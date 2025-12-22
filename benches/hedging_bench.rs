//! Benchmarks for delta hedging operations.

use criterion::Criterion;
use option_chain_orderbook::hedging::{DeltaHedger, HedgeParams};
use option_chain_orderbook::pricing::Greeks;
use rust_decimal_macros::dec;

/// Benchmarks for delta hedging operations.
pub fn hedging_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("hedging_operations");

    // Benchmark DeltaHedger creation
    group.bench_function("hedger_new", |b| {
        let params = HedgeParams::default();
        b.iter(|| DeltaHedger::new(params));
    });

    // Benchmark update_delta
    group.bench_function("update_delta", |b| {
        let params = HedgeParams::default();
        let mut hedger = DeltaHedger::new(params);
        let greeks = Greeks::new(dec!(150), dec!(5), dec!(-20), dec!(100), dec!(10));
        b.iter(|| hedger.update_delta(&greeks));
    });

    // Benchmark needs_hedge check
    group.bench_function("needs_hedge", |b| {
        let params = HedgeParams::default();
        let mut hedger = DeltaHedger::new(params);
        let greeks = Greeks::new(dec!(150), dec!(5), dec!(-20), dec!(100), dec!(10));
        hedger.update_delta(&greeks);
        b.iter(|| hedger.needs_hedge());
    });

    // Benchmark calculate_hedge (no hedge needed)
    group.bench_function("calculate_hedge_no_action", |b| {
        let params = HedgeParams::default();
        let mut hedger = DeltaHedger::new(params);
        let greeks = Greeks::new(dec!(5), dec!(0.5), dec!(-2), dec!(10), dec!(1));
        hedger.update_delta(&greeks);
        b.iter(|| hedger.calculate_hedge("BTC", dec!(50000), 1234567890));
    });

    // Benchmark calculate_hedge (hedge needed)
    group.bench_function("calculate_hedge_with_action", |b| {
        let params = HedgeParams::default();
        let mut hedger = DeltaHedger::new(params);
        let greeks = Greeks::new(dec!(150), dec!(5), dec!(-20), dec!(100), dec!(10));
        hedger.update_delta(&greeks);
        b.iter(|| hedger.calculate_hedge("BTC", dec!(50000), 1234567890));
    });

    // Benchmark delta_deviation
    group.bench_function("delta_deviation", |b| {
        let params = HedgeParams::default();
        let mut hedger = DeltaHedger::new(params);
        let greeks = Greeks::new(dec!(150), dec!(5), dec!(-20), dec!(100), dec!(10));
        hedger.update_delta(&greeks);
        b.iter(|| hedger.delta_deviation());
    });

    group.finish();
}
