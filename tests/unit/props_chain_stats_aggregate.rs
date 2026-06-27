//! Property: stats aggregate as the deterministic sum of their children.
//!
//! Invariants asserted:
//!
//! * `OptionChainStats` — `strike_count` equals the number of strikes created
//!   and `total_orders` equals `Σ strike.order_count()` over every child strike
//!   (the deterministic, ordered `SkipMap` walk).
//! * `GlobalStats` (the top manager) — `underlying_count`, `total_expirations`,
//!   `total_strikes`, and `total_orders` equal an independent walk of the public
//!   tree (underlyings → expirations → chains → strikes → leaf `order_count()`).
//!
//! Both are checked against a *separately computed* sum, not against the
//! single-pass `stats()` implementation, so the test genuinely cross-checks the
//! aggregation rather than restating it.

use crate::common::strategies::{
    EXP_POOL_LEN, STRIKES, UNDERLYINGS, apply_op, chain_op_stream, chain_op_strike, hier_op_stream,
    nth_expiration,
};
use option_chain_orderbook::orderbook::{OptionChainOrderBook, UnderlyingOrderBookManager};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    /// Chain stats equal the deterministic sum of the child strike stats.
    #[test]
    fn chain_stats_equal_child_sum(stream in chain_op_stream(1..160)) {
        let chain = OptionChainOrderBook::new("BTC", nth_expiration(0));
        for routed in &stream {
            let strike = chain.get_or_create_strike(chain_op_strike(routed));
            apply_op(strike.get(routed.style), &routed.op);
        }

        // Deterministic, ordered walk of the children.
        let strike_prices = chain.strike_prices();
        let mut expected_orders = 0usize;
        for price in &strike_prices {
            let strike = chain.get_strike(*price).expect("listed strike must exist");
            expected_orders += strike.order_count();
        }

        let stats = chain.stats();
        prop_assert_eq!(stats.strike_count, strike_prices.len());
        prop_assert_eq!(stats.total_orders, expected_orders);
    }

    /// Global stats equal an independent walk of the whole public tree.
    #[test]
    fn global_stats_equal_tree_walk(stream in hier_op_stream(1..160)) {
        let manager = UnderlyingOrderBookManager::new();
        for h in &stream {
            let underlying = manager.get_or_create(UNDERLYINGS[h.underlying_idx]);
            let expiration = underlying.get_or_create_expiration(nth_expiration(h.expiration_idx));
            let strike = expiration.get_or_create_strike(STRIKES[h.strike_idx]);
            apply_op(strike.get(h.style), &h.op);
        }

        let mut exp_underlyings = 0usize;
        let mut exp_expirations = 0usize;
        let mut exp_strikes = 0usize;
        let mut exp_orders = 0usize;

        for sym in manager.underlying_symbols() {
            let underlying = manager.get(sym.as_str()).expect("listed underlying must exist");
            exp_underlyings += 1;
            for ei in 0..EXP_POOL_LEN {
                let exp = nth_expiration(ei);
                if let Ok(expiration) = underlying.get_expiration(&exp) {
                    exp_expirations += 1;
                    let chain = expiration.chain();
                    for price in chain.strike_prices() {
                        exp_strikes += 1;
                        let strike = chain.get_strike(price).expect("listed strike must exist");
                        exp_orders += strike.order_count();
                    }
                }
            }
        }

        let stats = manager.stats();
        prop_assert_eq!(stats.underlying_count, exp_underlyings);
        prop_assert_eq!(stats.total_expirations, exp_expirations);
        prop_assert_eq!(stats.total_strikes, exp_strikes);
        prop_assert_eq!(stats.total_orders, exp_orders);
    }
}
