# Migrating `option-chain-orderbook` 0.4.4 → 0.5.0

`0.5.0` is a single batch that absorbs every breaking change accumulated since
the published `0.4.4`. The dominant themes are: the `pricelevel` newtypes now
appear at every domain boundary (no more raw `u64`/`u128`); status mutations are
validated against a lifecycle state machine and return `Result`; several
infallible constructors became fallible `try_new`/`build`; the NATS publisher
wiring moved from a post-construction `connect_nats` to pre-`Arc` builder
functions; and `Error` is now `#[non_exhaustive]`. Most call sites need a `?`,
a wrapped/unwrapped newtype, or a wildcard `match` arm. Work through the
sections below; the closing checklist summarizes every step.

---

## `Error` is now `#[non_exhaustive]`, and five aspirational variants were removed

**Before (0.4.4)**

```rust
#[derive(Error, Debug)]
pub enum Error { /* exhaustively matchable, no #[non_exhaustive] */ }

// removed variants + constructors:
Error::inventory_limit_exceeded(limit_type, limit /* Decimal */, current /* Decimal */);
Error::risk_limit_breached(limit_type);
Error::hedging(message);
Error::market_data(message);
Error::adapter(exchange, message);
```

**After (0.5.0)**

```rust
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error { /* match now requires a wildcard arm */ }
// InventoryLimitExceeded / RiskLimitBreached / HedgingError /
// MarketDataError / AdapterError and their constructors are gone.
```

**What to do:** add a catch-all `_ => { … }` arm to every `match` over an
`Error` value. Stop referencing the removed variants/constructors; for analogous
failures use the retained `Error::validation` / `Error::configuration` /
`Error::pricing` / `Error::no_data`, or move risk/inventory/hedging/market-data/
adapter concerns into your own error type. (Existing variants are still
constructible and matchable by name; only the exhaustiveness requirement
changed.)

---

## `Quote` switched to the `pricelevel` newtypes

**Before (0.4.4)**

```rust
Quote::new(
    bid_price: Option<u128>, bid_size: u64,
    ask_price: Option<u128>, ask_size: u64,
    timestamp_ms: u64,
) -> Quote;
// getters: bid_price()/ask_price() -> Option<u128>;
//          bid_size()/ask_size() -> u64; timestamp_ms() -> u64
```

**After (0.5.0)**

```rust
Quote::new(
    bid_price: Option<Price>, bid_size: Quantity,
    ask_price: Option<Price>, ask_size: Quantity,
    timestamp_ms: TimestampMs,
) -> Quote; // now const
// getters: bid_price()/ask_price() -> Option<Price>;
//          bid_size()/ask_size() -> Quantity; timestamp_ms() -> TimestampMs
```

**What to do:** wrap raw inputs with `Price::new(p)` / `Quantity::new(q)` /
`TimestampMs::new(ms)` (all re-exported from the crate root), and where you read
a getter unwrap with `Price::as_u128()` / `Quantity::as_u64()` /
`TimestampMs::as_u64()`. `spread()` still returns `Option<u128>`. **No serde
change** — the newtypes are `#[serde(transparent)]`, so the JSON stays bare
numbers.

---

## `Quote` no longer has an `id`

**Before (0.4.4)** — JSON object included `id`:

```json
{ "bid_price": 100, "bid_size": 5, "ask_price": 101, "ask_size": 5, "id": 42, "timestamp_ms": 1719446400000 }
```

**After (0.5.0)** — `id` is gone and the `id()` accessor is removed:

```json
{ "bid_price": 100, "bid_size": 5, "ask_price": 101, "ask_size": 5, "timestamp_ms": 1719446400000 }
```

**What to do:** stop calling `quote.id()`; maintain your own identifier if you
need one. Old JSON that still contains `id` continues to deserialize (the extra
field is ignored — there is no `deny_unknown_fields`), but **newly serialized
Quotes omit `id`**, so update any downstream consumer/schema that required it.
`Quote`'s `PartialEq` now excludes only `timestamp_ms` (it already ignored
market timestamp).

