//! Integration tests for the orderbook module.

use option_chain_orderbook::orderbook::{
    ContractSpecs, OptionOrderBook, UnderlyingOrderBookManager, ValidationConfig,
};
use optionstratlib::prelude::pos_or_panic;
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::{OrderId, Side};
use pricelevel::Hash32;

#[test]
fn test_option_order_book_integration() {
    let book = OptionOrderBook::new("BTC-20240329-50000-C", OptionStyle::Call);

    // Add orders
    if let Err(err) = book.add_limit_order(OrderId::new(), Side::Buy, 100, 10) {
        panic!("add order failed: {}", err);
    }
    if let Err(err) = book.add_limit_order(OrderId::new(), Side::Sell, 101, 5) {
        panic!("add order failed: {}", err);
    }

    // Verify state
    assert_eq!(book.order_count(), 2);
    assert!(book.best_quote().is_two_sided());
}

#[test]
fn test_underlying_manager_integration() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

    // Create BTC option chain
    {
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(exp_date);
        let strike = exp.get_or_create_strike(50000);

        // Add orders to call and put
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .unwrap_or_else(|err| panic!("add order failed: {}", err));
        strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 50, 5)
            .unwrap_or_else(|err| panic!("add order failed: {}", err));
    }

    // Verify aggregation
    let stats = manager.stats();
    assert_eq!(stats.underlying_count, 1);
    assert_eq!(stats.total_expirations, 1);
    assert_eq!(stats.total_strikes, 1);
    assert_eq!(stats.total_orders, 2);
}

#[test]
fn test_cancel_all_across_underlyings() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

    {
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(exp_date);
        let strike = exp.get_or_create_strike(50000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 80, 5)
        {
            panic!("add order failed: {}", err);
        }
    }

    {
        let eth = manager.get_or_create("ETH");
        let exp = eth.get_or_create_expiration(exp_date);
        let strike = exp.get_or_create_strike(3000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 50, 7)
        {
            panic!("add order failed: {}", err);
        }
    }

    let result = match manager.cancel_all_across_underlyings() {
        Ok(result) => result,
        Err(err) => panic!("cancel failed: {}", err),
    };

    assert_eq!(result.total_cancelled(), 3);
    // `books_affected` reports leaf option books (call/put contract books):
    // BTC touched both legs (2 books) and ETH touched only the call (1 book),
    // so 3 leaf books are affected (NOT 2 underlyings).
    assert_eq!(result.books_affected(), 3);
    assert_eq!(manager.total_order_count(), 0);
}

#[test]
fn test_cancel_by_user_across_underlyings() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));
    let user_a = Hash32::from([1u8; 32]);
    let user_b = Hash32::from([2u8; 32]);

    {
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(exp_date);
        let strike = exp.get_or_create_strike(50000);

        if let Err(err) =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
        {
            panic!("add order failed: {}", err);
        }
    }

    {
        let eth = manager.get_or_create("ETH");
        let exp = eth.get_or_create_expiration(exp_date);
        let strike = exp.get_or_create_strike(3000);

        if let Err(err) =
            strike
                .put()
                .add_limit_order_with_user(OrderId::new(), Side::Sell, 80, 5, user_a)
        {
            panic!("add order failed: {}", err);
        }

        if let Err(err) =
            strike
                .call()
                .add_limit_order_with_user(OrderId::new(), Side::Buy, 90, 6, user_b)
        {
            panic!("add order failed: {}", err);
        }
    }

    let result = match manager.cancel_by_user_across_underlyings(user_a) {
        Ok(result) => result,
        Err(err) => panic!("cancel failed: {}", err),
    };

    assert_eq!(result.total_cancelled(), 2);
    assert_eq!(result.books_affected(), 2);
    assert_eq!(manager.total_order_count(), 1);
}

#[test]
fn test_cancel_by_side_across_underlyings() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

    {
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(exp_date);
        let strike = exp.get_or_create_strike(50000);

        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 110, 5)
        {
            panic!("add order failed: {}", err);
        }
    }

    {
        let eth = manager.get_or_create("ETH");
        let exp = eth.get_or_create_expiration(exp_date);
        let strike = exp.get_or_create_strike(3000);

        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Buy, 50, 7)
        {
            panic!("add order failed: {}", err);
        }
        if let Err(err) = strike
            .put()
            .add_limit_order(OrderId::new(), Side::Sell, 60, 3)
        {
            panic!("add order failed: {}", err);
        }
    }

    assert_eq!(manager.total_order_count(), 4);

    let result = match manager.cancel_by_side_across_underlyings(Side::Buy) {
        Ok(result) => result,
        Err(err) => panic!("cancel failed: {}", err),
    };

    assert_eq!(result.total_cancelled(), 2);
    assert_eq!(result.books_affected(), 2);
    assert_eq!(manager.total_order_count(), 2);
}

