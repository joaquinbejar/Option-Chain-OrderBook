//! Benchmarks for option-chain-orderbook library.
//!
//! This module provides comprehensive benchmarks for all major components:
//! - Order book operations (add, cancel, query)
//! - Order book manager operations
//! - Greeks calculations and aggregation
//! - Spread calculation (Avellaneda-Stoikov model)
//! - Inventory management
//! - Delta hedging calculations
//! - Risk controller checks

mod greeks_bench;
mod hedging_bench;
mod inventory_bench;
mod manager_bench;
mod orderbook_bench;
mod risk_bench;
mod spread_bench;
mod workflows_bench;

use criterion::{criterion_group, criterion_main};

criterion_group!(
    orderbook_benches,
    orderbook_bench::orderbook_operations,
    orderbook_bench::orderbook_scaling,
);

criterion_group!(
    manager_benches,
    manager_bench::manager_operations,
    manager_bench::manager_scaling,
);

criterion_group!(
    greeks_benches,
    greeks_bench::greeks_operations,
    greeks_bench::greeks_aggregation,
);

criterion_group!(quoting_benches, spread_bench::spread_calculation,);

criterion_group!(
    inventory_benches,
    inventory_bench::position_operations,
    inventory_bench::inventory_manager_operations,
    inventory_bench::inventory_scaling,
);

criterion_group!(hedging_benches, hedging_bench::hedging_operations,);

criterion_group!(risk_benches, risk_bench::risk_controller_operations,);

criterion_group!(workflow_benches, workflows_bench::combined_workflows,);

criterion_main!(
    orderbook_benches,
    manager_benches,
    greeks_benches,
    quoting_benches,
    inventory_benches,
    hedging_benches,
    risk_benches,
    workflow_benches,
);