---

## `OptionOrderBook` status mutations are fallible and lifecycle-validated

**Before (0.4.4)**

```rust
pub fn set_status(&self, status: InstrumentStatus); // -> ()
pub fn halt(&self);   // -> ()
pub fn resume(&self); // -> ()
pub fn expire(&self) -> Vec<OrderId>;
```

**After (0.5.0)**

```rust
pub fn set_status(&self, status: InstrumentStatus) -> Result<()>;
pub fn halt(&self)   -> Result<()>;
pub fn resume(&self) -> Result<()>;
pub fn expire(&self) -> Result<Vec<OrderId>>;
```

**What to do:** handle the `Result` (`?`, `.expect`, or `match`). Newly rejected
edges return `Error::IllegalStatusTransition` and leave the status unchanged:
resuming/halting a `Settling` or `Expired` book, or any move back to `Pending`.
Legal forward edges, the operator `Active <-> Halted` resume, and self-transitions
(`X -> X`, a legal no-op) still succeed. Two related behavior changes (signatures
otherwise unchanged):

- `compare_and_set_status` now returns `false` (no swap) when `expected -> new`
  is not a legal edge, even if `expected` matches the current status. The
  expiry-lifecycle forward edges (`{Pending,Active,Halted} -> Settling` and
  `{Pending,Active,Halted,Settling} -> Expired`) are all still accepted.
- `expire()`'s returned `Vec<OrderId>` is now in engine cancel/processing order,
  not `get_all_orders()` order — do not depend on a particular ordering. Dropped
  orders now reach the terminal `Cancelled` state.

---

## `OptionOrderBook` cached-quote API removed

**Before (0.4.4)**

```rust
pub fn update_last_quote(&mut self) -> bool;
pub fn last_quote(&self) -> &Quote;
pub fn last_quote_arc(&self) -> Arc<Quote>;
```

**After (0.5.0)** — all three removed.

**What to do:** call `best_quote()` and compare against your own previously
stored `Quote` to detect changes (`Quote`'s `PartialEq` compares market data
only, ignoring the timestamp). Note `update_last_quote` was the only `&mut self`
method on `OptionOrderBook`; the leaf is now used purely through shared
references. As a bonus, `best_quote()` is now an allocation-free bounded
top-of-book read (~11.75µs → ~69ns) with an unchanged return value.

---

## `last_trade_result()` is disarmed by default

**Before (0.4.4):** every match recorded a `TradeResult` that
`last_trade_result()` could poll afterwards.

**After (0.5.0):** on the plain order path `last_trade_result()` returns `None`
unless `arm_trade_capture(true)` was called first; the `*_full` order methods
still capture and return their own trade regardless.

**What to do:** call `arm_trade_capture(true)` before polling
`last_trade_result()` on the plain path, or switch to the `*_full` methods'
return value, the NATS trade publisher, or the order-state tracker. Polling
remains a single-slot, last-write-wins read — not a per-fill feed.

---

## `StrikeOrderBook::call_greeks` / `put_greeks` return owned values

**Before (0.4.4)**

```rust
pub const fn call_greeks(&self) -> Option<&Greek>;
pub const fn put_greeks(&self) -> Option<&Greek>;
```

**After (0.5.0)**

```rust
pub fn call_greeks(&self) -> Option<Greek>;
pub fn put_greeks(&self) -> Option<Greek>; // no longer const
```

**What to do:** bind the owned value (`let g = strike.call_greeks();`), drop any
`&`/deref on the result, and change pattern bindings from `Some(g)` where
`g: &Greek` to `g: Greek` (`Greek` is `Clone`/`Copy`-cheap). The matching setters
`update_call_greeks`/`update_put_greeks` now take `&self` (strictly more
permissive — existing callers still compile).

---

