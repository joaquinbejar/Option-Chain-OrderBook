# Migrating `option-chain-orderbook` 0.7.0 → 0.8.0

`0.8.0` delivers the deterministic venue seams (#147) and the lossless venue
integration surface (#148). The journal wire format is backward-compatible for
self-describing encodings — a 0.8.0 binary reads every 0.5.0+ **JSON** journal
unchanged (positional codecs such as bincode cannot apply missing-field
defaults; see the wire-format note below). The only source breaks are in code
that constructs or exhaustively destructures two sequencer enum variants by
literal.

---

## `AddOrder` and `OrderAdded` variants gained fields

`OptionChainCommand::AddOrder` gained `tif: TimeInForce` and
`user_id: Hash32`; `OptionChainResult::OrderAdded` gained
`trade: Option<TradeResult>`. Both enums were already `#[non_exhaustive]`
(0.7.0), so `match`es with a wildcard arm keep compiling — but a **literal
constructor** or a **pattern that names every field without `..`** does not.

**Before (0.7.0)**

```rust
let cmd = OptionChainCommand::AddOrder { symbol, order_id, side, price, quantity };

if let OptionChainResult::OrderAdded { order_id } = receipt.result { /* … */ }
```

**After (0.8.0)**

```rust
use option_chain_orderbook::{Hash32, TimeInForce};

let cmd = OptionChainCommand::AddOrder {
    symbol,
    order_id,
    side,
    price,
    quantity,
    tif: TimeInForce::Gtc,      // pre-0.8.0 behavior
    user_id: Hash32::zero(),    // pre-0.8.0 behavior
};

if let OptionChainResult::OrderAdded { order_id, .. } = receipt.result { /* … */ }
```

**What to do:** add the two fields to `AddOrder` literals (`TimeInForce::Gtc`
+ `Hash32::zero()` reproduce the old semantics exactly), and add `..` to
field patterns. Prefer the `submit_add_order` / `submit_add_order_with`
helpers over hand-built command literals — the former is signature-unchanged.

**Wire format:** unaffected in the backward direction **for self-describing
encodings (JSON)**. The new fields carry `#[serde(default)]`, so old JSON
journals decode to `Gtc` / zero-user / no-trade and replay identically
(pinned by the frozen v0.5.0 fixture test). Serde's missing-field default
does not exist in positional codecs: a bincode-style journal cannot detect
an absent trailing field, so pre-0.8.0 *binary* records fail to decode
against the new `AddOrder` / `OrderAdded` shape — re-journal or migrate
them before upgrading. (Appending the `ReplaceOrder` / `OrderReplaced`
*variants* is safe in every codec — earlier variant indices never shift.)
An *old* binary reading a journal produced by 0.8.0 fails loudly on the
unknown fields or the new `ReplaceOrder` variant tag — the established,
intentional asymmetry.

---

## Everything else is additive

- `submit_add_order_with(symbol, order_id, side, price, quantity, tif, user_id)`
  — full-fidelity journaled add (TIF variety + account/STP identity).
- `OrderAdded.trade: Option<TradeResult>` — `Some` iff the add produced
  fills, attributed per-call (never the shared capture slot). Trade IDs,
  timestamps and `engine_seq` inside the payload are not replay-stable until
  OrderBook-rs#199 ships; the replay oracle compares book state and ignores
  journaled results.
- `OptionChainCommand::ReplaceOrder` / `OptionChainResult::OrderReplaced` +
  `submit_replace_order(...)` — atomic validate-first replace through the
  sequencer; the original order survives any rejection; a replace never
  vivifies expirations/strikes.
- `OptionOrderBook::replace_order(order_id, price, quantity, side)` — the
  leaf primitive (queue priority is lost; rematch fills reach the trade
  listener only).
- `OptionOrderBook::take_trade_result()` / `clear_trade_capture()` — atomic
  consuming read / explicit reset for the single-slot trade capture.
- `ValidationConfig::with_max_price(u128)` — crate-side maximum price bound,
  enforced on every add and replace path (the upstream engine has no
  price-bound hook). A finite bound lets venues prove upstream fee
  saturation unreachable.
- Deterministic seams from #147: internal serialization of the sequencer's
  assign→execute→journal-append critical section, `set_clock` /
  `clear_clock` / `clock()` at every hierarchy level (plus
  `SequencedUnderlyingOrderBook::set_clock`, linearized against the command
  stream), and re-exported `Clock` / `MonotonicClock` / `StubClock`.
