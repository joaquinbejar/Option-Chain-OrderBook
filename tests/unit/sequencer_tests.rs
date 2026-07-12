//! Cross-module integration tests for the sequencer replay path (feature
//! `sequencer`).
//!
//! The headline test is the **replay == live oracle** (issue #86): replaying a
//! journaled command prefix into a freshly constructed book must rebuild state
//! that is equal to the live book across the FULL comparable surface — not just
//! top-of-book. The oracle snapshots the entire comparable state of a
//! [`SequencedUnderlyingOrderBook`] and `assert_eq!`s the live snapshot against
//! the replayed snapshot, so a future replay regression in ANY of these
//! dimensions fails the test:
//!
//! - top-level counts (`expiration_count`, `total_order_count`);
//! - per-underlying and per-chain stats;
//! - per-contract **full depth ladder** — every bid and every ask price level
//!   (price, visible + hidden size, per-level order count), plus the total
//!   bid/ask depth and the bid/ask level counts. The prefix deliberately rests
//!   multiple price levels per side, so a below-top (level-2) divergence that
//!   left `best_quote` and `order_count` intact still fails the oracle;
//! - per-contract lifecycle `status` (made load-bearing by halting a contract
//!   through a `SetInstrumentStatus` command and proving the halt — and the
//!   rejection of a subsequent add — replays faithfully);
//! - the instrument registry ids and the symbol index.
//!
//! What is intentionally NOT asserted: intra-level queue position (FIFO /
//! time-priority ordering of individual orders *within* a single price level),
//! because the leaf exposes no public per-order-queue accessor. The depth ladder
//! pins each level's aggregate (price/size/order-count) but not the order of
//! orders inside it — see the follow-up note in the issue.
//!
//! Determinism is proven two ways: the test inputs are fully deterministic
//! (fixed sequential order ids, fixed prices, fixed `YYYYMMDD` symbols that
//! parse to absolute `ExpirationDate::DateTime` expiries — never clock-relative
//! `Days`), and two independent fresh books replay the same journal and are
//! asserted byte-for-byte equal to each other and to live.

use option_chain_orderbook::SymbolParser;
use option_chain_orderbook::orderbook::{
    InMemoryOptionChainJournal, InstrumentRegistry, InstrumentStatus, MassCancelScope,
    MassCancelType, OptionChainJournal, OptionOrderBook, SequencedUnderlyingOrderBook, SymbolIndex,
};
use optionstratlib::OptionStyle;
use orderbook_rs::{OrderId, Side};
use std::sync::Arc;

// ── Deterministic test inputs ──────────────────────────────────────────────
//
// All symbols use absolute `YYYYMMDD` dates that parse to
// `ExpirationDate::DateTime`, so the derived symbols are replay-stable — the
// #84 limitation that clock-relative `Days` expiries would violate.

/// A deterministic, sequential order id (no clock / RNG in the test inputs).
fn oid(n: u64) -> OrderId {
    OrderId::from_u64(n)
}