## Manager `iter()` yields owned tuples instead of `Entry` handles

**Before (0.4.4)**

```rust
pub fn iter(&self)
    -> impl Iterator<Item = crossbeam_skiplist::map::Entry<'_, ExpirationDate, Arc<OptionChainOrderBook>>>;
// (and the analogous Entry<'_, ExpirationDate, Arc<ExpirationOrderBook>>)
```

**After (0.5.0)**

```rust
pub fn iter(&self)
    -> impl Iterator<Item = (ExpirationDate, Arc<OptionChainOrderBook>)> + '_;
// (and (ExpirationDate, Arc<ExpirationOrderBook>) for the expiration manager)
```

**What to do:** destructure the yielded tuple `(exp, book)` instead of calling
`entry.key()` / `entry.value()`. Iteration is now deterministically ordered by a
collision-free internal expiration key (replay-safe).

---

## `books_affected()` now counts leaf contract books

**Before (0.4.4):** counted affected *direct children* (strikes, chains,
expirations, or underlyings, depending on level).

**After (0.5.0):** drills down and sums each child's leaf option-book count, so
the unit is a leaf call/put contract book at every level. A chain spanning N
strikes with both legs touched now reports `2N`, not `N`; the global total is
the sum of all per-leaf counts.

**What to do:** re-interpret the returned number as a leaf contract-book count
(now consistent across `Chain`/`Expiration`/`Underlying`/`Global` mass-cancel
results). If you need the old per-child count, derive it from the `per_child`
entries on the result.

---

## `ContractSpecsBuilder::build` is fallible

**Before (0.4.0)**

```rust
#[must_use] pub fn build(self) -> ContractSpecs;
```

**After (0.5.0)**

```rust
pub fn build(self) -> Result<ContractSpecs>; // Error::ConfigurationError on bad specs
```

**What to do:** append `?` or `.expect("valid contract specs")` to `.build()`
calls. It rejects zero `tick_size`/`lot_size`/`contract_size`/`min_order_size`
and an inverted `[min,max]` order-size window. Structurally-valid specs are
unaffected. (`ValidationConfig::validate` / `ContractSpecs::validate` are new and
additive — the individual `with_*` setters remain infallible.)

---

## `InstrumentRegistry::allocate` is fallible

**Before (0.4.4)**

```rust
pub fn allocate(&self) -> u32;
```

**After (0.5.0)**

```rust
pub fn allocate(&self) -> Result<u32>; // Error::InstrumentIdExhausted at u32::MAX
```

**What to do:** handle the `Result` (`?`/`match`/`.expect`). Exhaustion requires
~4 billion allocations and is astronomically unlikely; the hierarchy's own
`get_or_create` degrades gracefully (leaves the call/put pair at `instrument_id`
0 and logs) rather than propagating the error.

---

## `ParsedSymbol` fields are private; construct via `try_new`

**Before (0.4.4)**

```rust
pub struct ParsedSymbol {
    pub underlying: String,
    pub expiration: ExpirationDate,
    pub expiration_str: String,
    pub strike: u64,
    pub option_style: OptionStyle,
}
let p = ParsedSymbol { underlying, expiration, expiration_str, strike, option_style };
let u: String = p.underlying;
```

**After (0.5.0)**

```rust
let p = ParsedSymbol::try_new(underlying, expiration_str, strike, option_style)?;
let u: &str = p.underlying();
let e: &ExpirationDate = p.expiration();
let es: &str = p.expiration_str();
let s: u64 = p.strike();
let style: OptionStyle = p.option_style();
```

**What to do:** replace field reads (`p.underlying`, `p.expiration`, …) with the
accessor calls, noting the `String`/`ExpirationDate` fields now hand back
`&str`/`&ExpirationDate`. Replace any `ParsedSymbol { .. }` struct literal with
`ParsedSymbol::try_new(..)?`, which validates a non-empty underlying, a positive
strike, and a valid `YYYYMMDD` expiration. As a convenience, `SymbolParser::parse`
now also accepts lowercase `c`/`p` suffixes.

