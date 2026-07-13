# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Pre-1.0 note:** while the crate is below `1.0.0`, a minor version bump
> (`0.x.0`) may carry breaking changes. `0.5.0` is the first release with a
> changelog and absorbs every breaking change accumulated since the published
> `0.4.4`. A full upgrade walkthrough lives in
> [`MIGRATING-0.5.0.md`](./MIGRATING-0.5.0.md).

## [Unreleased]

### Added

- **Per-contract price banding on `ContractSpecs` (#152).** `ContractSpecs` and
  `ValidationConfig` gained an inclusive absolute `[min_price, max_price]` band
  (`Option<u128>` each, smallest price units). The engine has no price-bound
  hook, so the band is enforced crate-side on every add and replace, after the
  active check. When both a `ContractSpecs` band and a validation band apply to
  the same contract they merge tightest-wins (`ValidationConfig::tightened_price_band`).
  The `ContractSpecs` band fields carry `#[serde(default, skip_serializing_if)]`,
  so band-free specs keep their pre-0.8.0 wire shape; specs that DO carry a band
  are unreadable by 0.8.0 consumers (documented asymmetry).
- **Order-kind variety through the sequenced path (#151).** New
  `OrderKind` enum (`Limit` / `PostOnly` / `Iceberg`, `#[non_exhaustive]`),
  re-exported from the crate root. `OptionChainCommand::AddOrder` gained
  `kind: OrderKind` and `hidden_quantity: Option<u64>` (both `#[serde(default)]`,
  always emitted). New `SequencedUnderlyingOrderBook::submit_add_order_kind`
  wrapper carries the kind and hidden reserve into the journal so replay
  reconstructs the exact order shape; `submit_add_order` / `submit_add_order_with`
  now delegate through it (their 0.8.0 signatures are unchanged). For an iceberg,
  `quantity` is the visible tranche and `hidden_quantity` the reserve behind it;
  an iceberg with a missing/zero hidden reserve, or a limit/post-only add with a
  hidden reserve, is a deterministic `Rejected`. A post-only add that would cross
  is journaled as a deterministic `PriceCrossing` rejection.
- **Deterministic trade-ID namespace + replace fills (#153).** The upstream
  namespace seam (orderbook-rs 0.11, upstream #199/#200) is now threaded end to
  end: `SequencedUnderlyingOrderBook::set_trade_id_namespace(root)` propagates a
  root namespace down the hierarchy, and each leaf derives its own
  `UUIDv5(root, symbol)` at construction (call and put get distinct namespaces).
  With a root namespace and an injected clock (`set_clock`) both set before the
  first submit, two identically-seeded sequenced runs on fresh instances produce
  identical trade payloads — trade ids, engine trade timestamps, and `engine_seq`
  included (the sequencer's wall-clock event-envelope `timestamp_ns` is the one
  exception). `uuid` is now a direct dependency and `Uuid` is re-exported from
  the crate root. New leaf `OptionOrderBook::replace_order_full` recovers a
  replacement's on-entry fills via a dedicated capture slot (taker-id filtered,
  missing-never-wrong); `OptionChainResult::OrderReplaced` gained
  `trade: Option<TradeResult>` (`#[serde(default)]`, always emitted), and
  `execute_replace_order` now carries the replacement's fills.

### Changed

- **Breaking for co-pinners: `orderbook-rs` public dependency `0.10` → `0.11`.**
  `orderbook-rs` is a public dependency, so this pin move is breaking for
  downstream crates that co-pin it (same precedent as the 0.6.0 `0.9` → `0.10`
  bump). The only breaking changes in orderbook-rs 0.11 are in
  `ReplayBookConfig` / `ReplayError`, which this crate does not use, so the
  hierarchy and sequencer surface is otherwise unchanged.

### Wire compatibility

- The new `AddOrder` fields (`kind` / `hidden_quantity`) and the
  `OrderReplaced.trade` field are backward-compatible **for self-describing
  encodings (JSON)**: they carry `#[serde(default)]`, so a pre-#151/#153 JSON
  journal decodes to a plain limit add (`OrderKind::Limit`, no hidden reserve)
  and a trade-less `OrderReplaced` respectively, and replays identically (pinned
  by the frozen v0.5.0 and v0.8.0 fixture tests, which patch these defaults in).
  Positional codecs such as bincode cannot apply a missing-field default, so
  pre-#151/#153 *binary* journal records must be re-journaled or migrated. An
  unknown `OrderKind` variant tag written by a newer binary is rejected by an
  older one — the same additive forward-compat asymmetry as the command/result
  enums. A journal carrying `trade` on `OrderReplaced` is undecodable by a 0.8.0
  binary that predates the field (documented asymmetry).

## [0.8.0] - 2026-07-12

### ⚠️ Breaking Changes

- **`OptionChainCommand::AddOrder` gained `tif: TimeInForce` + `user_id: Hash32`;
  `OptionChainResult::OrderAdded` gained `trade: Option<TradeResult>`.** Both
  enums were already `#[non_exhaustive]`, so wildcard `match`es keep compiling,
  but literal constructors and full field patterns need the new fields (or
  `..`). `TimeInForce::Gtc` + `Hash32::zero()` reproduce the old semantics
  exactly. The journal wire format is backward-compatible **for self-describing
  encodings (JSON)**: the fields carry `#[serde(default)]`, so every pre-0.8.0
  JSON journal decodes and replays identically (pinned by the frozen v0.5.0
  fixture test). Positional codecs such as bincode cannot apply a
  missing-field default, so pre-0.8.0 *binary* journal records do not decode
  against the new shape — re-journal or migrate them. (Appending the new
  `ReplaceOrder` / `OrderReplaced` *variants* is bincode-safe either way:
  earlier variant indices never shift.) Walkthrough:
  [`MIGRATING-0.8.0.md`](./MIGRATING-0.8.0.md). (#148)

### Added

- **Deterministic venue seams (#147).**
  - `SequencedUnderlyingOrderBook::submit` now serializes the whole
    assign→execute→journal-append sequence in one critical section: journal
    insertion order == sequence order == book-mutation order. `replay` takes
    the same gate, so a rebuild cannot interleave with live submits.
  - An injectable engine clock threads from every manager level into lazily
    vivified leaf books (`set_clock` / `clear_clock` / `clock()` on managers
    and books; `SequencedUnderlyingOrderBook::set_clock` is linearized against
    the command stream). `Clock`, `MonotonicClock`, `StubClock` re-exported.
    GTD/Day admission becomes replay-deterministic under an injected clock.
  - Known limitation documented: trade IDs are not replay-stable — upstream
    `orderbook-rs` mints each book's trade-ID namespace with a random
    `Uuid::new_v4()` (tracked as OrderBook-rs#199); the replay oracle compares
    book state and excludes trade IDs.
- **Lossless venue integration (#148).**
  - `OrderAdded` carries the fills: `trade: Option<TradeResult>`, `Some` iff
    the add crossed, attributed per-call (never the shared capture slot). A
    journaled `Rejected` after real fills (unfillable IOC remainder, STP
    taker-cancel) is deterministic and documented; those fills stay
    listener-visible.
  - `submit_add_order_with(...)` journals TIF variety and account/STP
    identity; `submit_add_order` delegates unchanged (Gtc + zero user).
  - `OptionChainCommand::ReplaceOrder` / `OptionChainResult::OrderReplaced` +
    `submit_replace_order(...)`: atomic validate-first replace through the
    sequencer, resolved via the non-creating lookup (a replace never vivifies
    expirations/strikes); the original order survives any rejection.
  - `OptionOrderBook::replace_order(order_id, price, quantity, side)` — leaf
    primitive over the engine's validate-first `OrderUpdate::Replace` (queue
    priority lost; rematch fills reach the trade listener only).
  - `OptionOrderBook::take_trade_result()` / `clear_trade_capture()` — atomic
    consuming read and explicit reset for the single-slot trade capture.
  - `ValidationConfig::with_max_price(u128)` — crate-side maximum price bound
    enforced on every add and replace path (the upstream engine has no
    price-bound hook); `validation_config()` readback merges the leaf-held
    bound. A finite bound lets venues prove upstream fee saturation
    unreachable (`FeeSchedule::max_guaranteed_exact_notional_for_bps`,
    orderbook-rs ≥ 0.10.4).
- New strict wire fixture `tests/fixtures/journal_event_v0.8.0.json` freezing
  the current schema (deterministic `Some(trade)` payload built via serde, no
  wall-clock/random-id construction); wire pins for the enriched `AddOrder`,
  `OrderAdded`-with-trade and `ReplaceOrder`, deny-unknown-fields probes, and
  an old-binary-rejects-new-variant simulation.

### Tests

Concurrent-load journal-order and replay-equals-live oracles under the new
gate; GTD admission via injected `StubClock` at leaf/strike/underlying levels;
replay-equals-live under an injected clock; crossing/IOC/nonzero-user adds,
replace-that-rematches and unknown-id replace pinned by the full-state replay
oracle; max-price enforcement across all eight add paths plus replace;
capture take/clear including poisoned-lock recovery.

## [0.7.0] - 2026-07-11

### ⚠️ Breaking Changes

- **`OptionChainCommand` and `OptionChainResult` are now `#[non_exhaustive]`.**
  Every downstream `match` over either enum must add a wildcard (`_ =>`) arm.
  This is a one-time source break: future command/result variants are additive
  and source-compatible once the wildcard arm is present, matching the precedent
  set by `orderbook-rs` 0.10's `SequencerCommand` / `SequencerResult`. The
  attribute is Rust-source only — it does not change the serde/journal wire
  format. `OptionChainEvent` and `OptionChainReceipt` are structs and are left
  exhaustive. A full walkthrough lives in
  [`MIGRATING-0.7.0.md`](./MIGRATING-0.7.0.md). (#144)

### Added

- **`OptionChainCommand::EvictExpiredOrders { now_ms }`** journals a host-driven
  GTD/DAY expiry sweep across the whole underlying at the wrapper's sequencer
  layer — the piece #141 could not add under 0.6.1's additive-only gate. It is
  submitted via `SequencedUnderlyingOrderBook::submit_evict_expired_orders(now_ms)`,
  ferries through `UnderlyingOrderBook::evict_expired_orders` (the #141 surface)
  on both live execution and replay, and re-applies the journaled `now_ms` (never
  the replay clock) so replay reproduces the exact evictions; the sweep is
  idempotent. The variant is appended after every prior one, so bincode variant
  indices are unaffected and existing journals replay unchanged. (#144)
- **`OptionChainResult::ExpiredEvicted { evicted_ids }`** reports the sweep's
  evicted order ids flattened in the hierarchy's deterministic order (expirations
  by key, strikes ascending, call book before put, and within each leaf the
  engine's eviction order: bids then asks, ascending price, oldest first within a
  level) — the id-centric shape of the leaf
  `OptionOrderBook::evict_expired_orders`. An empty list is a successful no-op
  sweep. (#144)
- Replay coverage for the sweep: a journaled add / `EvictExpiredOrders` / add
  session replays into a fresh instance with byte-identical final state, and the
  `ExpiredEvicted` payload round-trips byte-identically. (#144)

### Journal forward-compatibility (`deny_unknown_fields`)

- The journal enums keep `#[serde(deny_unknown_fields, …)]`. Adding the appended
  variant makes forward-compat **asymmetric**, which is the intended and
  documented behavior:
  - **New binary reads old journal → OK.** Journals written before this release
    carry only known tags and still decode (pinned by the frozen
    `journal_event_v0.5.0.json` fixture test, which predates the new variant).
  - **Old binary reads new journal → hard decode error.** A journal carrying
    `EvictExpiredOrders` / `ExpiredEvicted` fails to decode against a binary that
    predates the variant. `serde` rejects an unknown *variant tag* independently
    of `deny_unknown_fields` (which governs unknown *fields within a known
    variant*), so the failure is guaranteed and loud rather than a silent replay
    corruption. This matches the `orderbook-rs` `SequencerCommand` precedent.
  - `#[non_exhaustive]` does not weaken either guarantee; it is a source-level
    attribute with no effect on the wire format. New journal-format pinning tests
    cover the new variant's exact wire shape, the unknown-field rejection, and
    the unknown-variant-tag rejection.

## [0.6.1] - 2026-07-11

### Added

- Host-driven GTD/DAY expiry sweep surfaced through the whole hierarchy, wrapping
  `orderbook-rs` 0.10's `OrderBook::evict_expired_orders(now_ms)`. `now_ms` is a
  caller-supplied Unix-milliseconds cutoff (the sweep reads no clock, so it is a
  pure function of `now_ms` and the resting book and replays identically), the
  boundary is inclusive (an order whose deadline equals `now_ms` is evicted), and
  expiry is realized only when the sweep runs — an order past its deadline that
  has not yet been swept still rests and remains matchable until the next call.
  - `OptionOrderBook::evict_expired_orders(now_ms) -> Vec<OrderId>` on the leaf
    book, returning the evicted ids in the engine's deterministic order (bids then
    asks, ascending price, oldest first within a level) — matching the id-centric
    shape of `expire()` and the mass-cancel pass-throughs.
  - Aggregated variants mirroring the `*MassCancelResult` pattern:
    `StrikeOrderBook::evict_expired_orders`,
    `OptionChainOrderBook::evict_expired_orders`,
    `ExpirationOrderBook::evict_expired_orders`,
    `UnderlyingOrderBook::evict_expired_orders`, and
    `UnderlyingOrderBookManager::evict_expired_across_underlyings`, returning the
    new `StrikeEvictExpiredResult` / `ChainEvictExpiredResult` /
    `ExpirationEvictExpiredResult` / `UnderlyingEvictExpiredResult` /
    `GlobalEvictExpiredResult` types. Each exposes `books_affected()` (leaf
    contract-book unit, identical semantics to the mass-cancel counterpart) and
    `total_evicted()`, and walks the tree in each container's deterministic order
    (expirations by key, strikes ascending, underlyings by symbol).
  - Not journaled as a wrapper-level sequencer command: `OptionChainCommand` is
    not `#[non_exhaustive]`, so adding a variant would be a breaking change
    (rejected by `cargo-semver-checks` and incompatible with a patch release).
    The engine journals the sweep at its own layer via
    `SequencerCommand::EvictExpiredOrders` (`orderbook-rs`). (#141)

### Fixed

- `OptionOrderBook`'s `*_full` submit methods (`add_limit_order_full`,
  `add_limit_order_with_tif_full`, `add_limit_order_with_user_full`,
  `add_limit_order_with_tif_and_user_full`) now attribute fills **per-call**:
  each returns the `TradeResult` built from its own submission via the engine's
  `add_limit_order_with_result` / `add_limit_order_with_user_and_result`
  primitives (`orderbook-rs` 0.10), instead of reading a per-book capture slot
  populated by a trade listener. Concurrent submits to the same book no longer
  cross-attribute (a caller reading another submission's result) or lose fills
  (a caller reading `None`). The internal per-book capture slot the `*_full`
  methods used for attribution — its scope-refcount arming and the
  clear/extract helpers — is removed; the opt-in continuous-capture accessor
  (`arm_trade_capture` / `last_trade_result` / `is_trade_capture_armed`) is
  unchanged. Public signatures are unchanged (no API break). On an
  error-after-fills path (an unfillable IOC remainder, or a self-trade-prevention
  taker-cancel after earlier non-self fills) the `*_full` methods return the
  typed `Err` and the executed fills reach only the trade listener — now
  documented on the `*_full` surface. (#140)

## [0.6.0] - 2026-07-10

### ⚠️ Breaking Changes

- **`orderbook-rs` dependency bumped `0.9` → `0.10`.** `orderbook-rs` is a
  *public* dependency: types re-exported at this crate's root (`TradeResult`,
  `MassCancelResult`, `OrderStatus`, `OrderStateTracker`, `FeeSchedule`,
  `STPMode`, `CancelReason`, the boundary newtypes, …) now come from
  `orderbook-rs 0.10`, so downstream crates that also depend on `orderbook-rs`
  directly must move their own pin to `0.10` in the same update or the two
  copies' types will not unify. No code changes are required in this crate's
  own API — signatures and behavior are unchanged.
- Migration notes for direct `orderbook-rs` users: `SequencerCommand` /
  `SequencerResult` are `#[non_exhaustive]` in 0.10 (exhaustive `match`es need
  a wildcard arm). 0.10 also adds `OrderBook::evict_expired_orders(now_ms)`
  (host-driven GTD/DAY expiry sweep) — not yet surfaced through this wrapper;
  tracked in #141.

### Changed

- `pricelevel` floor raised to `0.8.4` (GTD deadline documented as Unix
  milliseconds + pinning test).

## [0.5.0] - 2026-06-27

### ⚠️ Breaking Changes

#### Errors

- `Error` is now `#[non_exhaustive]` — every `match` over an `Error` value must add a wildcard arm. Five new variants ship this release (`OrderBookEngine`, `IllegalStatusTransition`, `UnderlyingMismatch`, `InstrumentIdExhausted`, `NatsSubject`). (#121)
- Removed the never-constructed `Error` variants `InventoryLimitExceeded`, `RiskLimitBreached`, `HedgingError`, `MarketDataError`, `AdapterError` and their constructors; use `Error::validation`/`configuration`/`pricing`/`no_data` or your own error type instead. (#121)

#### `Quote`

- `Quote::new`/`empty` and the price/size/timestamp accessors now use the `pricelevel` newtypes `Price`/`Quantity`/`TimestampMs` instead of raw integers; wrap inputs with `Price::new`/`Quantity::new`/`TimestampMs::new` and unwrap getters with `as_u128`/`as_u64`. The JSON wire format is unchanged (newtypes are `#[serde(transparent)]`). (#125)
- `Quote` no longer carries an `id` field or `id()` accessor; newly serialized JSON omits `id` (old JSON containing `id` still deserializes). (#124)

#### Leaf book (`OptionOrderBook`)

- `set_status`, `halt`, and `resume` are now fallible (`-> Result<()>`) and reject illegal lifecycle edges with `Error::IllegalStatusTransition` instead of silently storing the status. (#112, #119)
- `expire` is now fallible (`-> Result<Vec<OrderId>>`) and returns cancelled ids in engine processing order, not `get_all_orders()` order. (#97, #112)
- `compare_and_set_status` now returns `false` (no swap) for an illegal target edge even when `expected` matches the current status. (#112)
- Removed the cached-quote API `update_last_quote`, `last_quote`, and `last_quote_arc`; call `best_quote()` and diff against your own stored `Quote`. This removes the only `&mut self` method on the leaf. (#124)
- `last_trade_result()` no longer auto-populates on the plain order path; call `arm_trade_capture(true)` first, or use the `*_full` methods / NATS publisher / order-state tracker. (#124)

#### Hierarchy

- `StrikeOrderBook::call_greeks`/`put_greeks` now return an owned `Option<Greek>` (no longer `const`, no longer a borrow). (#111)
- `OptionChainOrderBookManager::iter` and `ExpirationOrderBookManager::iter` now yield owned `(ExpirationDate, Arc<…>)` tuples in ascending expiration order instead of `crossbeam_skiplist` `Entry` handles. (#90)
- `books_affected()` on every mass-cancel result type now counts affected leaf call/put books across the whole subtree (a both-legs chain over N strikes reports `2N`, not `N`). (#122)

#### Config / construction

- `ContractSpecsBuilder::build` is now fallible (`-> Result<ContractSpecs>`), rejecting zero tick/lot/contract/min sizes and an inverted `[min,max]` window via `Error::ConfigurationError`. (#110)
- `InstrumentRegistry::allocate` is now fallible (`-> Result<u32>`), returning `Error::InstrumentIdExhausted` on u32 ID-space exhaustion. (#107)
- `ParsedSymbol` fields are now private; construct via `ParsedSymbol::try_new(..)?` and read through `underlying()`/`expiration()`/`expiration_str()`/`strike()`/`option_style()`. (#91)

#### Greeks / pricing

- `GreeksEngine::subscribe` now returns a `SubscriptionId` (was `()`); statement-style calls keep compiling. (#133)
- `GreeksEngine::calculate_greeks`/`calculate_strike_greeks` now reject a non-finite risk-free rate and a non-finite/negative dividend yield with `Error::GreeksError`, drop the silent 5% rate fallback, and price a `0.0` dividend as a true zero (no `0.0001` clamp) — Greeks differ numerically for `dividend_yield == 0.0`. (#100)
- `MockPriceFeed::set_price` delivery is now asynchronous, per-subscriber, keep-latest: it no longer notifies listeners inline and may coalesce/drop intermediate updates; do not assume a wired `MarkPriceCalculator` has observed an update when `set_price` returns. (#129)

#### NATS (feature `nats`)

- Removed the post-construction `connect_nats` methods across the hierarchy, `OptionChainOrderBook::nats_handle_count`, and the old `book`-module `NatsPublisherHandles` (with its `trade_listener`/`book_listener` fields). Build NATS-enabled books via `build_option_order_book_with_nats` / `build_underlying_manager_with_nats`, which install publishers pre-`Arc`; the reshaped `nats`-module `NatsPublisherHandles` drops the listener fields and adds metrics + async `shutdown()`. (#101, #120)
- `OptionChainNatsConfig::new` and `OptionChainSubjectBuilder::new` were replaced by fallible `try_new(..) -> Result<…>` that validate subject components and fail with `Error::NatsSubject`. (#103)
- `OptionChainSubjectBuilder::from_symbol` now parses through `SymbolParser`: expiry must be a valid `YYYYMMDD`, strike a positive integer (normalized via `u64`, leading zeros dropped); invalid input yields `Error::InvalidSymbol`/`Error::NatsSubject`. (#103, #91)

#### Expiry config

- `ExpiryCycleConfig::expiry_time_utc`/`settlement_time_utc` changed from `(u32, u32)` hour/minute tuples to `chrono::NaiveTime`; the JSON wire shape changed from `[hour, minute]` arrays to `"HH:MM:SS"` strings. Persisted 0.4.4 configs must be re-encoded. (#123)
- `ExpiryCycleConfig::validate` now rejects any `CycleRule.count` greater than `MAX_CYCLE_COUNT` (512). (#123)

#### Sequencer (feature `sequencer`)

- The journaled types `MassCancelScope`, `MassCancelType`, `OptionChainCommand`, `OptionChainResult`, `OptionChainEvent` gained `#[serde(deny_unknown_fields)]`; journal JSON with extra/extension fields no longer decodes (variant tags and field names are unchanged). (#113)
- `OptionChainCommand` gained `SetInstrumentStatus` and `OptionChainResult` gained `StatusChanged`; neither enum is `#[non_exhaustive]`, so existing exhaustive matches must add arms. (#114)
- `SequencedUnderlyingOrderBook::submit_add_order` now vivifies the target expiration/strike instead of returning `BookNotFound` for an unlisted contract; only an unparseable symbol yields `BookNotFound`, and a cross-underlying symbol is `Rejected`. (#93)

### Added

#### Crate surface / re-exports

- Every public `orderbook::` item is now also re-exported at the crate root, so each type resolves as both `option_chain_orderbook::X` and `option_chain_orderbook::orderbook::X` (purely additive; no 0.4.4 path removed). (#109)
- The boundary newtypes `OrderId`, `OrderType`, `Side`, `TimeInForce` (from `orderbook_rs`) and `Price`, `Quantity`, `TimestampMs` (from `pricelevel`) are now re-exported from this crate; consumers no longer need a direct `orderbook_rs`/`pricelevel` dependency to name them. (#109, #125)
- Under `sequencer`, the command/event/journal/replay types are now also re-exported at the crate root. (#109)

#### Errors

- New `Error` variants, each with a `#[must_use] #[cold]` constructor: `OrderBookEngine(orderbook_rs::prelude::OrderBookError)` wrapping the upstream engine error via `#[from]` and preserving the typed source chain (#108); `IllegalStatusTransition { from, to }` (#112); `UnderlyingMismatch { symbol, parsed, expected }` (#91); `InstrumentIdExhausted` (#107); `NatsSubject { field, reason }` (present unconditionally, not feature-gated) (#103).

#### Leaf book

- `arm_trade_capture(bool)` / `is_trade_capture_armed()` to opt a book into continuous trade capture for `last_trade_result()` (disarmed by default to keep the match hot path allocation-free). (#124)
- `has_both_sides()`, a cheap top-of-book two-sidedness check used by the strike layer's `is_fully_quoted`. (#124)

#### Hierarchy / instruments

- `ExpirationOrderBookManager::get_or_create_inserted` and `UnderlyingOrderBook::get_or_create_expiration_inserted`, race-free creation helpers returning `(Arc<…>, bool)` where the bool flags the single insert winner. (#115)
- `InstrumentStatus::can_transition(self, to) -> bool`, the single source of truth for legal lifecycle edges. (#112)
- `ValidationConfig::validate` / `ContractSpecs::validate` returning `Result<()>` for structurally-broken settings. (#110)
- `SymbolParser::parse_yyyymmdd`, `ParsedSymbol::try_new`, and the `ParsedSymbol` accessors. (#91)
- `InstrumentRegistry` now implements `Debug`. (#132)

#### Greeks / pricing

- `MarkPriceCalculator::current_mark_price()` (pure read of the last committed mark, `Option<u64>`) and `advance_mark()` (explicit mutating tick), splitting read from tick. (#99)
- `GreeksEngine::unsubscribe(id: SubscriptionId) -> bool` to remove a Greeks listener registered via `subscribe`. (#133)

#### NATS (feature `nats`)

- `build_option_order_book_with_nats(symbol, option_style, &config)` and `build_underlying_manager_with_nats(config)` constructors that install publishers pre-`Arc`; both re-exported at the crate root. (#101, #120)
- `NatsPublisherHandles` metrics — `trade_publish_count`/`trade_error_count`/`book_publish_count`/`book_error_count` (`-> u64`) — and an async `shutdown()`. (#120)
- `OptionChainSubjectBuilder::option_style() -> OptionStyle`. (#103)

#### Sequencer (feature `sequencer`)

- `SequencedUnderlyingOrderBook::submit_set_instrument_status(symbol, status)`, journaling an `OptionChainCommand::SetInstrumentStatus` into the replayable stream. (#114)
- `replay()` now reconstructs journaled instrument-status transitions, so a halted/settling/expired strike replays into the same status (and an `AddOrder` the live run rejected stays rejected on replay). (#114)

#### Expiry config

- `ExpiryCycleConfig::MAX_CYCLE_COUNT` (= 512), the inclusive upper bound now enforced by `validate()`. (#123)

### Changed

- `StrikeOrderBook::update_call_greeks`/`update_put_greeks` now take `&self` (interior mutability via `RwLock`); existing callers still compile. (#111)
- `OptionOrderBook::best_quote` is now an allocation-free bounded top-of-book read (~11.75µs → ~69ns); the returned value is unchanged. (#124)
- `InstrumentRegistry::iter()` now returns entries sorted ascending by instrument id (was arbitrary `DashMap` shard order); signature unchanged. (#132)
- `SymbolParser::parse` now accepts lowercase `c`/`p` option-type tokens in addition to `C`/`P`. (#91)
- `Quote::mid_price`/`spread_bps` now return `None` for a non-finite result (and `spread_bps` additionally requires a positive finite mid). (#125)
- `SymbolIndex::symbols()` is now `#[must_use]` (documented as an unspecified-order snapshot). (#132)
- `GlobalStats`/`UnderlyingStats` are now computed in a single coherent traversal with checked accumulation; struct shapes and quiescent-tree values are unchanged. (#126)
- `MarkPriceCalculator::mark_price` is now `#[deprecated(since = "0.5.0")]` and delegates to `advance_mark()`; migrate to `current_mark_price()` (read) or `advance_mark()` (tick). Builds with `deny(deprecated)`/`deny(warnings)` will fail until callers migrate. (#99)
- `OptionChainSubjectBuilder::option_type()` now returns `&'static str` and always emits the canonical `C`/`P` token. (#103)
- Sequencer mass-cancel totals now use `checked_add`, returning `Rejected { reason: "mass-cancel total overflow" }` instead of saturating at `usize::MAX`. (#105)
- Structured `tracing` was added on cold (non-order, non-quote) paths across the Greeks engine, mark-price, index feed, expiry lifecycle, and scheduler; no global subscriber is installed by the crate. (#128)
- Internal module reorganization behind unchanged public paths: `NatsPublisherHandles` moved to the `nats` module, and the `IndexPriceFeed` trait + `PriceUpdate`/`PriceUpdateListener`/`SubscriptionId` moved to a new neutral `index_feed` module; the documented `orderbook::` re-export paths are unchanged. (#118, #120)
- The published-crate `include` list now packages `tests/fixtures/**/*` (packaging-only; no downstream compile effect). (#135)

### Fixed

- `OptionOrderBook::cancel_order` now returns `Ok(false)` for a not-found/no-op cancel (previously `Ok(true)`) and propagates a genuine engine failure as `Err` (previously swallowed as `Ok(false)`). (#95)
- `OptionOrderBook::clear` (and the sweep inside `expire`) now route through the engine's `cancel_all_orders`, so dropped orders reach the terminal `Cancelled` state, the cancelled counter and order-state tracker advance, book-change listeners fire, and per-account risk state resets. (#97)
- `StrikeOrderBookManager::atm_strike` now uses `u64::abs_diff` for overflow-safe nearest-strike selection (lower strike wins ties, documented and deterministic); chain/expiration accessors inherit the fix. (#131)
- `StrikeGenerator::cleanup_empty_strikes` now uses `checked_mul` for the keep-range and returns `Error::ConfigurationError` on overflow instead of saturating to a wrong range; the index slice is also bounds-checked. (#104)
- The chain and expiration managers now key their SkipMaps on a collision-free, clock-independent `ExpirationKey`, fixing silent key collisions and making get/contains/remove/iter deterministic; stored books still expose the original `ExpirationDate`. (#90)
- `get_or_create` across all hierarchy managers is now atomic and idempotent (`SkipMap::get_or_insert` with one-time side effects gated to the `ptr_eq` winner), eliminating split-brain books and double ID allocation under concurrent creation. (#92)
- `GreeksEngine` notification no longer holds the listener lock across callbacks (with poisoned-lock recovery): a listener may re-enter the engine without deadlock, and a panicking listener no longer poisons the mutex or permanently disables future notifications. (#98)
- `GreeksAggregator::remove_position` now prunes the empty account shell via an atomic `remove_if`, fixing an unbounded-memory leak on repeated open/close cycles on a single-position account. (#127)
- The sequencer now validates a command's parsed underlying against the book and rejects a mismatch with `Error::UnderlyingMismatch` instead of silently mis-routing (e.g. an `ETH-…` command into a BTC book). (#91)
- `ExpiryLifecycleManager::check_expirations` now computes a chain's lifecycle state as the minimum status across every call and put book, so a lagging strike no longer causes a skipped transition. (#106)
- `ExpiryScheduler::refresh_expirations` now derives `is_new` from a single atomic `get_or_create_expiration_inserted`, invoking the `ExpirationCallback` exactly once per date under concurrent refresh. (#115)
- Sequencer `replay()` now advances the sequence with `checked_add(1)`, failing loudly on a journal at `u64::MAX` instead of the previous `saturating_add(1)` that silently stalled. (#105)

[0.5.0]: https://github.com/joaquinbejar/Option-Chain-OrderBook/releases/tag/v0.5.0