/// Drives the realistic command prefix through the (journaling) sequencer.
///
/// Mix: 15 `AddOrder`s spanning two expirations × three strikes, calls and
/// puts, both sides, with multiple price levels per side so top-of-book is
/// non-trivial; 2 `CancelOrder`s; 1 `SetInstrumentStatus` (halt a55c) followed
/// by an `AddOrder` to the halted book that the LIVE run must reject (so the
/// `status` dimension is load-bearing and the reject-on-replay path is
/// exercised); and 2 `MassCancel`s (a by-book cancel-all and a by-side strike
/// cancel). Returns the number of commands submitted.
fn drive_command_prefix(book: &SequencedUnderlyingOrderBook) -> usize {
    // ── Expiration A: strike 50000 (call/put), strike 55000 (call/put) ──
    let a50c = "BTC-20240329-50000-C";
    let a50p = "BTC-20240329-50000-P";
    let a55c = "BTC-20240329-55000-C";
    let a55p = "BTC-20240329-55000-P";
    // ── Expiration B: strike 60000 (call/put) ──
    let b60c = "BTC-20240628-60000-C";
    let b60p = "BTC-20240628-60000-P";

    // A 50000-C: two bid levels + two ask levels (rich top-of-book).
    book.submit_add_order(a50c, oid(1), Side::Buy, 100, 10)
        .expect("add a50c buy 100");
    book.submit_add_order(a50c, oid(2), Side::Buy, 99, 5)
        .expect("add a50c buy 99");
    book.submit_add_order(a50c, oid(3), Side::Sell, 110, 8)
        .expect("add a50c sell 110");
    book.submit_add_order(a50c, oid(4), Side::Sell, 112, 4)
        .expect("add a50c sell 112");

    // A 50000-P: one level each side.
    book.submit_add_order(a50p, oid(5), Side::Buy, 50, 6)
        .expect("add a50p buy");
    book.submit_add_order(a50p, oid(6), Side::Sell, 60, 3)
        .expect("add a50p sell");

    // A 55000-C: two bid levels + one ask level.
    book.submit_add_order(a55c, oid(7), Side::Buy, 120, 4)
        .expect("add a55c buy 120");
    book.submit_add_order(a55c, oid(8), Side::Buy, 118, 2)
        .expect("add a55c buy 118");
    book.submit_add_order(a55c, oid(9), Side::Sell, 130, 7)
        .expect("add a55c sell 130");

    // A 55000-P: a lone resting sell (later wiped by a by-book mass cancel).
    book.submit_add_order(a55p, oid(10), Side::Sell, 70, 5)
        .expect("add a55p sell");

    // B 60000-C: one bid + two ask levels (asks later wiped by-side).
    book.submit_add_order(b60c, oid(11), Side::Buy, 200, 3)
        .expect("add b60c buy");
    book.submit_add_order(b60c, oid(12), Side::Sell, 210, 9)
        .expect("add b60c sell 210");
    book.submit_add_order(b60c, oid(13), Side::Sell, 215, 1)
        .expect("add b60c sell 215");

    // B 60000-P: two bid levels.
    book.submit_add_order(b60p, oid(14), Side::Buy, 80, 12)
        .expect("add b60p buy 80");
    book.submit_add_order(b60p, oid(15), Side::Buy, 78, 4)
        .expect("add b60p buy 78");

    // ── Cancels: peel off the second level on each side of A 50000-C ──
    book.submit_cancel_order(a50c, oid(4))
        .expect("cancel a50c sell 112");
    book.submit_cancel_order(a50c, oid(2))
        .expect("cancel a50c buy 99");

    // ── Halt a55c (already created above) AFTER its 3 orders rested. A halt
    //    does not cancel resting orders; it stops the book accepting new ones.
    //    This journaled status transition must be reconstructed on replay. ──
    let halt = book
        .submit_set_instrument_status(a55c, InstrumentStatus::Halted)
        .expect("submit halt a55c");
    assert!(
        halt.result.is_success(),
        "halting a55c must succeed: {halt:?}"
    );

    // ── AddOrder to the HALTED a55c: the live run rejects it (InstrumentNot
    //    Active), so it neither rests nor changes top-of-book. Replay must
    //    reconstruct Halted and reject the same add — making `status` real. ──
    let rejected = book
        .submit_add_order(a55c, oid(16), Side::Buy, 121, 1)
        .expect("submit add to halted a55c");
    assert!(
        rejected.result.is_error(),
        "add to a halted book must be rejected live: {rejected:?}"
    );

    // ── Mass cancel #1: cancel-all on the A 55000-P book (wipes oid 10) ──
    book.submit_mass_cancel(MassCancelScope::Book(a55p.to_string()), MassCancelType::All)
        .expect("mass cancel a55p book");

    // ── Mass cancel #2: cancel the SELL side of the whole B 60000 strike ──
    let exp_b = *SymbolParser::parse(b60c).expect("parse b60c").expiration();
    book.submit_mass_cancel(
        MassCancelScope::Strike {
            expiration: exp_b,
            strike: 60000,
        },
        MassCancelType::BySide(Side::Sell),
    )
    .expect("mass cancel b60 strike sell");

    // 15 adds + 2 cancels + 1 halt + 1 rejected add + 2 mass cancels.
    21
}

// ── Comparable full-state snapshot (the oracle) ────────────────────────────

/// One price level in a contract's depth ladder: price, visible + hidden size,
/// and the number of orders resting at that level. Capturing this for EVERY
/// level (not just the top) is what makes the oracle full-depth equality rather
/// than top-of-book equality — a level-2 divergence on replay fails here even
/// when `best_quote` and the contract order count are unchanged.
#[derive(Debug, PartialEq, Eq)]
struct LevelSnapshot {
    price: u128,
    visible: u64,
    hidden: u64,
    order_count: usize,
}