---

## `GreeksEngine::subscribe` returns a `SubscriptionId`

**Before (0.4.4)**

```rust
pub fn subscribe(&self, listener: GreeksUpdateListener); // -> ()
```

**After (0.5.0)**

```rust
pub fn subscribe(&self, listener: GreeksUpdateListener) -> SubscriptionId;
```

**What to do:** statement-style calls (`engine.subscribe(listener);`) keep
compiling unchanged — the id is simply dropped. Only fix sites that depend on the
old unit return: an explicit `let _: () = engine.subscribe(..)` binding, or a
coercion to a `fn(&GreeksEngine, GreeksUpdateListener)` function pointer. Capture
the returned `SubscriptionId` if you intend to call the new
`unsubscribe(id) -> bool`.

---

## `GreeksEngine` calculation input validation tightened

**Before (0.4.4):** `risk_free_rate` was not validated for finiteness; a failed
`f64 -> Decimal` conversion silently fell back to `dec!(0.05)` (5%);
`dividend_yield` was clamped via `.max(0.0001)`, so a `0.0` dividend was priced
as `0.0001`. NaN/Inf rates and NaN/Inf/negative dividends were accepted.

**After (0.5.0):** `calculate_greeks` / `calculate_strike_greeks` return
`Err(Error::GreeksError)` when `risk_free_rate` is non-finite (negative is still
allowed), when `dividend_yield` is non-finite or negative, or when the validated
rate cannot convert to `Decimal` (no 5% fallback). A `0.0` dividend is priced as
a true `0.0`.

**What to do:** stop passing NaN/±Inf risk-free rates or NaN/Inf/negative
dividend yields — these now return `Err`; handle the `Result`. **Expect
numerically different Greeks for any call that passed `dividend_yield = 0.0`**
(now a true zero, previously `0.0001`) — this affects typical crypto options.
Do not rely on the old silent 5%-rate fallback; pass a valid finite rate.

---

## `MockPriceFeed` delivery is now asynchronous and keep-latest

**Before (0.4.4):** `set_price` stored the price/timestamp, cloned the listener
list, dropped the lock, then called every listener synchronously before
returning. Every `set_price` delivered every update; no background threads.

**After (0.5.0):** `set_price` is non-blocking — it updates `latest_price()`
synchronously, writes each subscriber's latest-value mailbox, and wakes a
dedicated background drain thread. Delivery is async and keep-latest: if a
listener lags, older un-consumed updates are coalesced/dropped (counted, with a
throttled `WARN`). `subscribe` spawns a drain thread; `unsubscribe`/`Drop` stop
and join it. `wire_feed_to_calculator` therefore updates `calc.index_price()`
asynchronously.

**What to do:** do **not** assume listeners (or a wired `MarkPriceCalculator`)
have observed an update when `set_price` returns — poll/wait for the side effect:

```rust
feed.set_price(/* … */);
// was: assert_eq!(calc.index_price(), expected); // now racy
// now: poll until observed, with a timeout
let deadline = Instant::now() + Duration::from_secs(1);
while calc.index_price() != expected && Instant::now() < deadline {
    std::thread::yield_now();
}
assert_eq!(calc.index_price(), expected);
```

Tolerate gaps (only the freshest value is guaranteed under back-to-back updates —
do not count exact delivery totals), account for one background thread per
subscriber, and keep listeners panic-free (a panic aborts that subscriber's
drain thread).

---

## NATS publisher wiring moved to pre-`Arc` builders (feature `nats`)

**Before (0.4.4)**

