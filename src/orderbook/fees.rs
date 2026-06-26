//! Fee schedule configuration wiring.
//!
//! Hierarchy managers propagate an optional [`FeeSchedule`](orderbook_rs::FeeSchedule)
//! to the contract books they create. When a fee schedule is configured, trade
//! results include maker and taker fee calculations.
//!
//! The underlying `OrderBook<T>` supports configurable fee schedules via
//! [`FeeSchedule`](orderbook_rs::FeeSchedule):
//! - **None** (default) — no fees applied
//! - **Maker/taker fees** — specified in basis points (bps)
//! - **Maker rebates** — negative maker bps provide rebates
//!
//! The thread-safe holder is the generic
//! [`Shared`](super::shared::Shared)`<Option<FeeSchedule>>`: managers store the
//! schedule behind a lock so it can be propagated to children through `&self`
//! setters without requiring `&mut self`. The shared-wrapper boilerplate
//! (locking, poison recovery, `Debug`) lives once in
//! [`shared`](super::shared).