/// Per-contract structural state: the full bid/ask depth ladder, top-of-book
/// (bid/ask price + size), two-sidedness, total per-side depth, per-side level
/// counts, resting order count, and lifecycle status. These are the dimensions
/// a replay regression would disturb.
///
/// NOT captured: intra-level queue position (the order of individual orders
/// within a single price level), because the leaf exposes no public
/// per-order-queue accessor. `bids`/`asks` pin each level's aggregate, not the
/// queue order inside it.
#[derive(Debug, PartialEq)]
struct ContractSnapshot {
    symbol: String,
    bid_price: Option<u128>,
    bid_size: u64,
    ask_price: Option<u128>,
    ask_size: u64,
    is_two_sided: bool,
    order_count: usize,
    status: InstrumentStatus,
    // Full depth ladder (each side sorted by price for a deterministic compare).
    bids: Vec<LevelSnapshot>,
    asks: Vec<LevelSnapshot>,
    total_bid_depth: u64,
    total_ask_depth: u64,
    bid_level_count: usize,
    ask_level_count: usize,
}

/// Per-expiration state: the `OptionChainStats` (strike count + orders), the
/// sorted strike list, and the call/put contract snapshots per strike.
#[derive(Debug, PartialEq)]
struct ExpirationSnapshot {
    expiration: String,
    chain_strike_count: usize,
    chain_total_orders: usize,
    strikes: Vec<u64>,
    contracts: Vec<ContractSnapshot>,
}

/// One registry entry, captured via the deterministic id-sorted
/// [`InstrumentRegistry::iter`] (#75).
#[derive(Debug, PartialEq)]
struct RegistryEntry {
    id: u32,
    symbol: String,
    expiration: String,
    strike: u64,
    style: OptionStyle,
}

/// One symbol-index entry, sorted by symbol so the comparison is order-stable
/// despite the index's arbitrary `DashMap` iteration order.
#[derive(Debug, PartialEq)]
struct SymbolIndexEntry {
    symbol: String,
    underlying: String,
    expiration: String,
    strike: u64,
    style: OptionStyle,
}

/// The full comparable state of a [`SequencedUnderlyingOrderBook`]. Equality of
/// two snapshots is the replay correctness oracle.
#[derive(Debug, PartialEq)]
struct BookSnapshot {
    underlying: String,
    expiration_count: usize,
    total_order_count: usize,
    stats_expiration_count: usize,
    stats_total_strikes: usize,
    stats_total_orders: usize,
    expirations: Vec<ExpirationSnapshot>,
    registry: Vec<RegistryEntry>,
    symbol_index: Vec<SymbolIndexEntry>,
}

/// Depth large enough to capture every price level in the test prefix (max 2
/// levels per side); oversized so a regression that adds spurious levels is
/// still captured.
const SNAPSHOT_DEPTH: usize = 16;

/// Folds an order-book snapshot's price-level `Vec` into a deterministic,
/// price-sorted ladder. The leaf snapshot's level order is not contractually
/// stable, so both runs are sorted identically before comparison.
fn level_ladder(levels: &[pricelevel::PriceLevelSnapshot]) -> Vec<LevelSnapshot> {
    let mut ladder: Vec<LevelSnapshot> = levels
        .iter()
        .map(|level| LevelSnapshot {
            price: level.price().as_u128(),
            visible: level.visible_quantity().as_u64(),
            hidden: level.hidden_quantity().as_u64(),
            order_count: level.order_count(),
        })
        .collect();
    ladder.sort_by_key(|level| level.price);
    ladder
}

/// Snapshots a single contract's full depth ladder, top-of-book, and order
/// state.
fn contract_snapshot(leaf: &OptionOrderBook) -> ContractSnapshot {
    let quote = leaf.best_quote();
    let book = leaf.snapshot(SNAPSHOT_DEPTH);
    ContractSnapshot {
        symbol: leaf.symbol().to_string(),
        bid_price: quote.bid_price().map(|p| p.as_u128()),
        bid_size: quote.bid_size().as_u64(),
        ask_price: quote.ask_price().map(|p| p.as_u128()),
        ask_size: quote.ask_size().as_u64(),
        is_two_sided: quote.is_two_sided(),
        order_count: leaf.order_count(),
        status: leaf.status(),
        bids: level_ladder(&book.bids),
        asks: level_ladder(&book.asks),
        total_bid_depth: leaf.total_bid_depth(),
        total_ask_depth: leaf.total_ask_depth(),
        bid_level_count: leaf.bid_level_count(),
        ask_level_count: leaf.ask_level_count(),
    }
}