```rust
// re-exported as `pub use book::NatsPublisherHandles;`
pub struct NatsPublisherHandles {
    pub trade_handle: Arc<NatsTradePublisher>,
    pub trade_listener: TradeListener,
    pub book_handle: Arc<NatsBookChangePublisher>,
    pub book_listener: PriceLevelChangedListener,
}

let handles = book.connect_nats(&config)?;          // OptionOrderBook
let n = chain.nats_handle_count();                   // OptionChainOrderBook
let cfg = OptionChainNatsConfig::new(jetstream, subject_prefix /* String */, runtime);
let b = OptionChainSubjectBuilder::new(underlying, expiry, strike, option_type);
```

**After (0.5.0)**

```rust
// re-exported as `pub use nats::NatsPublisherHandles;` (crate-root path unchanged)
pub struct NatsPublisherHandles {
    pub trade_handle: Arc<NatsTradePublisher>,
    pub book_handle: Arc<NatsBookChangePublisher>,
}
impl NatsPublisherHandles {
    pub fn trade_publish_count(&self) -> u64; pub fn trade_error_count(&self) -> u64;
    pub fn book_publish_count(&self)  -> u64; pub fn book_error_count(&self)  -> u64;
    pub async fn shutdown(&self);
}

let (book, handles) = build_option_order_book_with_nats(symbol, option_style, &config)?;
let manager = build_underlying_manager_with_nats(config); // per-contract publishers, lazily attached
let cfg = OptionChainNatsConfig::try_new(jetstream, subject_prefix /* impl Into<String> */, runtime)?;
let b = OptionChainSubjectBuilder::try_new(underlying, expiry, strike, option_type)?;
```

**What to do:**

- Stop calling `connect_nats` after construction (the old method could not
  actually attach listeners — the inner `OrderBook` was already `Arc`-wrapped)
  and stop calling `OptionChainOrderBook::nats_handle_count`. Build NATS-enabled
  books/managers through `build_option_order_book_with_nats` (returns
  `(OptionOrderBook, NatsPublisherHandles)`) or `build_underlying_manager_with_nats`
  so publishers attach at construction.
- Stop reading `.trade_listener` / `.book_listener` (removed). Use the new count
  methods for metrics and `shutdown().await` for teardown of the standalone book
  (lazily-created books under the manager are drop-driven).
- Rename `OptionChainNatsConfig::new` → `try_new` and
  `OptionChainSubjectBuilder::new` → `try_new` and handle the `Result`. An empty
  prefix/component, a leading/trailing/doubled `.`, or a `*`/`>` now yields
  `Error::NatsSubject`. `option_type` for the subject builder must be
  `C`/`Call`/`P`/`Put` (case-insensitive) and is normalized to the canonical
  `C`/`P` token (`option_type()` now returns `&'static str`; `option_style()` is
  a new typed accessor).
- `OptionChainSubjectBuilder::from_symbol` now parses through `SymbolParser`, so
  symbols must match `{UNDERLYING}-{YYYYMMDD}-{STRIKE}-{C|P}`. Non-date expiries
  and non-integer/zero strikes are rejected; a zero-padded strike (`050000`) is
  normalized to `50000` in the subject. Match on `Error::InvalidSymbol` vs
  `Error::NatsSubject` accordingly.

---

## `ExpiryCycleConfig` time fields are now `chrono::NaiveTime` (serde wire change)

**Before (0.4.4)**

```rust
pub expiry_time_utc: (u32, u32),
pub settlement_time_utc: (u32, u32),
```

```json
{ "expiry_time_utc": [8, 0], "settlement_time_utc": [8, 30] }
```

**After (0.5.0)**

```rust
pub expiry_time_utc: chrono::NaiveTime,
pub settlement_time_utc: chrono::NaiveTime,
```

```json
{ "expiry_time_utc": "08:00:00", "settlement_time_utc": "08:30:00" }
```

