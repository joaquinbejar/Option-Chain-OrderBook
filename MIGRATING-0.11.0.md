# Migrating `option-chain-orderbook` 0.10.0 → 0.11.0

`0.11.0` is a dependency-identity release: **no API, journal wire format or
replay semantics changed**, and no source file moved. It is a minor rather
than a patch because two *public* dependencies moved to a new line, so a
downstream crate that co-pins either one must move its own pin in the same
update or the types will not unify.

---

## `optionstratlib` public dependency moved `0.18` → `0.21`

`ExpirationDate`, `Positive` and `Greek` appear in this crate's public
signatures, and `ExpirationDateError` / `DecimalError` are carried by
`Error::ExpirationDateError` and `Error::OptionStratLibDecimal`. Moving to
`optionstratlib 0.21` pulls `expiration_date 0.2 → 0.3` and
`positive 0.5 → 0.6` with it, so from the point of view of a crate still on
`optionstratlib 0.18` those five are new types (same precedent as the
`orderbook-rs` bumps in 0.6.0 and 0.9.0).

`OptionStyle` and `Side` are **not** affected: both `optionstratlib` lines
re-export them from `financial_types 0.2`, so
`OptionOrderBook::new(symbol, OptionStyle)` keeps unifying across the bump.

**Before (0.10.0)**

```toml
[dependencies]
option-chain-orderbook = "0.10"
optionstratlib = "0.18"
```

**After (0.11.0)**

```toml
[dependencies]
option-chain-orderbook = "0.11"
optionstratlib = "0.21"
```

A downstream crate that bridged the two lines with a renamed second copy of
`optionstratlib` (a `package = "optionstratlib"` shim to hand this crate a
`0.18` type) can delete the shim: one `optionstratlib` now satisfies both.

`cargo semver-checks` reports "no semver update required" for this move
because it only inspects this crate's own rustdoc; the break lives in the
identity of the upstream types, which is why it is recorded here.

## `async-nats` public dependency moved `0.49` → `0.50` (feature `nats`)

`OptionChainNatsConfig::jetstream()` returns `&async_nats::jetstream::Context`,
and that `Context` must be the same type `orderbook-rs`'s publishers accept.
`orderbook-rs 0.12.1` moved to `async-nats 0.50` in a patch release, so this
crate follows it and raises its `orderbook-rs` floor to `0.12.1`. The
`connect`, `jetstream::new` and `jetstream::Context` surface this crate uses is
unchanged; `async-nats 0.50` only adds an optional `chrono` backend.

**Before (0.10.0)**

```toml
async-nats = "0.49"
orderbook-rs = "0.12"
```

**After (0.11.0)**

```toml
async-nats = "0.50"
orderbook-rs = "0.12.1"
```

Crates that do not enable the `nats` feature and do not depend on
`async-nats` directly need nothing here.

## Behaviour: verified unchanged

Every `optionstratlib` symbol this crate uses exists unchanged in `0.21.1`,
and the behaviour this crate observes is identical: the `0.19` change that
signs every Greek by `Side` does not reach `GreeksEngine` (it prices
`Side::Long` at quantity one); the `positive 0.6` serde change does not touch
the journal wire format (`ExpirationDate`'s serialiser is byte-identical
between `expiration_date 0.2.1` and `0.3.0`, and no journaled type carries a
bare `Positive`); the `0.19` vanna-at-expiry change is unreachable behind the
engine's `tte > 0` guard. The frozen `v0.5.0` / `v0.8.0` / `v0.9.0` journal
fixtures decode unchanged.