/// Walks the entire hierarchy of a sequenced book and captures its full
/// comparable state. Traversal order is deterministic: expirations come from
/// the ordered `ExpirationOrderBookManager::iter`, strikes from the sorted
/// `strike_prices`, the registry from the id-sorted `iter`, and the symbol
/// index is sorted by symbol here.
fn snapshot(book: &SequencedUnderlyingOrderBook) -> BookSnapshot {
    let underlying = book.underlying();
    let stats = underlying.stats();

    let mut expirations = Vec::new();
    for (exp_date, exp_book) in underlying.expirations().iter() {
        let chain_stats = exp_book.chain().stats();
        let strikes = exp_book.strike_prices();
        let mut contracts = Vec::with_capacity(strikes.len() * 2);
        for &strike in &strikes {
            let strike_book = exp_book
                .get_strike(strike)
                .expect("strike from strike_prices() must resolve");
            contracts.push(contract_snapshot(strike_book.call()));
            contracts.push(contract_snapshot(strike_book.put()));
        }
        expirations.push(ExpirationSnapshot {
            expiration: exp_date.to_string(),
            chain_strike_count: chain_stats.strike_count,
            chain_total_orders: chain_stats.total_orders,
            strikes,
            contracts,
        });
    }

    let registry = book
        .registry()
        .expect("oracle book is constructed with a registry")
        .iter()
        .into_iter()
        .map(|(id, info)| RegistryEntry {
            id,
            symbol: info.symbol().to_string(),
            expiration: info.expiration().to_string(),
            strike: info.strike(),
            style: info.option_style(),
        })
        .collect();

    let mut symbol_index: Vec<SymbolIndexEntry> = book
        .symbol_index()
        .expect("oracle book is constructed with a symbol index")
        .entries()
        .into_iter()
        .map(|(symbol, sym_ref)| SymbolIndexEntry {
            symbol,
            underlying: sym_ref.underlying().to_string(),
            expiration: sym_ref.expiration().to_string(),
            strike: sym_ref.strike(),
            style: sym_ref.option_style(),
        })
        .collect();
    symbol_index.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    BookSnapshot {
        underlying: underlying.underlying().to_string(),
        expiration_count: book.expiration_count(),
        total_order_count: book.total_order_count(),
        stats_expiration_count: stats.expiration_count,
        stats_total_strikes: stats.total_strikes,
        stats_total_orders: stats.total_orders,
        expirations,
        registry,
        symbol_index,
    }
}