**What to do:** construct with `NaiveTime::from_hms_opt(8, 0, 0).unwrap()` instead
of `(8, 0)`. **Persisted/transmitted 0.4.4 configs written with the
`[hour, minute]` array no longer deserialize — re-encode them as `"HH:MM:SS"`
strings.** Out-of-range hours/minutes are now unrepresentable, so `validate()` no
longer returns the old "not a valid 24-hour time" error. Separately, `validate()`
now rejects any `CycleRule.count` greater than the new
`ExpiryCycleConfig::MAX_CYCLE_COUNT` (512) — keep every count in `1..=512`. The
`settlement >= expiry` check is retained.

---

## Journal types now `deny_unknown_fields` (feature `sequencer`, serde)

**Before (0.4.4)**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OptionChainCommand { /* … */ } // unknown fields silently ignored
```

**After (0.5.0)**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase", rename_all_fields = "snake_case")]
pub enum OptionChainCommand { /* … */ } // unknown fields now hard-error
```

`MassCancelScope`, `MassCancelType`, `OptionChainResult`, and `OptionChainEvent`
gained the same guard (`OptionChainEvent` uses `deny_unknown_fields` +
`rename_all = "snake_case"`).

**What to do:** stop attaching extra/extension fields to journal JSON — any field
not in the schema now fails deserialization. The on-the-wire variant-tag strings
(PascalCase) and field names (snake_case) are unchanged, so existing 0.4.4
journals **without** extra fields still decode.

---

## New sequencer enum variants break exhaustive matches (feature `sequencer`)

**Before (0.4.4)**

```rust
match cmd {
    OptionChainCommand::AddOrder { .. }    => { /* … */ }
    OptionChainCommand::CancelOrder { .. } => { /* … */ }
    OptionChainCommand::MassCancel { .. }  => { /* … */ }
} // exhaustive, compiles
```

**After (0.5.0)**

```rust
match cmd {
    OptionChainCommand::AddOrder { .. }            => { /* … */ }
    OptionChainCommand::CancelOrder { .. }         => { /* … */ }
    OptionChainCommand::MassCancel { .. }          => { /* … */ }
    OptionChainCommand::SetInstrumentStatus { .. } => { /* … */ }
    _ => { /* future-proof */ }
}
```

**What to do:** add arms for `OptionChainCommand::SetInstrumentStatus` and
`OptionChainResult::StatusChanged` (or a `_ =>` wildcard) to any exhaustive match
over these enums — neither is `#[non_exhaustive]`.

---

## `submit_add_order` now vivifies unlisted contracts (feature `sequencer`)

**Before (0.4.4)**

```rust
// fresh book, contract not yet listed:
submit_add_order("BTC-20240329-50000-C", /* … */); // -> OptionChainResult::BookNotFound
```

**After (0.5.0)**

```rust
submit_add_order("BTC-20240329-50000-C", /* … */);
// -> materializes the expiration + strike via get_or_create_*, rests the order
// -> OptionChainResult::OrderAdded
// Only an unparseable symbol -> BookNotFound; a cross-underlying symbol -> Rejected.
```

**What to do:** do not rely on `BookNotFound` to detect unlisted contracts —
`AddOrder` now auto-creates the book (this is what lets journal replay rebuild
`AddOrder` state). Pre-validate/whitelist contracts before submitting if you need
to reject unknown strikes/expiries.

---

## New capabilities you may want to adopt

