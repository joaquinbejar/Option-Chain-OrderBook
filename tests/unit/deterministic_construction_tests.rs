//! Construction of the hierarchy with an engine clock and a trade-ID root
//! namespace: node ids derive from the root and the node's path, the leaves
//! carry the configured clock, tracking and STP mode, building reads no
//! time, and trade ids repeat across hierarchies.

use chrono::{TimeZone, Utc};
use option_chain_orderbook::orderbook::{OptionOrderBook, UnderlyingOrderBookManager};
use option_chain_orderbook::{
    Clock, Hash32, OrderId, STPMode, Side, StubClock, TimeInForce, TimestampMs, Uuid,
};
use optionstratlib::ExpirationDate;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const ROOT: Uuid = Uuid::from_u128(0x5eed_0000_0000_0000_0000_0000_0000_0001);
const STRIKE: u64 = 50_000;

fn expiry() -> ExpirationDate {
    ExpirationDate::DateTime(
        Utc.with_ymd_and_hms(2030, 3, 29, 8, 0, 0)
            .single()
            .expect("valid instant"),
    )
}

/// A clock that counts its reads and always answers the same millisecond.
#[derive(Debug, Default)]
struct CountingClock {
    reads: AtomicU64,
}

impl Clock for CountingClock {
    fn now_millis(&self) -> TimestampMs {
        self.reads.fetch_add(1, Ordering::Relaxed);
        TimestampMs::new(1_000)
    }
}

fn manager(clock: Option<Arc<dyn Clock>>, root: Option<Uuid>) -> UnderlyingOrderBookManager {
    let manager = UnderlyingOrderBookManager::new();
    if let Some(clock) = clock {
        manager.set_clock(clock);
    }
    if let Some(root) = root {
        manager.set_trade_id_namespace(root);
    }
    manager.set_state_tracking(false);
    manager.set_stp_mode(STPMode::CancelMaker);
    manager
}

/// The ids of every node on the path to `STRIKE`: expiration, chain,
/// strike, call and put.
fn node_ids(manager: &UnderlyingOrderBookManager) -> [OrderId; 5] {
    let expiration = manager
        .get_or_create("BTC")
        .get_or_create_expiration(expiry());
    let strike = expiration.get_or_create_strike(STRIKE);
    [
        expiration.id(),
        expiration.chain().id(),
        strike.id(),
        strike.call().id(),
        strike.put().id(),
    ]
}

fn call_leaf(manager: &UnderlyingOrderBookManager) -> Arc<OptionOrderBook> {
    manager
        .get_or_create("BTC")
        .get_or_create_expiration(expiry())
        .get_or_create_strike(STRIKE)
        .call_arc()
}

/// Trades a fixed stream on the call leaf and returns the trade ids.
fn trade_ids(leaf: &OptionOrderBook) -> Vec<String> {
    let maker = Hash32([1; 32]);
    let taker = Hash32([2; 32]);
    let mut ids = Vec::new();
    for n in 0..3_u64 {
        leaf.add_limit_order_with_tif_and_user_full(
            OrderId::Sequential(2 * n + 1),
            Side::Sell,
            100,
            5,
            TimeInForce::Gtc,
            maker,
        )
        .expect("resting sell");
        let result = leaf
            .add_limit_order_with_tif_and_user_full(
                OrderId::Sequential(2 * n + 2),
                Side::Buy,
                100,
                5,
                TimeInForce::Gtc,
                taker,
            )
            .expect("crossing buy");
        for trade in result.match_result.trades().as_vec() {
            ids.push(trade.trade_id().to_string());
        }
    }
    ids
}

#[test]
fn test_construction_same_root_same_node_ids_across_hierarchies() {
    let clock = || Some(Arc::new(StubClock::new()) as Arc<dyn Clock>);
    let a = node_ids(&manager(clock(), Some(ROOT)));
    let b = node_ids(&manager(clock(), Some(ROOT)));
    assert_eq!(a, b, "same root and same paths give the same ids");

    let mut distinct: Vec<String> = a.iter().map(ToString::to_string).collect();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 5, "every level and both styles differ");

    let other = node_ids(&manager(clock(), Some(Uuid::from_u128(7))));
    for (x, y) in a.iter().zip(&other) {
        assert_ne!(x, y, "another root separates every node");
    }
}