/// Builds a fresh, registry+index-backed sequenced book sharing `journal`. Each
/// book gets its OWN fresh registry/index so id allocation starts from 1 — that
/// is what makes the registry ids comparable across the live and replayed runs.
fn fresh_book(journal: &Arc<InMemoryOptionChainJournal>) -> SequencedUnderlyingOrderBook {
    let journal_dyn: Arc<dyn OptionChainJournal> =
        Arc::clone(journal) as Arc<dyn OptionChainJournal>;
    SequencedUnderlyingOrderBook::with_journal_registry_and_index(
        "BTC",
        journal_dyn,
        Arc::new(InstrumentRegistry::new()),
        Arc::new(SymbolIndex::new()),
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn test_replay_equals_live_full_state_oracle() {
    let journal = Arc::new(InMemoryOptionChainJournal::new());

    // ── Live run: drive the journaled command prefix ──
    let live = fresh_book(&journal);
    let submitted = drive_command_prefix(&live);
    assert_eq!(
        journal.len(),
        submitted,
        "every submitted command must be journaled"
    );

    // Spot-check the live structure so the prefix's intent is explicit and a
    // silently-rejected order would be caught before the oracle runs.
    // The halted a55c keeps its 3 resting orders; the rejected add rested
    // nothing, so the total is unchanged at 10.
    assert_eq!(live.expiration_count(), 2);
    assert_eq!(live.total_order_count(), 10);
    let exp_a = live
        .underlying()
        .get_expiration(
            SymbolParser::parse("BTC-20240329-50000-C")
                .expect("parse a50c")
                .expiration(),
        )
        .expect("exp A present");
    let a50c = exp_a.get_strike(50000).expect("strike 50000 present");
    let q = a50c.call().best_quote();
    assert_eq!(q.bid_price().map(|p| p.as_u128()), Some(100));
    assert_eq!(q.bid_size().as_u64(), 10);
    assert_eq!(q.ask_price().map(|p| p.as_u128()), Some(110));
    assert_eq!(q.ask_size().as_u64(), 8);
    assert_eq!(a50c.call().order_count(), 2);

    // a55c was halted AFTER its 3 orders rested; the halt is load-bearing.
    let a55c = exp_a.get_strike(55000).expect("strike 55000 present");
    assert_eq!(a55c.call().status(), InstrumentStatus::Halted);
    assert_eq!(
        a55c.call().order_count(),
        3,
        "halt preserves resting orders"
    );
    assert_eq!(a55c.call().bid_level_count(), 2, "two resting bid levels");
    assert_eq!(a55c.call().best_bid(), Some(120));

    let live_snapshot = snapshot(&live);

    // ── Replay into a fresh, identically-configured book ──
    let replay_a = fresh_book(&journal);
    assert_eq!(replay_a.expiration_count(), 0, "fresh book starts empty");
    assert_eq!(replay_a.total_order_count(), 0);
    let replayed_a = replay_a.replay(0).expect("replay a");
    assert_eq!(
        replayed_a, submitted,
        "replay re-runs every journaled command"
    );

    let snapshot_a = snapshot(&replay_a);

    // THE ORACLE: replayed state equals live state in EVERY dimension —
    // top-level counts, per-contract top-of-book + order counts, per-chain and
    // per-underlying stats, registry ids, and the symbol index.
    assert_eq!(
        live_snapshot, snapshot_a,
        "replayed state must equal live state across the full comparable surface"
    );

    // ── Run-to-run determinism: a second independent replay is identical ──
    let replay_b = fresh_book(&journal);
    let replayed_b = replay_b.replay(0).expect("replay b");
    assert_eq!(replayed_b, submitted);
    let snapshot_b = snapshot(&replay_b);

    assert_eq!(
        snapshot_a, snapshot_b,
        "two independent replays of the same journal must be byte-for-byte equal"
    );
    assert_eq!(
        live_snapshot, snapshot_b,
        "the second replay must also equal live"
    );
}

#[test]
fn test_replay_registry_ids_match_live() {
    // A focused assertion that the registry-assigned instrument ids — not just
    // the symbols — are identical between live and replay. They match because
    // strike creation (the sole id-allocation site) happens in the SAME
    // deterministic command order on both runs.
    let journal = Arc::new(InMemoryOptionChainJournal::new());

    let live = fresh_book(&journal);
    drive_command_prefix(&live);

    let replay = fresh_book(&journal);
    replay.replay(0).expect("replay");

    let live_ids = live.registry().expect("live registry").iter();
    let replay_ids = replay.registry().expect("replay registry").iter();

    assert_eq!(
        live_ids.len(),
        replay_ids.len(),
        "same number of instruments registered"
    );
    for ((live_id, live_info), (replay_id, replay_info)) in live_ids.iter().zip(replay_ids.iter()) {
        assert_eq!(live_id, replay_id, "instrument id diverged on replay");
        assert_eq!(
            live_info.symbol(),
            replay_info.symbol(),
            "instrument symbol diverged for id {live_id}"
        );
    }
    // Six leaf books: 3 strikes × (call + put).
    assert_eq!(live_ids.len(), 6);
}

#[test]
fn test_replay_determinism_repeated() {
    // Loop many independent replays of the same journal and assert each rebuilds
    // a snapshot identical to live — proving replay is deterministic run-to-run
    // within a single process (no map-order / clock leakage).
    let journal = Arc::new(InMemoryOptionChainJournal::new());

    let live = fresh_book(&journal);
    drive_command_prefix(&live);
    let live_snapshot = snapshot(&live);

    for round in 0..16 {
        let replay = fresh_book(&journal);
        replay.replay(0).expect("replay round");
        assert_eq!(
            live_snapshot,
            snapshot(&replay),
            "replay round {round} diverged from live"
        );
    }
}

#[test]
fn test_replay_into_empty_book_then_resume_is_consistent() {
    // After replay rebuilds state, the sequencer is advanced past the replayed
    // range, so a NEW command appends at the correct sequence and lands in the
    // rebuilt hierarchy — replay leaves a usable, consistent book.
    let journal = Arc::new(InMemoryOptionChainJournal::new());

    let live = fresh_book(&journal);
    let submitted = drive_command_prefix(&live);

    let resumed = fresh_book(&journal);
    resumed.replay(0).expect("replay");
    assert!(
        resumed.current_sequence() >= submitted as u64,
        "sequencer must advance past the replayed range"
    );

    // A post-replay command rests in the rebuilt hierarchy and journals next.
    let new_oid = oid(1000);
    resumed
        .submit_add_order("BTC-20240329-50000-C", new_oid, Side::Buy, 101, 2)
        .expect("post-replay add");
    assert_eq!(journal.len(), submitted + 1);
    // 10 rebuilt resting orders + 1 new.
    assert_eq!(resumed.total_order_count(), 11);
}

// ── Concurrent submit: gate ordering + replay-under-load oracle ─────────────

/// Number of worker threads used by the concurrent-load helper.
const CONCURRENT_THREADS: u64 = 8;
/// Number of `AddOrder` commands each worker submits.
const CONCURRENT_PER_THREAD: u64 = 25;

/// Drives a concurrent `AddOrder` load through the sequencer.
///
/// Spawns [`CONCURRENT_THREADS`] workers that each submit
/// [`CONCURRENT_PER_THREAD`] orders to a per-thread-distinct symbol, released in
/// lockstep by a [`Barrier`](std::sync::Barrier) so the submissions genuinely
/// contend for the sequencer gate rather than running one after another.
/// Distinct strikes keep the workers from matching against each other, and the
/// absolute `YYYYMMDD` date makes every derived symbol replay-stable. Returns
/// the total number of commands submitted.
fn drive_concurrent_add_load(book: &Arc<SequencedUnderlyingOrderBook>) -> usize {
    use std::sync::Barrier;
    use std::thread;

    let barrier = Arc::new(Barrier::new(CONCURRENT_THREADS as usize));
    let mut handles = Vec::with_capacity(CONCURRENT_THREADS as usize);
    for t in 0..CONCURRENT_THREADS {
        let book = Arc::clone(book);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let symbol = format!("BTC-20240329-{}-C", 50000 + t * 1000);
            barrier.wait();
            for i in 0..CONCURRENT_PER_THREAD {
                // Order ids are offset by 1 so no worker ever submits id 0, and
                // the per-thread 1000-stride keeps them globally distinct.
                book.submit_add_order(
                    &symbol,
                    oid(1 + t * 1000 + i),
                    Side::Buy,
                    100 + u128::from(i),
                    10,
                )
                .expect("concurrent add must succeed");
            }
        }));
    }
    for handle in handles {
        handle.join().expect("worker thread must not panic");
    }
    (CONCURRENT_THREADS * CONCURRENT_PER_THREAD) as usize
}

