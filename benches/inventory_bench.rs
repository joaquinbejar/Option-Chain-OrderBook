//! Benchmarks for inventory management.

use criterion::{BenchmarkId, Criterion, Throughput};
use option_chain_orderbook::inventory::{InventoryManager, Position, PositionLimits};
use option_chain_orderbook::pricing::Greeks;
use rust_decimal_macros::dec;

/// Benchmarks for position operations.
pub fn position_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("position_operations");

    // Benchmark Position creation
    group.bench_function("position_new", |b| {
        b.iter(|| Position::new(dec!(100)));
    });

    // Benchmark Position with entry
    group.bench_function("position_with_entry", |b| {
        b.iter(|| Position::with_entry(dec!(10), dec!(5.50), dec!(100), 1234567890));
    });

    // Benchmark unrealized P&L calculation
    group.bench_function("unrealized_pnl", |b| {
        let position = Position::with_entry(dec!(10), dec!(5.50), dec!(100), 1234567890);
        b.iter(|| position.unrealized_pnl(dec!(6.00)));
    });

    // Benchmark total_pnl calculation
    group.bench_function("total_pnl", |b| {
        let position = Position::with_entry(dec!(10), dec!(5.50), dec!(100), 1234567890);
        b.iter(|| position.total_pnl(dec!(6.00)));
    });

    group.finish();
}

/// Benchmarks for inventory manager operations.
pub fn inventory_manager_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("inventory_manager");

    let limits = PositionLimits::small();

    // Benchmark manager creation
    group.bench_function("new", |b| {
        b.iter(|| InventoryManager::new("BTC", limits, dec!(1)));
    });

    // Benchmark get_or_create_position
    group.bench_function("get_or_create_position", |b| {
        let mut manager = InventoryManager::new("BTC", limits, dec!(1));
        b.iter(|| {
            manager.get_or_create_position("BTC-20240329-50000-C");
        });
    });

    // Benchmark total_greeks calculation
    group.bench_function("total_greeks", |b| {
        let mut manager = InventoryManager::new("BTC", limits, dec!(1));
        for i in 0..50 {
            let pos =
                manager.get_or_create_position(format!("BTC-20240329-{}-C", 40000 + i * 1000));
            pos.update_greeks(
                Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01)),
                1234567890,
            );
        }
        b.iter(|| manager.total_greeks());
    });

    group.finish();
}

/// Benchmarks for inventory manager scaling.
pub fn inventory_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("inventory_scaling");
    let limits = PositionLimits::small();

    for num_positions in [10, 100, 500].iter() {
        group.throughput(Throughput::Elements(*num_positions as u64));

        group.bench_with_input(
            BenchmarkId::new("total_greeks", num_positions),
            num_positions,
            |b, &num_positions| {
                let mut manager = InventoryManager::new("BTC", limits, dec!(1));
                for i in 0..num_positions {
                    let pos = manager
                        .get_or_create_position(format!("BTC-20240329-{}-C", 40000 + i * 100));
                    pos.update_greeks(
                        Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01)),
                        1234567890,
                    );
                }
                b.iter(|| manager.total_greeks());
            },
        );
    }

    group.finish();
}
