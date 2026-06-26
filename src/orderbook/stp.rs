//! Self-trade prevention (STP) configuration wiring.
//!
//! Hierarchy managers propagate an [`STPMode`](orderbook_rs::STPMode) to the
//! contract books they create. STP prevents a trader's incoming order from
//! matching against their own resting orders.
//!
//! The underlying `OrderBook<T>` supports these STP modes via
//! [`STPMode`](orderbook_rs::STPMode):
//! - **None** (default) — no self-trade prevention
//! - **CancelTaker** — cancel the incoming order on self-trade
//! - **CancelMaker** — cancel the resting order on self-trade
//! - **CancelBoth** — cancel both orders on self-trade
//!
//! The thread-safe holder is the generic
//! [`Shared`](super::shared::Shared)`<STPMode>`: managers store the mode behind
//! a lock so it can be propagated to children through `&self` setters without
//! requiring `&mut self`. The shared-wrapper boilerplate (locking, poison
//! recovery, `Debug`) lives once in [`shared`](super::shared).
