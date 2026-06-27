//! Property: `get_or_create_*` is idempotent.
//!
//! Invariant asserted: a second `get_or_create` (underlying / expiration /
//! strike) with the *same key* returns the **same** `Arc` (`Arc::ptr_eq`), never
//! a freshly allocated book — both for an immediate repeat call and for a later
//! collision on the same key anywhere in the generated stream.
//!
//! The strategy draws keys from the small fixed pools in
//! [`crate::common::strategies`], so collisions are frequent on short streams.

use crate::common::strategies::{EXP_POOL_LEN, STRIKES, UNDERLYINGS, nth_expiration};
use option_chain_orderbook::orderbook::{
    ExpirationOrderBook, StrikeOrderBook, UnderlyingOrderBook, UnderlyingOrderBookManager,
};
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    /// For every generated `(underlying, expiration, strike)` key:
    /// an immediate second `get_or_create_*` returns the same `Arc`, and any
    /// later access of a previously seen key returns that same `Arc` too.
    #[test]
    fn get_or_create_returns_same_handle(
        keys in prop::collection::vec(
            (0usize..UNDERLYINGS.len(), 0usize..EXP_POOL_LEN, 0usize..STRIKES.len()),
            1..64,
        ),
    ) {
        let manager = UnderlyingOrderBookManager::new();

        // First-seen handle per canonical key — used to assert later collisions
        // resolve to the identical allocation.
        let mut seen_u: HashMap<usize, Arc<UnderlyingOrderBook>> = HashMap::new();
        let mut seen_e: HashMap<(usize, usize), Arc<ExpirationOrderBook>> = HashMap::new();
        let mut seen_s: HashMap<(usize, usize, usize), Arc<StrikeOrderBook>> = HashMap::new();

        for (ui, ei, si) in keys {
            let u_sym = UNDERLYINGS[ui];
            let exp = nth_expiration(ei);
            let strike = STRIKES[si];

            // ── Underlying level ────────────────────────────────────────────
            let first_u = manager.get_or_create(u_sym);
            let again_u = manager.get_or_create(u_sym);
            prop_assert!(
                Arc::ptr_eq(&first_u, &again_u),
                "second get_or_create(underlying) must return the same Arc"
            );
            if let Some(prev) = seen_u.get(&ui) {
                prop_assert!(
                    Arc::ptr_eq(prev, &first_u),
                    "a repeat underlying key must resolve to the originally created Arc"
                );
            } else {
                seen_u.insert(ui, Arc::clone(&first_u));
            }

            // ── Expiration level ────────────────────────────────────────────
            let first_e = first_u.get_or_create_expiration(exp);
            let again_e = first_u.get_or_create_expiration(exp);
            prop_assert!(
                Arc::ptr_eq(&first_e, &again_e),
                "second get_or_create_expiration must return the same Arc"
            );
            if let Some(prev) = seen_e.get(&(ui, ei)) {
                prop_assert!(
                    Arc::ptr_eq(prev, &first_e),
                    "a repeat expiration key must resolve to the originally created Arc"
                );
            } else {
                seen_e.insert((ui, ei), Arc::clone(&first_e));
            }

            // ── Strike level ────────────────────────────────────────────────
            let first_s = first_e.get_or_create_strike(strike);
            let again_s = first_e.get_or_create_strike(strike);
            prop_assert!(
                Arc::ptr_eq(&first_s, &again_s),
                "second get_or_create_strike must return the same Arc"
            );
            if let Some(prev) = seen_s.get(&(ui, ei, si)) {
                prop_assert!(
                    Arc::ptr_eq(prev, &first_s),
                    "a repeat strike key must resolve to the originally created Arc"
                );
            } else {
                seen_s.insert((ui, ei, si), Arc::clone(&first_s));
            }
        }

        // The manager must not have created more than the distinct keys seen.
        prop_assert_eq!(manager.len(), seen_u.len());
    }
}
