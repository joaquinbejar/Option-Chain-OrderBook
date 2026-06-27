//! Property: aggregated Greeks equal the hand-computed `Σ (quantity × greek)`.
//!
//! Invariant asserted: for a set of added [`Position`]s under one underlying,
//! every field of `GreeksAggregator::aggregate_by_underlying` equals the
//! hand-computed `Σ (quantity × greek)` **exactly** in [`Decimal`] (no
//! tolerance). The bounded strategy keeps every product and sum well within
//! `Decimal` range, so the result is exact and the `saturated` flag stays
//! `false`. A separate deterministic test forces an overflow and asserts the
//! `saturated` flag is set (the skill's saturation hint).
//!
//! Aggregation iterates an unordered `DashMap` / `HashMap`, but exact `Decimal`
//! addition is associative and commutative, so the sum is order-independent —
//! the equality is not flaky.

use option_chain_orderbook::orderbook::{GreeksAggregator, Position};
use optionstratlib::greeks::Greek;
use proptest::prelude::*;
use rust_decimal::Decimal;

/// Builds a per-instrument `Greek` from 12 raw integers, each scaled by 1e-2 so
/// the fields carry two decimal places (exercising non-integer `Decimal`s while
/// staying exact).
fn greek_from(raws: &[i64; 12]) -> Greek {
    let s = |i: usize| Decimal::new(raws[i], 2);
    Greek {
        delta: s(0),
        gamma: s(1),
        theta: s(2),
        vega: s(3),
        rho: s(4),
        rho_d: s(5),
        alpha: s(6),
        vanna: s(7),
        vomma: s(8),
        veta: s(9),
        charm: s(10),
        color: s(11),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    /// Aggregated Greeks for `"BTC"` equal `Σ (quantity × greek)`, exact.
    #[test]
    fn aggregate_by_underlying_equals_manual_sum(
        positions in prop::collection::vec(
            (-1_000i64..=1_000, proptest::array::uniform12(-50i64..=50)),
            0..16,
        ),
    ) {
        let agg = GreeksAggregator::new();

        // Hand-computed expected sums, one Decimal per Greek field.
        let mut expected = [Decimal::ZERO; 12];
        for (idx, (qty, raws)) in positions.iter().enumerate() {
            // Unique instrument symbol per position so none replaces another.
            let symbol = format!("BTC-20260130-{}-C", idx + 1);
            agg.add_position("acct", Position::new(symbol, "BTC", *qty, greek_from(raws)));

            let q = Decimal::from(*qty);
            for (field, raw) in expected.iter_mut().zip(raws.iter()) {
                *field += q * Decimal::new(*raw, 2);
            }
        }

        let result = agg.aggregate_by_underlying("BTC");

        prop_assert_eq!(result.delta, expected[0]);
        prop_assert_eq!(result.gamma, expected[1]);
        prop_assert_eq!(result.theta, expected[2]);
        prop_assert_eq!(result.vega, expected[3]);
        prop_assert_eq!(result.rho, expected[4]);
        prop_assert_eq!(result.rho_d, expected[5]);
        prop_assert_eq!(result.alpha, expected[6]);
        prop_assert_eq!(result.vanna, expected[7]);
        prop_assert_eq!(result.vomma, expected[8]);
        prop_assert_eq!(result.veta, expected[9]);
        prop_assert_eq!(result.charm, expected[10]);
        prop_assert_eq!(result.color, expected[11]);

        prop_assert_eq!(result.position_count, positions.len());
        // Bounded inputs never saturate.
        prop_assert!(!result.saturated);
    }
}

#[test]
fn saturating_quantity_sets_the_flag() {
    let agg = GreeksAggregator::new();
    // delta = Decimal::MAX, quantity = 2  ⇒  MAX * 2 overflows ⇒ saturates.
    let greeks = Greek {
        delta: Decimal::MAX,
        gamma: Decimal::ZERO,
        theta: Decimal::ZERO,
        vega: Decimal::ZERO,
        rho: Decimal::ZERO,
        rho_d: Decimal::ZERO,
        alpha: Decimal::ZERO,
        vanna: Decimal::ZERO,
        vomma: Decimal::ZERO,
        veta: Decimal::ZERO,
        charm: Decimal::ZERO,
        color: Decimal::ZERO,
    };
    agg.add_position("acct", Position::new("BTC-20260130-1-C", "BTC", 2, greeks));

    let result = agg.aggregate_by_underlying("BTC");
    assert!(
        result.saturated,
        "aggregation that overflows Decimal::MAX must set the saturated flag"
    );
    assert_eq!(result.delta, Decimal::MAX);
}