- **Crate-root re-exports.** Every `orderbook::` item now also resolves at the
  crate root, and the boundary newtypes `OrderId`, `OrderType`, `Side`,
  `TimeInForce`, `Price`, `Quantity`, `TimestampMs` are re-exported from this
  crate — you can drop direct `orderbook_rs` / `pricelevel` dependencies used
  only to name those types. (#109, #125)
- **Mark-price read/tick split.** Use `MarkPriceCalculator::current_mark_price()`
  to read the last committed mark without advancing dampening, and `advance_mark()`
  to tick exactly once per cycle. (#99)
- **Greeks listener removal.** `GreeksEngine::unsubscribe(id)` removes a listener
  registered via `subscribe`, with deterministic id-order notification preserved. (#133)
- **NATS metrics + teardown.** `NatsPublisherHandles` now exposes publish/error
  counts and an async `shutdown()`. (#120)
- **Replayable status transitions.** `submit_set_instrument_status` journals
  lifecycle changes, and `replay()` reconstructs them. (#114)
- **Race-free creation.** `get_or_create_inserted` /
  `get_or_create_expiration_inserted` return a `(handle, won_insert)` pair for
  exactly-once side effects. (#115)
- **Cheap leaf checks.** `OptionOrderBook::has_both_sides()` and
  `arm_trade_capture(true)` for opt-in continuous trade capture. (#124)
- **Pre-flight validation.** `InstrumentStatus::can_transition`,
  `ValidationConfig::validate`, and `ContractSpecs::validate`. (#112, #110)

### Deprecation to schedule

`MarkPriceCalculator::mark_price` is now `#[deprecated(since = "0.5.0")]` and
delegates to `advance_mark()` (identical runtime behavior). Migrate reads to
`current_mark_price()` and ticks to `advance_mark()`. Heads-up: builds with
`deny(deprecated)` or `deny(warnings)` will fail to compile until callers
migrate off `mark_price()`.

---

## Migration checklist

- [ ] Add a `_ =>` wildcard arm to every `match` over `Error`; remove references
      to the five deleted `Error` variants/constructors.
- [ ] Wrap `Quote` inputs in `Price`/`Quantity`/`TimestampMs`; unwrap getters
      with `as_u128`/`as_u64`.
- [ ] Stop calling `Quote::id()`; drop `id` from any schema that required it.
- [ ] `?`-propagate `set_status`/`halt`/`resume`/`expire`; review newly-rejected
      lifecycle edges; stop assuming `expire()`'s Vec ordering.
- [ ] Replace `update_last_quote`/`last_quote`/`last_quote_arc` with `best_quote()`
      + your own diff.
- [ ] Call `arm_trade_capture(true)` before polling `last_trade_result()` on the
      plain path (or switch to `*_full` / publisher / tracker).
- [ ] Bind `call_greeks`/`put_greeks` as owned `Greek`; drop `&`/deref.
- [ ] Destructure manager `iter()` tuples `(exp, book)`.
- [ ] Re-interpret `books_affected()` as a leaf contract-book count.
- [ ] Add `?`/`.expect` to `ContractSpecsBuilder::build` and
      `InstrumentRegistry::allocate`.
- [ ] Replace `ParsedSymbol` field reads/literals with accessors and
      `try_new(..)?`.
- [ ] Capture or ignore the `SubscriptionId` from `GreeksEngine::subscribe`; fix
      only unit-return-dependent sites.
- [ ] Stop passing non-finite rates / non-finite-or-negative dividends to the
      Greeks engine; re-baseline Greeks for `dividend_yield == 0.0`.
- [ ] Replace immediate-observe assertions after `MockPriceFeed::set_price` with
      poll-until-with-timeout; ensure listeners are panic-free.
- [ ] Replace `connect_nats`/`nats_handle_count` with the NATS builder functions;
      stop reading `.trade_listener`/`.book_listener`; rename NATS `new` →
      `try_new`; ensure symbols are `{UNDERLYING}-{YYYYMMDD}-{STRIKE}-{C|P}`.
- [ ] Migrate `ExpiryCycleConfig` time fields to `NaiveTime`; re-encode persisted
      configs to `"HH:MM:SS"`; keep `CycleRule.count` in `1..=512`.
- [ ] Remove extra fields from journal JSON (`deny_unknown_fields`).
- [ ] Add `SetInstrumentStatus` / `StatusChanged` arms (or a wildcard) to
      exhaustive sequencer matches.
- [ ] Stop using `BookNotFound` to detect unlisted contracts; pre-validate
      contracts if needed.
- [ ] Plan migration off the deprecated `MarkPriceCalculator::mark_price`
      (especially under `deny(warnings)`).
