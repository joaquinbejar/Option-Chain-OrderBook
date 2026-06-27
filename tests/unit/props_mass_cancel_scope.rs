//! Property: a scoped `cancel_all` cancels exactly the resting orders in scope.
//!
//! Invariant asserted, for strike / chain / expiration scopes after a generated
//! op stream:
//!
//! * the returned `*MassCancelResult.total_cancelled()` equals the count of
//!   resting orders in that scope *before* the cancel;
//! * the in-scope book is empty afterwards; and
//! * every out-of-scope book is left **untouched**.
//!
//! Each proptest seeds a structurally-separate, buy-only **witness** book (a
//! sibling strike outside the stream's strike pool / a different expiration's
//! chain / a second expiration). Buy-only ⇒ nothing crosses ⇒ all witness
//! orders rest, guaranteeing a non-empty out-of-scope book in *every* case; the
//! submit-heavy stream keeps the in-scope book non-empty most of the time. The
//! concrete `*_concrete` test pins exact in- and out-of-scope counts.

use crate::common::strategies::{
    STRIKES, apply_op, chain_op_stream, chain_op_strike, nth_expiration,
};
use option_chain_orderbook::orderbook::{
    ExpirationOrderBook, OptionChainOrderBook, StrikeOrderBook,
};
use option_chain_orderbook::{OrderId, Side};
use proptest::prelude::*;
use std::sync::Arc;

/// Strike price reserved as an out-of-scope witness: deliberately NOT in
/// [`STRIKES`], so the chain-routed stream never references it.
const WITNESS_STRIKE: u64 = 900;

/// Seeds `count` buy-only resting orders into a strike's call leg. Buy-only ⇒
/// no crossing ⇒ all rest, so the strike ends with exactly `count` orders.
fn seed_buy_only(strike: &Arc<StrikeOrderBook>, count: u64) {
    for k in 0..count {
        strike
            .call()
            .add_limit_order(
                OrderId::from_u64(1_000 + k),
                Side::Buy,
                50 + u128::from(k),
                1,
            )
            .expect("buy-only seed never crosses and must rest");
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    /// Strike-scope cancel: cancels exactly one strike, siblings untouched.
    #[test]
    fn strike_scope_cancel_counts(
        stream in chain_op_stream(1..120),
        witness_count in 1u64..=5,
    ) {
        let chain = OptionChainOrderBook::new("BTC", nth_expiration(0));

        // Out-of-scope witness (separate strike book, never in the stream pool).
        let witness = chain.get_or_create_strike(WITNESS_STRIKE);
        seed_buy_only(&witness, witness_count);
        let witness_before = witness.order_count();

        for routed in &stream {
            let strike = chain.get_or_create_strike(chain_op_strike(routed));
            apply_op(strike.get(routed.style), &routed.op);
        }

        // Scope = the first pool strike; the others are out-of-scope siblings.
        let target = chain.get_or_create_strike(STRIKES[0]);
        let sibling_a = chain.get_or_create_strike(STRIKES[1]);
        let sibling_b = chain.get_or_create_strike(STRIKES[2]);

        let target_before = target.order_count();
        let sibling_a_before = sibling_a.order_count();
        let sibling_b_before = sibling_b.order_count();

        let result = target.cancel_all().expect("strike cancel_all");

        prop_assert_eq!(result.total_cancelled(), target_before);
        prop_assert_eq!(target.order_count(), 0);
        // Out-of-scope books untouched.
        prop_assert_eq!(sibling_a.order_count(), sibling_a_before);
        prop_assert_eq!(sibling_b.order_count(), sibling_b_before);
        prop_assert_eq!(witness.order_count(), witness_before);
    }

    /// Chain-scope cancel: cancels the whole chain, a sibling chain untouched.
    #[test]
    fn chain_scope_cancel_counts(
        stream in chain_op_stream(1..120),
        witness_count in 1u64..=5,
    ) {
        let chain_a = OptionChainOrderBook::new("BTC", nth_expiration(0));
        // Out-of-scope witness: a different expiration's chain (separate books).
        let chain_b = OptionChainOrderBook::new("BTC", nth_expiration(1));
        seed_buy_only(&chain_b.get_or_create_strike(STRIKES[0]), witness_count);
        let witness_before = chain_b.total_order_count();

        for routed in &stream {
            let strike = chain_a.get_or_create_strike(chain_op_strike(routed));
            apply_op(strike.get(routed.style), &routed.op);
        }

        let before = chain_a.total_order_count();
        let result = chain_a.cancel_all().expect("chain cancel_all");

        prop_assert_eq!(result.total_cancelled(), before);
        prop_assert_eq!(chain_a.total_order_count(), 0);
        prop_assert_eq!(chain_b.total_order_count(), witness_before);
    }

    /// Expiration-scope cancel: cancels one expiration, another untouched.
    #[test]
    fn expiration_scope_cancel_counts(
        stream in chain_op_stream(1..120),
        witness_count in 1u64..=5,
    ) {
        let exp_a = ExpirationOrderBook::new("BTC", nth_expiration(0));
        let exp_b = ExpirationOrderBook::new("BTC", nth_expiration(1));
        seed_buy_only(&exp_b.get_or_create_strike(STRIKES[0]), witness_count);
        let witness_before = exp_b.total_order_count();

        for routed in &stream {
            let strike = exp_a.get_or_create_strike(chain_op_strike(routed));
            apply_op(strike.get(routed.style), &routed.op);
        }

        let before = exp_a.total_order_count();
        let result = exp_a.cancel_all().expect("expiration cancel_all");

        prop_assert_eq!(result.total_cancelled(), before);
        prop_assert_eq!(exp_a.total_order_count(), 0);
        prop_assert_eq!(exp_b.total_order_count(), witness_before);
    }
}

#[test]
fn strike_scope_cancel_counts_concrete() {
    let chain = OptionChainOrderBook::new("BTC", nth_expiration(0));

    // In-scope strike 100: two resting call orders (non-crossing) + one put.
    let target = chain.get_or_create_strike(100);
    target
        .call()
        .add_limit_order(OrderId::from_u64(1), Side::Buy, 99, 5)
        .expect("add buy");
    target
        .call()
        .add_limit_order(OrderId::from_u64(2), Side::Sell, 101, 3)
        .expect("add sell");
    target
        .put()
        .add_limit_order(OrderId::from_u64(3), Side::Buy, 50, 1)
        .expect("add put buy");
    assert_eq!(target.order_count(), 3);

    // Out-of-scope strike 200: one resting order.
    let sibling = chain.get_or_create_strike(200);
    sibling
        .call()
        .add_limit_order(OrderId::from_u64(4), Side::Buy, 10, 1)
        .expect("add sibling buy");
    assert_eq!(sibling.order_count(), 1);

    let result = chain
        .get_or_create_strike(100)
        .cancel_all()
        .expect("strike cancel_all");

    // Exactly the three in-scope orders cancelled; sibling untouched.
    assert_eq!(result.total_cancelled(), 3);
    assert_eq!(target.order_count(), 0);
    assert_eq!(sibling.order_count(), 1);
}