#[test]
fn test_submit_concurrent_journal_order_matches_sequence_order() {
    // The submit gate serializes assign→execute→journal-append, so even under a
    // concurrent load the journal is appended in sequence order.
    let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
    let book = Arc::new(SequencedUnderlyingOrderBook::with_journal(
        "BTC",
        Arc::clone(&journal),
    ));

    let total = drive_concurrent_add_load(&book);
    assert_eq!(total, 200);

    // Journal insertion order equals sequence order: reading from 0 yields
    // sequence numbers 0..200, strictly ascending by exactly +1, in the order
    // they were appended.
    let events = journal.read_from(0).expect("read journal");
    assert_eq!(
        events.len(),
        200,
        "every concurrent submit must be journaled exactly once"
    );
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.sequence_num, index as u64,
            "journal entry at position {index} must carry sequence {index} \
             (insertion order == sequence order)"
        );
    }

    // Each submit records exactly one outcome (success or reject).
    assert_eq!(
        book.success_count() + book.reject_count(),
        200,
        "each of the 200 submits records exactly one outcome"
    );
}

#[test]
fn test_submit_concurrent_replay_equals_live_full_state_oracle() {
    // Under a concurrent load the gate makes strike creation — and therefore
    // registry-id allocation — happen in sequence order across threads, so
    // replay (which re-runs the journal in that same order into a fresh book)
    // rebuilds an identical hierarchy AND identical registry ids. This exercises
    // the full-state oracle against a concurrently-produced journal.
    let journal = Arc::new(InMemoryOptionChainJournal::new());

    let live = Arc::new(fresh_book(&journal));
    let submitted = drive_concurrent_add_load(&live);
    assert_eq!(
        journal.len(),
        submitted,
        "every concurrent command must be journaled"
    );

    let live_snapshot = snapshot(&live);

    // Replay into a fresh, identically-configured book and compare.
    let replay = fresh_book(&journal);
    let replayed = replay.replay(0).expect("replay");
    assert_eq!(
        replayed, submitted,
        "replay must re-run every journaled command"
    );

    assert_eq!(
        live_snapshot,
        snapshot(&replay),
        "replayed state must equal live state after a concurrent load"
    );
}
