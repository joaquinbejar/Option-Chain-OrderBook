# Migrating `option-chain-orderbook` 0.8.0 → 0.9.0

`0.9.0` ships per-contract price banding (#152), order-kind variety through
the sequenced path (#151), and deterministic trade IDs plus replace fills
(#153). The journal wire format is backward-compatible for self-describing
encodings — a 0.9.0 binary reads every 0.5.0+ **JSON** journal unchanged
(positional codecs such as bincode cannot apply missing-field defaults; the
established caveat from `MIGRATING-0.8.0.md` applies to the new fields too).

---

## `orderbook-rs` public dependency moved `0.10` → `0.11`

`orderbook-rs` types are re-exported at this crate's root, so downstream
crates that co-pin `orderbook-rs` directly must move their own pin to `0.11`
in the same update (same precedent as the 0.6.0 `0.9` → `0.10` bump). The
only breaking changes in orderbook-rs 0.11 are in `ReplayBookConfig` /
`ReplayError`, which this crate does not use or re-export.

## `AddOrder` gained fields; `OrderReplaced` gained a field

`OptionChainCommand::AddOrder` gained `kind: OrderKind` (serde default
`Limit`) and `hidden_quantity: Option<u64>` (default `None`);
`OptionChainResult::OrderReplaced` gained `trade: Option<TradeResult>`
(default `None`). Wildcard `match`es keep compiling; **literal constructors**
and **full field patterns** need the new fields (or `..`).

**Before (0.8.0)**

```rust
let cmd = OptionChainCommand::AddOrder {
    symbol, order_id, side, price, quantity,
    tif: TimeInForce::Gtc,
    user_id: Hash32::zero(),
};

if let OptionChainResult::OrderReplaced { order_id } = receipt.result { /* … */ }
```

**After (0.9.0)**

```rust
use option_chain_orderbook::OrderKind;

let cmd = OptionChainCommand::AddOrder {
    symbol, order_id, side, price, quantity,
    tif: TimeInForce::Gtc,
    user_id: Hash32::zero(),
    kind: OrderKind::Limit,     // pre-0.9.0 behavior
    hidden_quantity: None,      // pre-0.9.0 behavior
};

if let OptionChainResult::OrderReplaced { order_id, .. } = receipt.result { /* … */ }
```

**What to do:** add the fields to literals (`OrderKind::Limit` + `None`
reproduce the old semantics exactly) and `..` to field patterns — or prefer
the wrappers (`submit_add_order`, `submit_add_order_with`,
`submit_add_order_kind`, `submit_replace_order`), which are all
signature-stable.

---

## Everything else is additive

- **Price banding (#152):** `ContractSpecs` gained optional inclusive
  `min_price` / `max_price` (smallest price units); `ValidationConfig` gained
  `min_price` and `tightened_price_band` (tightest-wins merge). Enforced
  crate-side at the leaf on every add and replace path. Band-free specs
  behave byte-identically to 0.8.0.
- **Order kinds (#151):** `OrderKind` (`Limit` / `PostOnly` / `Iceberg`) +
  `submit_add_order_kind(...)`; leaf
  `add_post_only_order_with_tif_and_user[_full]` and
  `add_iceberg_order_with_tif_and_user[_full]`. A crossing post-only never
  trades (deterministic `PriceCrossing` rejection); icebergs carry
  `quantity` = visible tranche + `hidden_quantity` reserve.
- **Deterministic trade IDs (#153):** `set_trade_id_namespace` /
  `clear_trade_id_namespace` / `trade_id_namespace()` at every hierarchy
  level plus `SequencedUnderlyingOrderBook::set_trade_id_namespace`
  (gate-linearized). Each leaf derives `UUIDv5(root, leaf_symbol)` at
  construction. With a root namespace and an injected clock both set before
  the first submit, two identically-seeded **live** sequenced runs on fresh
  instances produce identical trade payloads (trade ids, engine trade
  timestamps, `engine_seq`); the per-event envelope `timestamp_ns` remains
  wall-clock. `uuid` is a direct dependency and `Uuid` is re-exported.
- **Replace fills (#153):** leaf `replace_order_full` returns the
  replacement's on-entry fills (see its documented attribution contract —
  strictly single-writer per book); the sequenced `OrderReplaced` carries
  them.
