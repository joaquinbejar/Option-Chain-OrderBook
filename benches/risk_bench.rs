//! Benchmarks for risk controller operations.

use criterion::Criterion;
use option_chain_orderbook::pricing::Greeks;
use option_chain_orderbook::risk::{RiskController, RiskLimits};
use rust_decimal_macros::dec;

/// Benchmarks for risk controller operations.
pub fn risk_controller_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("risk_controller");

    // Benchmark RiskController creation
    group.bench_function("new", |b| {
        let limits = RiskLimits::default();
        b.iter(|| RiskController::new(limits));
    });

    // Benchmark check_greek_limits (no breach)
    group.bench_function("check_greek_limits_ok", |b| {
        let limits = RiskLimits::default();
        let controller = RiskController::new(limits);
        let greeks = Greeks::new(dec!(50), dec!(2), dec!(-10), dec!(50), dec!(5));
        b.iter(|| controller.check_greek_limits(&greeks));
    });

    // Benchmark check_greek_limits (with breaches)
    group.bench_function("check_greek_limits_breach", |b| {
        let limits = RiskLimits::default();
        let controller = RiskController::new(limits);
        let greeks = Greeks::new(dec!(5000), dec!(500), dec!(-1000), dec!(5000), dec!(500));
        b.iter(|| controller.check_greek_limits(&greeks));
    });

    // Benchmark update_pnl
    group.bench_function("update_pnl", |b| {
        let limits = RiskLimits::default();
        let mut controller = RiskController::new(limits);
        b.iter(|| controller.update_pnl(dec!(1000)));
    });

    // Benchmark is_halted check
    group.bench_function("is_halted", |b| {
        let limits = RiskLimits::default();
        let controller = RiskController::new(limits);
        b.iter(|| controller.is_halted());
    });

    group.finish();
}
