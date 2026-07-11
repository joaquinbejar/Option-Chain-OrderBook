# Migrating `option-chain-orderbook` 0.6.1 → 0.7.0

`0.7.0` is a small, focused breaking release. It marks the journaled sequencer
enums `#[non_exhaustive]` and adds a journaled expiry-sweep command. The only
source break is the wildcard `match` arm; everything else is additive.

---

## `OptionChainCommand` / `OptionChainResult` are now `#[non_exhaustive]`

Both journaled enums gained `#[non_exhaustive]`, so an exhaustive `match` over
either one no longer compiles without a wildcard arm.

**Before (0.6.1)**

```rust
match result {
    OptionChainResult::OrderAdded { order_id } => { /* … */ }
    OptionChainResult::OrderCancelled { order_id } => { /* … */ }
    OptionChainResult::MassCancelled { cancelled_count } => { /* … */ }
    OptionChainResult::StatusChanged { symbol, status } => { /* … */ }
    OptionChainResult::Rejected { reason } => { /* … */ }
    OptionChainResult::BookNotFound { symbol } => { /* … */ }
}
```

**After (0.7.0)**

```rust
match result {
    OptionChainResult::OrderAdded { order_id } => { /* … */ }
    OptionChainResult::OrderCancelled { order_id } => { /* … */ }
    OptionChainResult::MassCancelled { cancelled_count } => { /* … */ }
    OptionChainResult::StatusChanged { symbol, status } => { /* … */ }
    OptionChainResult::ExpiredEvicted { evicted_ids } => { /* … */ }
    OptionChainResult::Rejected { reason } => { /* … */ }
    OptionChainResult::BookNotFound { symbol } => { /* … */ }
    _ => { /* required: future variants land here */ }
}
```

**What to do:** add a catch-all `_ => { … }` arm to every `match` over an
`OptionChainCommand` or `OptionChainResult` value (and handle the new
`EvictExpiredOrders` / `ExpiredEvicted` variants if you care about them). This is
a one-time break: once the wildcard arm is present, later variant additions are
source-compatible. The attribute is Rust-source only — it does **not** change the
serde/journal wire format. `OptionChainEvent` and `OptionChainReceipt` are
structs and are unaffected.

---

## New command: `EvictExpiredOrders` (host-driven expiry sweep)

The sweep #141 surfaced through the hierarchy is now journalable at the wrapper's
sequencer layer.

```rust
use option_chain_orderbook::TimestampMs;

// Evict every GTD/DAY order across the underlying that has expired at `now_ms`.
let receipt = book.submit_evict_expired_orders(TimestampMs::new(now_ms))?;

if let OptionChainResult::ExpiredEvicted { evicted_ids } = receipt.result {
    // Evicted ids in the hierarchy's deterministic sweep order
    // (expirations by key, strikes ascending, call before put, then the
    // engine's per-leaf eviction order). `evicted_ids.len()` is the count.
}
```

`now_ms` is a caller-supplied Unix-milliseconds cutoff; the sweep reads no clock,
so it is a pure function of `now_ms` and the resting books. When journaled, replay
re-applies the recorded `now_ms` (never the replay clock), reproducing the exact
evictions; the sweep is idempotent, so a duplicate replay is a no-op.

### Journal forward-compatibility note

The journal enums keep `#[serde(deny_unknown_fields)]`, so forward-compat is
**asymmetric** (intended):

- A **new** binary reads **old** journals fine (old journals carry only known
  tags).
- An **old** binary reading a **new** journal that contains `EvictExpiredOrders`
  / `ExpiredEvicted` fails to decode — `serde` rejects the unknown variant tag.
  This is deliberately loud rather than a silent replay corruption, matching the
  `orderbook-rs` `SequencerCommand` precedent. Upgrade all readers before writing
  journals that use the new variant.