#[test]
fn test_hierarchy_set_validation_max_price_propagates_to_new_strikes() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

    let btc = manager.get_or_create("BTC");
    // Configure the price bound BEFORE vivifying the expiration/strike so the
    // new leaf inherits it through the validation propagation path.
    btc.set_validation(ValidationConfig::new().with_max_price(1_000));

    let exp = btc.get_or_create_expiration(exp_date);
    let strike = exp.get_or_create_strike(50000);

    // An above-bound add on the freshly vivified leaf is rejected crate-side.
    let rejected = strike
        .call()
        .add_limit_order(OrderId::new(), Side::Buy, 1_001, 10);
    let err = match rejected {
        Ok(()) => panic!("above-bound add should be rejected on a propagated leaf"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("max_price"));

    // A within-bound add on the same leaf still succeeds.
    strike
        .call()
        .add_limit_order(OrderId::new(), Side::Buy, 1_000, 10)
        .unwrap_or_else(|err| panic!("within-bound add failed: {err}"));
    assert_eq!(strike.call().order_count(), 1);
}

#[test]
fn test_replace_order_through_hierarchy_handles() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

    let btc = manager.get_or_create("BTC");
    let exp = btc.get_or_create_expiration(exp_date);
    let strike = exp.get_or_create_strike(50000);

    let id = OrderId::new();
    strike
        .call()
        .add_limit_order(id, Side::Buy, 100, 10)
        .unwrap_or_else(|err| panic!("add failed: {err}"));

    // Replace through the strike's call() leaf handle.
    let replaced = strike
        .call()
        .replace_order(id, 110, 20, Side::Buy)
        .unwrap_or_else(|err| panic!("replace failed: {err}"));
    assert!(replaced, "replace should report a hit");
    assert_eq!(strike.call().best_bid(), Some(110));
    assert_eq!(strike.call().order_count(), 1);
}

#[test]
fn test_hierarchy_set_specs_price_band_propagates_to_new_strikes() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

    let btc = manager.get_or_create("BTC");
    // A contract-spec band set at the underlying level is derived into the
    // validation config and reaches every leaf vivified afterwards.
    let specs = ContractSpecs::builder()
        .min_price(100)
        .max_price(1_000)
        .build()
        .expect("valid specs");
    btc.set_specs(specs);

    let exp = btc.get_or_create_expiration(exp_date);
    let strike = exp.get_or_create_strike(50000);

    // Within-band add succeeds; out-of-band adds are rejected crate-side.
    strike
        .call()
        .add_limit_order(OrderId::new(), Side::Buy, 500, 10)
        .unwrap_or_else(|err| panic!("within-band add failed: {err}"));
    assert!(
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 1_001, 10)
            .is_err(),
        "above-band add must be rejected"
    );
    assert!(
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 99, 10)
            .is_err(),
        "below-band add must be rejected"
    );
    assert_eq!(strike.call().order_count(), 1);
}

#[test]
fn test_hierarchy_chain_set_specs_band_activates_at_leaf() {
    let manager = UnderlyingOrderBookManager::new();
    let exp_date = ExpirationDate::Days(pos_or_panic!(30.0));

    let btc = manager.get_or_create("BTC");
    let exp = btc.get_or_create_expiration(exp_date);
    let chain = exp.chain();

    // Setting specs directly on the chain (not through the underlying
    // derivation) still activates the band at leaves via effective_validation.
    let specs = ContractSpecs::builder()
        .max_price(2_000)
        .build()
        .expect("valid specs");
    chain.set_specs(specs);

    let strike = chain.get_or_create_strike(50000);
    strike
        .call()
        .add_limit_order(OrderId::new(), Side::Buy, 1_500, 10)
        .unwrap_or_else(|err| panic!("within-band add failed: {err}"));
    assert!(
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 2_001, 10)
            .is_err(),
        "above-band add must be rejected once the chain-level band is set"
    );
    assert_eq!(strike.call().order_count(), 1);
}