#[test]
fn test_construction_root_only_still_derives_node_ids() {
    // Node ids depend on the root alone; the clock is independent of them.
    assert_eq!(
        node_ids(&manager(None, Some(ROOT))),
        node_ids(&manager(None, Some(ROOT)))
    );
}

#[test]
fn test_construction_without_root_node_ids_are_random() {
    for clock in [None, Some(Arc::new(StubClock::new()) as Arc<dyn Clock>)] {
        let a = node_ids(&manager(clock.clone(), None));
        let b = node_ids(&manager(clock, None));
        for (x, y) in a.iter().zip(&b) {
            assert_ne!(x, y, "without a root, ids stay random");
        }
    }
}

#[test]
fn test_construction_recreated_strike_keeps_its_id() {
    let manager = manager(Some(Arc::new(StubClock::new())), Some(ROOT));
    let expiration = manager
        .get_or_create("BTC")
        .get_or_create_expiration(expiry());
    let first = expiration.get_or_create_strike(STRIKE);
    let ids = (first.id(), first.call().id(), first.put().id());
    drop(first);
    assert!(expiration.chain().strikes().remove(STRIKE));
    let again = expiration.get_or_create_strike(STRIKE);
    assert_eq!((again.id(), again.call().id(), again.put().id()), ids);
}

#[test]
fn test_construction_leaves_carry_configured_clock_tracking_and_stp() {
    let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
    let manager = manager(Some(Arc::clone(&clock)), Some(ROOT));
    let leaf = call_leaf(&manager);
    assert!(
        Arc::ptr_eq(leaf.inner().clock(), &clock),
        "the leaf runs on the injected clock, never on MonotonicClock"
    );
    assert!(leaf.inner().order_state_tracker().is_none());
    assert_eq!(leaf.stp_mode(), STPMode::CancelMaker);
    assert_eq!(leaf.inner().stp_mode(), STPMode::CancelMaker);
}

#[test]
fn test_construction_with_clock_and_root_reads_no_time() {
    let clock = Arc::new(CountingClock::default());
    let manager = manager(Some(Arc::clone(&clock) as Arc<dyn Clock>), Some(ROOT));
    let leaf = call_leaf(&manager);
    assert_eq!(
        clock.reads.load(Ordering::Relaxed),
        0,
        "building the hierarchy reads no time"
    );
    let _ = trade_ids(&leaf);
    assert!(
        clock.reads.load(Ordering::Relaxed) > 0,
        "operations read the injected clock"
    );
}

#[test]
fn test_construction_same_root_same_trade_ids_across_hierarchies() {
    let run = || {
        let manager = manager(Some(Arc::new(StubClock::new())), Some(ROOT));
        trade_ids(&call_leaf(&manager))
    };
    let first = run();
    assert_eq!(first.len(), 3);
    assert_eq!(first, run());
}

#[test]
fn test_construction_setters_reach_only_nodes_built_after_them() {
    // Every node keeps the configuration it was built with, at every level:
    // a strike added later under an expiration built before the setters
    // still gets that expiration's old configuration. So the setters must
    // run before the first node exists.
    let manager = UnderlyingOrderBookManager::new();
    let early = call_leaf(&manager);
    let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
    manager.set_clock(Arc::clone(&clock));
    manager.set_trade_id_namespace(ROOT);
    assert!(!Arc::ptr_eq(early.inner().clock(), &clock));
    let under_old_expiration = manager
        .get_or_create("BTC")
        .get_or_create_expiration(expiry())
        .get_or_create_strike(STRIKE + 1)
        .call_arc();
    assert!(!Arc::ptr_eq(under_old_expiration.inner().clock(), &clock));
    let under_new_underlying = manager
        .get_or_create("ETH")
        .get_or_create_expiration(expiry())
        .get_or_create_strike(STRIKE)
        .call_arc();
    assert!(Arc::ptr_eq(under_new_underlying.inner().clock(), &clock));
}
