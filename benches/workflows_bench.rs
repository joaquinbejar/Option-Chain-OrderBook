//! Benchmarks for realistic combined workflows.

use criterion::Criterion;
use option_chain_orderbook::hedging::{DeltaHedger, HedgeParams};
use option_chain_orderbook::inventory::{InventoryManager, PositionLimits};
use option_chain_orderbook::orderbook::OptionOrderBook;
use option_chain_orderbook::pricing::Greeks;
use option_chain_orderbook::quoting::{QuoteParams, SpreadCalculator};
use option_chain_orderbook::risk::{RiskController, RiskLimits};
use orderbook_rs::{OrderId, Side};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;

/// Benchmarks for realistic combined workflows.
pub fn combined_workflows(c: &mut Criterion) {
    let mut group = c.benchmark_group("combined_workflows");

    // Benchmark: Full quote cycle (spread calc + order placement)
    group.bench_function("quote_cycle", |b| {
        let book = OptionOrderBook::new("BTC-20240329-50000-C");
        let calc = SpreadCalculator::new();
        let params = QuoteParams::new(dec!(5.50), dec!(0), dec!(0.30), dec!(0.25));

        b.iter(|| {
            let quote = calc.generate_quote(&params, 1234567890);
            let bid = (quote.bid_price() * dec!(100)).to_u64().unwrap_or(100);
            let ask = (quote.ask_price() * dec!(100)).to_u64().unwrap_or(101);
            book.add_limit_order(OrderId::new(), Side::Buy, bid, 10)
                .unwrap();
            book.add_limit_order(OrderId::new(), Side::Sell, ask, 10)
                .unwrap();
        });
    });

    // Benchmark: Risk check cycle
    group.bench_function("risk_check_cycle", |b| {
        let limits = RiskLimits::default();
        let mut controller = RiskController::new(limits);
        let greeks = Greeks::new(dec!(100), dec!(5), dec!(-20), dec!(100), dec!(10));

        b.iter(|| {
            controller.update_pnl(dec!(500));
            let _breaches = controller.check_greek_limits(&greeks);
            controller.is_halted()
        });
    });

    // Benchmark: Inventory + Hedging cycle
    group.bench_function("inventory_hedge_cycle", |b| {
        let limits = PositionLimits::small();
        let mut manager = InventoryManager::new("BTC", limits, dec!(1));
        let hedge_params = HedgeParams::default();
        let mut hedger = DeltaHedger::new(hedge_params);

        // Setup some positions
        for i in 0..10 {
            let pos =
                manager.get_or_create_position(format!("BTC-20240329-{}-C", 40000 + i * 1000));
            pos.update_greeks(
                Greeks::new(dec!(0.5), dec!(0.05), dec!(-0.02), dec!(0.15), dec!(0.01)),
                1234567890,
            );
        }

        b.iter(|| {
            let total_greeks = manager.total_greeks();
            hedger.update_delta(&total_greeks);
            hedger.calculate_hedge("BTC", dec!(50000), 1234567890)
        });
    });

    group.finish();
}
