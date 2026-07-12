//! Sequencer integration for deterministic ordering and replay.
//!
//! This module provides sequencer support for the option chain hierarchy,
//! enabling deterministic ordering of all operations and journal-based replay
//! for disaster recovery and state verification.
//!
//! # Architecture
//!
//! Each [`SequencedUnderlyingOrderBook`] owns its own sequencer, providing:
//!
//! - Monotonic sequence numbers per underlying
//! - Independent journaling and replay per underlying
//! - Parallel operation across different underlyings
//! - Isolated failure domains
//!
//! # Ordering & atomicity
//!
//! Within a single [`SequencedUnderlyingOrderBook`], submissions are internally
//! serialized. Each [`submit`](SequencedUnderlyingOrderBook::submit) holds a
//! per-sequencer gate across the whole assign→execute→journal-append sequence,
//! so concurrent callers are safe but serialized: sequence assignment, book
//! mutation, and journal append happen as one indivisible step. This is what
//! makes journal insertion order equal to sequence order equal to book-mutation
//! order — a property replay relies on. Because the journal append happens
//! *inside* the critical section, the [`OptionChainJournal`] contract is that
//! `append` must be fast: a slow or blocking journal serializes every
//! submission on that underlying.
//!
//! [`replay`](SequencedUnderlyingOrderBook::replay) takes the same gate, so a
//! rebuild cannot interleave with a live submit. Deadlock analysis: there is no
//! `submit`↔`replay` nesting (neither calls the other), and the lock order is
//! always gate → journal-internal lock (never the reverse), so the gate cannot
//! deadlock against the journal. `execute_command` must never acquire the gate,
//! since it runs while the gate is already held.
//!
//! # Known limitation — trade-ID determinism
//!
//! The upstream `orderbook_rs::OrderBook` mints its trade/transaction-ID
//! namespace internally with a random `Uuid::new_v4()` at construction, and
//! offers no injection seam. Trade IDs therefore differ between a live run and
//! its replay even when the command stream, the injected
//! [`Clock`](orderbook_rs::Clock), and the matching are identical. The replay
//! oracle intentionally compares order-book *state* (resting orders,
//! top-of-book, instrument status) and excludes trade IDs. Tracked upstream as
//! [OrderBook-rs#199](https://github.com/joaquinbejar/OrderBook-rs/issues/199);
//! once a namespace seam ships, the hierarchy will thread a deterministic
//! namespace alongside the clock.
//!
//! # Feature Gate
//!
//! This module is only available when the `sequencer` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! option-chain-orderbook = { version = "0.4", features = ["sequencer"] }
//! ```

use crate::error::Error;
use crate::orderbook::book::TerminalOrderSummary;
use crate::orderbook::instrument_registry::InstrumentRegistry;
use crate::orderbook::instrument_status::InstrumentStatus;
use crate::orderbook::symbol_index::SymbolIndex;
use crate::orderbook::underlying::UnderlyingOrderBook;
use crate::utils::{SymbolParser, nanos_since_epoch};
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::{Clock, OrderId, Side, TimeInForce, TradeResult};
use pricelevel::{Hash32, TimestampMs};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Scope for mass cancel operations in the option chain hierarchy.
///
/// This is a journaled (on-disk) type. The wire format is pinned: variant tags
/// are `PascalCase` and any struct-variant fields are `snake_case`, and
/// [`deny_unknown_fields`](https://serde.rs/container-attrs.html#deny_unknown_fields)
/// makes a renamed/dropped field a hard decode error rather than a silent
/// replay corruption. `rename_all` / `rename_all_fields` only make the existing
/// casing explicit — they do not change the wire strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "PascalCase",
    rename_all_fields = "snake_case"
)]
pub enum MassCancelScope {
    /// Cancel across the entire underlying (all expirations, all strikes).
    Underlying,
    /// Cancel within a specific expiration.
    Expiration(ExpirationDate),
    /// Cancel within a specific strike of an expiration.
    Strike {
        /// The expiration date.
        expiration: ExpirationDate,
        /// The strike price.
        strike: u64,
    },
    /// Cancel within a specific option book (call or put).
    Book(String),
}

/// Type of mass cancel operation.
///
/// Journaled (on-disk) type. Variant tags are pinned to `PascalCase` and
/// unknown fields are rejected so journal schema drift fails loudly. All
/// variants are unit or newtype, so `rename_all_fields` is a no-op here but is
/// kept for parity with the other journal enums.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "PascalCase",
    rename_all_fields = "snake_case"
)]
pub enum MassCancelType {
    /// Cancel all orders.
    All,
    /// Cancel orders on a specific side.
    BySide(Side),
    /// Cancel orders belonging to a specific user.
    ByUser(Hash32),
}

/// The default time-in-force for a journaled [`OptionChainCommand::AddOrder`]
/// that predates the #148 `tif` field.
///
/// Serde uses this via `#[serde(default = "...")]` so an old journal — written
/// before `AddOrder` carried a `tif` — decodes to [`TimeInForce::Gtc`], which is
/// exactly the good-till-cancelled behavior every pre-#148 add had. Keeping the
/// default here (rather than relying on a `Default` impl) is required because
/// `TimeInForce` has no `Default`.
#[must_use]
#[inline]
fn default_add_order_tif() -> TimeInForce {
    TimeInForce::Gtc
}

/// Command for the option chain sequencer with hierarchy routing.
///
/// Each variant represents an operation that can be sequenced through
/// the option chain sequencer. Commands are serializable for journal
/// persistence.
///
/// This is a journaled (on-disk) type: variant tags are pinned to `PascalCase`,
/// struct-variant fields to `snake_case`, and unknown fields are rejected so a
/// renamed/dropped field can never silently corrupt replay.
///
/// This enum is `#[non_exhaustive]`: new commands are added over time, so
/// downstream `match` expressions must include a wildcard arm. This makes future
/// variant additions source-compatible; wire compatibility is preserved
/// separately by only ever appending variants (existing bincode variant indices
/// never shift, and externally-tagged JSON addresses variants by name).
///
/// Wire forward-compat is asymmetric under
/// [`deny_unknown_fields`](https://serde.rs/container-attrs.html#deny_unknown_fields):
/// a **new** binary decodes an **old** journal (the old variants are unchanged),
/// but an **old** binary decoding a **new** journal that carries a variant it
/// does not know fails to decode — expected, and the same precedent as
/// `orderbook-rs`'s `SequencerCommand`. `deny_unknown_fields` only rejects
/// unknown *fields within a known variant*; an unknown *variant tag* is rejected
/// regardless. Neither is weakened by `#[non_exhaustive]`, which is a Rust
/// source-level attribute with no effect on the serde wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "PascalCase",
    rename_all_fields = "snake_case"
)]
#[non_exhaustive]
pub enum OptionChainCommand {
    /// Add a limit order to a specific option book.
    AddOrder {
        /// Target option symbol (e.g., "BTC-20240329-50000-C").
        symbol: String,
        /// Order identifier.
        order_id: OrderId,
        /// Buy or Sell.
        side: Side,
        /// Limit price.
        price: u128,
        /// Order quantity.
        quantity: u64,
        /// Time-in-force policy for the order.
        ///
        /// Defaults to [`TimeInForce::Gtc`] (via a serde default) when absent, so
        /// a journal written before #148 decodes and replays exactly
        /// as it did then (every pre-#148 add was good-till-cancelled). That
        /// missing-field default only exists in self-describing encodings
        /// (JSON): a positional codec such as bincode cannot detect an absent
        /// trailing field, so pre-#148 *binary* records do not decode against
        /// the new shape — re-journal or migrate them instead. The wire
        /// tags are pricelevel's casing — `"GTC" | "IOC" | "FOK" | {"GTD": ms} |
        /// "DAY"` — not the `snake_case` used for the other fields. The
        /// clock-relative variants (`Gtd`, `Day`) are replay-stable only because
        /// #147 injects the engine clock; without that the eviction cutoff would
        /// read wall-clock and diverge.
        #[serde(default = "default_add_order_tif")]
        tif: TimeInForce,
        /// Owning user identity, used for by-user mass cancel and STP grouping.
        ///
        /// Defaults to the zero [`Hash32`] (via `#[serde(default)]`, which
        /// `Hash32` derives) when absent, reproducing the pre-#148 behavior where
        /// every add was attributed to the zero user. Serializes as a hex string.
        #[serde(default)]
        user_id: Hash32,
    },
    /// Cancel an order in a specific option book.
    CancelOrder {
        /// Target option symbol.
        symbol: String,
        /// Order to cancel.
        order_id: OrderId,
    },
    /// Mass cancel across a hierarchy level.
    MassCancel {
        /// Scope of the mass cancel operation.
        scope: MassCancelScope,
        /// Type of cancellation.
        cancel_type: MassCancelType,
    },
    /// Transition an instrument's lifecycle status.
    ///
    /// Journaling status changes as commands (instead of applying `halt` /
    /// `resume` / `set_status` out of band on the leaf book) is what lets replay
    /// reconstruct halted / settling / expired instruments. Without this, a
    /// strike the live run had halted would be vivified as a fresh
    /// [`Active`](InstrumentStatus::Active) book during replay, so an order the
    /// live run rejected would instead rest and diverge from live state.
    ///
    /// The transition is validated against the lifecycle state machine
    /// ([`InstrumentStatus::can_transition`]) via the leaf book's
    /// [`set_status`](crate::orderbook::OptionOrderBook::set_status); an illegal
    /// edge is recorded as [`OptionChainResult::Rejected`]. Replay is
    /// deterministic: the target status is carried in the command, and the book
    /// is materialized as a pure function of the symbol.
    SetInstrumentStatus {
        /// Target option symbol (e.g., "BTC-20240329-50000-C").
        symbol: String,
        /// The lifecycle status to transition the instrument to.
        status: InstrumentStatus,
    },
    /// Evict every resting order across the underlying whose time-in-force has
    /// expired as of `now_ms` (Unix milliseconds), in the hierarchy's
    /// deterministic sweep order. Ferries through
    /// [`UnderlyingOrderBook::evict_expired_orders`] on both live execution and
    /// replay (the #141 surface).
    ///
    /// The journaled `now_ms` is the sole deterministic input: replay MUST apply
    /// the journaled value rather than read the replay clock, so the sweep
    /// reproduces the exact set of evictions on every run. `now_ms` is a
    /// [`TimestampMs`], which is `#[serde(transparent)]` over `u64`, so the field
    /// encodes to the same bytes a bare millisecond count would in both JSON and
    /// bincode. The sweep is idempotent, so a duplicate replay evicts nothing.
    ///
    /// Wire-compatible addition: this variant is appended after every prior one,
    /// so existing journals replay unchanged and their bincode variant indices
    /// are unaffected. A journal carrying `EvictExpiredOrders` fails to decode
    /// against an older binary that predates the variant — expected, and the same
    /// precedent as `orderbook-rs`'s
    /// [`SequencerCommand::EvictExpiredOrders`](orderbook_rs::SequencerCommand).
    EvictExpiredOrders {
        /// Caller-supplied cutoff in Unix milliseconds. Every resting order whose
        /// time-in-force has expired at `now_ms` is evicted: `Gtd(deadline)` when
        /// `now_ms >= deadline`, and `Day` when `now_ms >=` the book's configured
        /// market close.
        now_ms: TimestampMs,
    },
    /// Atomically replace a resting order's price, quantity, and side.
    ///
    /// A replace names an order that already exists, so it resolves through the
    /// NON-creating `find_book_by_symbol` path — it never vivifies an expiration
    /// or strike. This is deterministic:
    /// live execution and replay both use the same non-creating resolver, so a
    /// replace against a book that was never created is rejected identically on
    /// both. The replacement carries validate-first atomic semantics from the
    /// leaf [`replace_order`](crate::orderbook::OptionOrderBook::replace_order):
    /// if the replacement's shape, risk, or self-cross check fails, the original
    /// order survives untouched.
    ///
    /// Replace fills are NOT carried in the v1 result. If the new price crosses
    /// the book the replacement can rematch and fill immediately; those fills
    /// reach only the trade listener (and the NATS publisher), not the journaled
    /// [`OptionChainResult::OrderReplaced`] — a follow-up will surface them once
    /// OrderBook-rs#199 ships replay-stable ids.
    ///
    /// Wire-compatible addition: appended after every prior variant, so existing
    /// journals replay unchanged and their bincode variant indices are
    /// unaffected. A journal carrying `ReplaceOrder` fails to decode against an
    /// older binary that predates the variant — expected, and the same
    /// forward-compat asymmetry documented on this enum.
    ReplaceOrder {
        /// Target option symbol (e.g., "BTC-20240329-50000-C").
        symbol: String,
        /// Identifier of the resting order to replace.
        order_id: OrderId,
        /// New limit price in smallest units.
        price: u128,
        /// New order quantity.
        quantity: u64,
        /// New side (Buy or Sell); a flip moves the order across the book.
        side: Side,
    },
}

/// Result of executing an option chain command.
///
/// Journaled (on-disk) type: variant tags are pinned to `PascalCase`,
/// struct-variant fields to `snake_case`, and unknown fields are rejected.
///
/// Like [`OptionChainCommand`], this enum is `#[non_exhaustive]`: new result
/// shapes accompany new commands, so downstream `match` expressions must include
/// a wildcard arm. Variants are only ever appended, so the journal wire format is
/// forward-compatible in the same asymmetric sense documented on
/// [`OptionChainCommand`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "PascalCase",
    rename_all_fields = "snake_case"
)]
#[non_exhaustive]
pub enum OptionChainResult {
    /// An order was successfully added.
    OrderAdded {
        /// The identifier of the newly added order.
        order_id: OrderId,
        /// The fills the add produced, if any.
        ///
        /// `Some` iff the add crossed the book and executed at least one trade;
        /// `None` when the order rested unfilled — which is also what any
        /// pre-#148 journal (whose `OrderAdded` had no `trade` field) decodes to
        /// via `#[serde(default)]`. As with `AddOrder`'s appended fields, that
        /// old-journal default applies only to self-describing encodings
        /// (JSON); positional codecs such as bincode cannot decode pre-#148
        /// binary records against the new shape.
        ///
        /// # Replay caveat
        ///
        /// The payload's [`TradeResult::engine_seq`], the individual trade ids,
        /// and the trade timestamps are NOT replay-stable: a replay into a fresh
        /// book mints a fresh engine-seq / trade-id namespace (pending
        /// OrderBook-rs#199). Replay discards journaled results by design (it
        /// re-executes commands and keeps the freshly produced state), so the
        /// replay==live *state* oracle is unaffected — but a consumer must not
        /// diff `trade` payloads across a live run and a replay run and expect
        /// them to match. There is deliberately no `skip_serializing_if` here:
        /// the field is always emitted (as `null` when `None`) so the JSON and
        /// bincode encodings stay symmetric across decode/encode round-trips.
        #[serde(default)]
        trade: Option<TradeResult>,
    },
    /// An order was successfully cancelled.
    OrderCancelled {
        /// The identifier of the cancelled order.
        order_id: OrderId,
    },
    /// A mass cancel operation was executed.
    MassCancelled {
        /// Number of cancelled orders.
        cancelled_count: usize,
    },
    /// An instrument's lifecycle status was changed.
    StatusChanged {
        /// The symbol whose status changed.
        symbol: String,
        /// The new lifecycle status.
        status: InstrumentStatus,
    },
    /// The command was rejected.
    Rejected {
        /// Human-readable reason for the rejection.
        reason: String,
    },
    /// The target book was not found.
    BookNotFound {
        /// The symbol that was not found.
        symbol: String,
    },
    /// An expiry sweep evicted the carried orders.
    ///
    /// The outcome of an [`OptionChainCommand::EvictExpiredOrders`]. `evicted_ids`
    /// lists every evicted order across the underlying, flattened in the
    /// hierarchy's deterministic sweep order (expirations by key, strikes
    /// ascending, call book before put, and within each leaf the engine's
    /// eviction order: bids then asks, ascending price, oldest first within a
    /// level). The list mirrors the id-centric shape of the leaf
    /// [`OptionOrderBook::evict_expired_orders`](crate::orderbook::OptionOrderBook::evict_expired_orders);
    /// the evicted count is `evicted_ids.len()`. An empty list is a successful
    /// no-op sweep, not an error.
    ExpiredEvicted {
        /// Evicted order identifiers in the sweep's deterministic order.
        evicted_ids: Vec<OrderId>,
    },
    /// A resting order was atomically replaced.
    ///
    /// The successful outcome of an [`OptionChainCommand::ReplaceOrder`]. The
    /// replacement's fills, if the new price crossed the book, are NOT carried
    /// here in v1 — they reach only the trade listener / NATS publisher (see the
    /// command's doc). This variant reports placement, not fills.
    OrderReplaced {
        /// The identifier of the replaced order.
        order_id: OrderId,
    },
}

impl OptionChainResult {
    /// Returns true if this result represents a rejection or error.
    #[must_use]
    #[inline]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Rejected { .. } | Self::BookNotFound { .. })
    }

    /// Returns true if this result represents a successful operation.
    #[must_use]
    #[inline]
    pub fn is_success(&self) -> bool {
        !self.is_error()
    }
}

/// A sequenced event emitted after processing an option chain command.
///
/// Every event carries a monotonically increasing `sequence_num` and a
/// nanosecond-precision `timestamp_ns`, enabling deterministic replay
/// and total ordering of all option chain operations.
///
/// This is the top-level journaled (on-disk) record. Its field names are pinned
/// to `snake_case` and unknown fields are rejected on decode, so a journal
/// written by a renamed/extended schema fails loudly instead of silently
/// dropping data during replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct OptionChainEvent {
    /// Monotonically increasing sequence number.
    pub sequence_num: u64,
    /// Wall-clock timestamp in nanoseconds since Unix epoch.
    pub timestamp_ns: u64,
    /// The command that was executed.
    pub command: OptionChainCommand,
    /// The result of executing the command.
    pub result: OptionChainResult,
}

/// Receipt returned after submitting a command to the sequencer.
#[derive(Debug, Clone)]
pub struct OptionChainReceipt {
    /// The sequence number assigned to this command.
    pub sequence_num: u64,
    /// The timestamp when the command was processed.
    pub timestamp_ns: u64,
    /// The result of the command.
    pub result: OptionChainResult,
}

/// Journal trait for option chain events.
///
/// Provides append and read operations for [`OptionChainEvent`] persistence,
/// replacing the upstream `Journal<()>` placeholder encoding with a
/// purpose-built abstraction.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync`. The intended pattern is
/// single-writer (the sequencer) with concurrent readers (replay,
/// monitoring).
pub trait OptionChainJournal: Send + Sync {
    /// Appends an event to the journal.
    ///
    /// The event must be durably persisted before this method returns.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if serialization or I/O fails.
    fn append(&self, event: &OptionChainEvent) -> Result<(), Error>;

    /// Reads events starting from the given sequence number (inclusive).
    ///
    /// Returns events in sequence order. If `sequence` is beyond the
    /// last entry, returns an empty `Vec`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if deserialization or I/O fails.
    fn read_from(&self, sequence: u64) -> Result<Vec<OptionChainEvent>, Error>;

    /// Returns the sequence number of the last entry, or `None` if empty.
    #[must_use]
    fn last_sequence(&self) -> Option<u64>;

    /// Returns the total number of entries in the journal.
    ///
    /// This allows callers to check journal size without loading entries,
    /// enabling memory-conscious decisions before replay operations.
    ///
    /// The default implementation returns `None` to indicate the count is
    /// unavailable. Implementations that can efficiently count entries
    /// should override this method.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if counting fails (e.g., I/O error for file journals).
    fn entry_count(&self) -> Result<Option<u64>, Error> {
        Ok(None)
    }

    /// Reads up to `limit` events starting from the given sequence number.
    ///
    /// This enables OOM-safe streaming replay by limiting allocation at the
    /// source instead of loading all entries and truncating afterwards.
    ///
    /// The default implementation falls back to [`read_from`](Self::read_from)
    /// and truncates the result. Implementations that can efficiently limit
    /// reads at the source should override this method.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if deserialization or I/O fails.
    fn read_from_with_limit(
        &self,
        sequence: u64,
        limit: usize,
    ) -> Result<Vec<OptionChainEvent>, Error> {
        let all = self.read_from(sequence)?;
        Ok(all.into_iter().take(limit).collect())
    }
}

/// In-memory journal for testing and lightweight usage.
///
/// Stores events in a `Mutex<Vec<OptionChainEvent>>`. Not suitable for
/// production persistence but useful for unit tests and replay validation.
#[derive(Debug, Default)]
pub struct InMemoryOptionChainJournal {
    /// Stored events in sequence order.
    events: std::sync::Mutex<Vec<OptionChainEvent>>,
}

impl InMemoryOptionChainJournal {
    /// Creates a new empty in-memory journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of stored events.
    ///
    /// Recovers from a poisoned lock instead of silently reporting zero.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.events.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Returns `true` if the journal contains no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl OptionChainJournal for InMemoryOptionChainJournal {
    fn append(&self, event: &OptionChainEvent) -> Result<(), Error> {
        let mut guard = self
            .events
            .lock()
            .map_err(|e| Error::journal_error(format!("lock poisoned: {}", e)))?;
        guard.push(event.clone());
        Ok(())
    }

    fn read_from(&self, sequence: u64) -> Result<Vec<OptionChainEvent>, Error> {
        let guard = self
            .events
            .lock()
            .map_err(|e| Error::journal_error(format!("lock poisoned: {}", e)))?;
        Ok(guard
            .iter()
            .filter(|e| e.sequence_num >= sequence)
            .cloned()
            .collect())
    }

    fn last_sequence(&self) -> Option<u64> {
        match self.events.lock() {
            Ok(guard) => guard.last().map(|e| e.sequence_num),
            Err(poisoned) => poisoned.into_inner().last().map(|e| e.sequence_num),
        }
    }

    fn entry_count(&self) -> Result<Option<u64>, Error> {
        let guard = self
            .events
            .lock()
            .map_err(|e| Error::journal_error(format!("lock poisoned: {}", e)))?;
        Ok(Some(guard.len() as u64))
    }

    fn read_from_with_limit(
        &self,
        sequence: u64,
        limit: usize,
    ) -> Result<Vec<OptionChainEvent>, Error> {
        let guard = self
            .events
            .lock()
            .map_err(|e| Error::journal_error(format!("lock poisoned: {}", e)))?;
        Ok(guard
            .iter()
            .filter(|e| e.sequence_num >= sequence)
            .take(limit)
            .cloned()
            .collect())
    }
}

/// Internal sequencer that assigns sequence numbers and timestamps.
pub(crate) struct OptionChainSequencer {
    /// Next sequence number to assign.
    sequence: AtomicU64,
    /// Count of successfully executed commands.
    success_count: AtomicU64,
    /// Count of rejected commands.
    reject_count: AtomicU64,
    /// Serializes the assign→execute→journal-append critical section.
    gate: Mutex<()>,
}

impl OptionChainSequencer {
    /// Creates a new sequencer starting from sequence 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            reject_count: AtomicU64::new(0),
            gate: Mutex::new(()),
        }
    }

    /// Creates a new sequencer starting from a specific sequence number.
    ///
    /// Use this when resuming from a journal checkpoint.
    #[must_use]
    #[allow(dead_code)]
    pub fn with_start_sequence(start: u64) -> Self {
        Self {
            sequence: AtomicU64::new(start),
            success_count: AtomicU64::new(0),
            reject_count: AtomicU64::new(0),
            gate: Mutex::new(()),
        }
    }

    /// Returns the sequence number that will be assigned by the next call to `assign()`.
    #[must_use]
    #[inline]
    pub fn current_sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    /// Returns the count of successfully executed commands.
    #[must_use]
    #[inline]
    pub fn success_count(&self) -> u64 {
        self.success_count.load(Ordering::Acquire)
    }

    /// Returns the count of rejected commands.
    #[must_use]
    #[inline]
    pub fn reject_count(&self) -> u64 {
        self.reject_count.load(Ordering::Acquire)
    }

    /// Assigns a sequence number and timestamp to a command.
    ///
    /// Returns `(sequence_num, timestamp_ns)`.
    #[inline]
    pub fn assign(&self) -> (u64, u64) {
        let seq = self.sequence.fetch_add(1, Ordering::AcqRel);
        let ts = nanos_since_epoch();
        (seq, ts)
    }

    /// Records a successful execution.
    #[inline]
    pub fn record_success(&self) {
        self.success_count.fetch_add(1, Ordering::Release);
    }

    /// Records a rejected command.
    #[inline]
    pub fn record_reject(&self) {
        self.reject_count.fetch_add(1, Ordering::Release);
    }

    /// Acquires the serialization gate for the assign→execute→journal-append
    /// critical section.
    ///
    /// The returned guard serializes concurrent submissions so that sequence
    /// assignment, book mutation, and journal append happen as one indivisible
    /// step; releasing the guard (dropping it) ends the critical section.
    /// Recovers from a poisoned lock via
    /// `unwrap_or_else(|poisoned| poisoned.into_inner())`, matching the
    /// poison-recovery policy used across the hierarchy (see
    /// [`Shared`](crate::orderbook::shared)), so a panic while another thread
    /// held the gate never wedges the sequencer.
    pub fn acquire_gate(&self) -> MutexGuard<'_, ()> {
        self.gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Advances the sequence counter so the next assigned number is at least
    /// `next`, without ever moving it backwards.
    ///
    /// Encapsulates the `fetch_max` used on the replay-resume path so callers
    /// (e.g. [`SequencedUnderlyingOrderBook::replay`]) do not reach into the
    /// private `sequence` field directly. Advancing past the highest replayed
    /// sequence number keeps subsequently assigned ids non-conflicting.
    #[inline]
    pub fn advance_to(&self, next: u64) {
        self.sequence.fetch_max(next, Ordering::Release);
    }
}

impl Default for OptionChainSequencer {
    fn default() -> Self {
        Self::new()
    }
}

/// A sequenced underlying order book with deterministic ordering and journaling.
///
/// Wraps an [`UnderlyingOrderBook`] and routes all operations through an
/// internal sequencer, assigning monotonic sequence numbers and optionally
/// persisting events to a journal for replay.
///
/// # Example — basic usage
///
/// ```rust,ignore
/// use option_chain_orderbook::orderbook::SequencedUnderlyingOrderBook;
///
/// let book = SequencedUnderlyingOrderBook::new("BTC");
///
/// // Submit a sequenced command
/// let receipt = book.submit_add_order(
///     "BTC-20240329-50000-C",
///     order_id,
///     Side::Buy,
///     price,
///     quantity,
/// )?;
///
/// println!("Sequence: {}", receipt.sequence_num);
/// ```
///
/// # Example — listing instruments via registry
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use option_chain_orderbook::orderbook::{
///     InstrumentRegistry, SequencedUnderlyingOrderBook, SymbolIndex,
/// };
/// use option_chain_orderbook::{OrderId, Side};
///
/// // 1. Create shared registry & symbol index
/// let registry = Arc::new(InstrumentRegistry::new());
/// let symbol_index = Arc::new(SymbolIndex::new());
///
/// // 2. Build the sequenced book with registry + index
/// let book = SequencedUnderlyingOrderBook::new_with_registry_and_index(
///     "BTC",
///     Arc::clone(&registry),
///     Arc::clone(&symbol_index),
/// );
///
/// // 3. Submit an order — the hierarchy auto-registers the instrument
/// let _receipt = book.submit_add_order(
///     "BTC-20240329-50000-C",
///     OrderId::new(),
///     Side::Buy,
///     100,
///     10,
/// )?;
///
/// // 4. Enumerate instruments
/// for (id, info) in registry.iter() {
///     println!("id={id}, symbol={}", info.symbol());
/// }
/// ```
pub struct SequencedUnderlyingOrderBook {
    /// The underlying order book hierarchy.
    inner: UnderlyingOrderBook,
    /// The sequencer for assigning sequence numbers.
    sequencer: OptionChainSequencer,
    /// Optional journal for event persistence.
    journal: Option<Arc<dyn OptionChainJournal>>,
}

impl SequencedUnderlyingOrderBook {
    /// Creates a new sequenced underlying order book without journaling.
    #[must_use]
    pub fn new(underlying: impl Into<String>) -> Self {
        Self {
            inner: UnderlyingOrderBook::new(underlying),
            sequencer: OptionChainSequencer::new(),
            journal: None,
        }
    }

    /// Creates a new sequenced underlying order book with a journal.
    #[must_use]
    pub fn with_journal(
        underlying: impl Into<String>,
        journal: Arc<dyn OptionChainJournal>,
    ) -> Self {
        Self {
            inner: UnderlyingOrderBook::new(underlying),
            sequencer: OptionChainSequencer::new(),
            journal: Some(journal),
        }
    }

    /// Creates a new sequenced underlying order book with an instrument
    /// registry and symbol index, without journaling.
    ///
    /// The registry and index are propagated through the
    /// [`UnderlyingOrderBook`] hierarchy so that every strike created by
    /// subsequent commands is automatically registered.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol (e.g., "BTC")
    /// * `registry` - Shared instrument registry for ID allocation
    /// * `symbol_index` - Shared symbol index for O(1) lookups
    #[must_use]
    pub fn new_with_registry_and_index(
        underlying: impl Into<String>,
        registry: Arc<InstrumentRegistry>,
        symbol_index: Arc<SymbolIndex>,
    ) -> Self {
        Self {
            inner: UnderlyingOrderBook::new_with_registry_and_index(
                underlying,
                registry,
                symbol_index,
            ),
            sequencer: OptionChainSequencer::new(),
            journal: None,
        }
    }

    /// Creates a new sequenced underlying order book with journal,
    /// instrument registry, and symbol index.
    ///
    /// # Arguments
    ///
    /// * `underlying` - The underlying asset symbol
    /// * `journal` - Journal for event persistence and replay
    /// * `registry` - Shared instrument registry for ID allocation
    /// * `symbol_index` - Shared symbol index for O(1) lookups
    #[must_use]
    pub fn with_journal_registry_and_index(
        underlying: impl Into<String>,
        journal: Arc<dyn OptionChainJournal>,
        registry: Arc<InstrumentRegistry>,
        symbol_index: Arc<SymbolIndex>,
    ) -> Self {
        Self {
            inner: UnderlyingOrderBook::new_with_registry_and_index(
                underlying,
                registry,
                symbol_index,
            ),
            sequencer: OptionChainSequencer::new(),
            journal: Some(journal),
        }
    }

    /// Creates a sequenced wrapper around an existing underlying order book.
    #[must_use]
    pub fn from_underlying(underlying: UnderlyingOrderBook) -> Self {
        Self {
            inner: underlying,
            sequencer: OptionChainSequencer::new(),
            journal: None,
        }
    }

    /// Creates a sequenced wrapper with journal around an existing book.
    #[must_use]
    pub fn from_underlying_with_journal(
        underlying: UnderlyingOrderBook,
        journal: Arc<dyn OptionChainJournal>,
    ) -> Self {
        Self {
            inner: underlying,
            sequencer: OptionChainSequencer::new(),
            journal: Some(journal),
        }
    }

    /// Returns a reference to the underlying order book.
    #[must_use]
    #[inline]
    pub fn underlying(&self) -> &UnderlyingOrderBook {
        &self.inner
    }

    /// Returns a reference to the instrument registry, if one was provided
    /// at construction time.
    #[must_use]
    #[inline]
    pub fn registry(&self) -> Option<&Arc<InstrumentRegistry>> {
        self.inner.registry()
    }

    /// Returns a reference to the symbol index, if one was provided
    /// at construction time.
    #[must_use]
    #[inline]
    pub fn symbol_index(&self) -> Option<&Arc<SymbolIndex>> {
        self.inner.symbol_index()
    }

    /// Injects the engine clock used to stamp orders on every leaf book the
    /// hierarchy vivifies from later commands.
    ///
    /// Delegates to [`UnderlyingOrderBook::set_clock`], so the clock is
    /// propagated to expirations, chains, and strikes created *after* this
    /// call. Set it BEFORE the first [`submit`](Self::submit) for full
    /// determinism: leaves vivified by earlier commands keep whatever clock
    /// they were built with (the upstream default `MonotonicClock` when none
    /// was injected).
    ///
    /// For byte-identical replay, the replaying instance must be configured
    /// with an identically-behaving clock — e.g. a fresh
    /// [`StubClock`](orderbook_rs::StubClock) constructed with the same
    /// start and step as the live instance — before its journal prefix is
    /// replayed. The `Arc<dyn Clock>` is shared, not deep-cloned.
    ///
    /// This call takes the same serialization gate as
    /// [`submit`](Self::submit), so it is linearized against the sequenced
    /// command stream: every command either fully precedes or fully follows
    /// the clock change, and which clock a lazily vivified leaf receives is
    /// well-defined relative to the journal order. The journaled `AddOrder`
    /// command carries a time-in-force (#148), so an injected deterministic
    /// clock makes `Gtd` / `Day` admission — and the order timestamps the
    /// engine stamps — replay-stable for commands flowing through
    /// [`submit`](Self::submit).
    #[inline]
    pub fn set_clock(&self, clock: Arc<dyn Clock>) {
        // Linearize the config change against submit/replay so a racing
        // command deterministically sees either the old or the new clock.
        let _serialized = self.sequencer.acquire_gate();
        self.inner.set_clock(clock);
    }

    /// Returns the current sequence number.
    #[must_use]
    #[inline]
    pub fn current_sequence(&self) -> u64 {
        self.sequencer.current_sequence()
    }

    /// Returns the count of successfully executed commands.
    #[must_use]
    #[inline]
    pub fn success_count(&self) -> u64 {
        self.sequencer.success_count()
    }

    /// Returns the count of rejected commands.
    #[must_use]
    #[inline]
    pub fn reject_count(&self) -> u64 {
        self.sequencer.reject_count()
    }

    /// Returns true if journaling is enabled.
    #[must_use]
    #[inline]
    pub fn has_journal(&self) -> bool {
        self.journal.is_some()
    }

    // ── Sequenced Operations ─────────────────────────────────────────────

    /// Submits a command and returns a receipt with the result.
    ///
    /// The command is assigned a sequence number and timestamp, executed
    /// against the underlying order book, and optionally persisted to the
    /// journal.
    ///
    /// # Ordering & atomicity
    ///
    /// Submissions are internally serialized: sequence assignment, book
    /// mutation, and journal append run inside a single critical section held
    /// for the duration of this call. Concurrent callers are therefore safe but
    /// serialized — one submission completes fully before the next begins. As a
    /// result, journal insertion order equals sequence order equals
    /// book-mutation order: the event journaled at sequence `n` was applied to
    /// the books strictly before the event at sequence `n + 1`.
    ///
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    pub fn submit(&self, command: OptionChainCommand) -> Result<OptionChainReceipt, Error> {
        // Hold the gate for the whole assign→execute→journal-append sequence so
        // concurrent submissions cannot interleave and so the journal is
        // appended in sequence order. Released when `_serialized` drops on
        // return.
        let _serialized = self.sequencer.acquire_gate();

        let (seq, ts) = self.sequencer.assign();
        let result = self.execute_command(&command);

        // Record metrics
        if result.is_success() {
            self.sequencer.record_success();
        } else {
            self.sequencer.record_reject();
        }

        // Persist to journal if enabled
        if let Some(ref journal) = self.journal {
            let event = OptionChainEvent {
                sequence_num: seq,
                timestamp_ns: ts,
                command,
                result: result.clone(),
            };
            journal.append(&event)?;
        }

        Ok(OptionChainReceipt {
            sequence_num: seq,
            timestamp_ns: ts,
            result,
        })
    }

    /// Submits an add order command to a specific option book.
    ///
    /// The order is good-till-cancelled ([`TimeInForce::Gtc`]) and attributed to
    /// the zero [`Hash32`] user. For a non-default time-in-force or user
    /// identity, use [`submit_add_order_with`](Self::submit_add_order_with).
    /// On a book with self-trade prevention enabled the zero user is rejected
    /// by the engine (`MissingUserId`) — STP-enabled sequenced books must
    /// submit through `submit_add_order_with` with a non-zero identity.
    ///
    /// # Arguments
    ///
    /// * `symbol` - Target option symbol (e.g., "BTC-20240329-50000-C")
    /// * `order_id` - Unique order identifier
    /// * `side` - Buy or Sell
    /// * `price` - Limit price
    /// * `quantity` - Order quantity
    ///
    /// # Errors
    ///
    /// Returns an error if the book is not found or journaling fails.
    pub fn submit_add_order(
        &self,
        symbol: &str,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
    ) -> Result<OptionChainReceipt, Error> {
        // Delegate with the pre-#148 defaults so this signature stays a
        // convenience wrapper and the two paths cannot drift.
        self.submit_add_order_with(
            symbol,
            order_id,
            side,
            price,
            quantity,
            TimeInForce::Gtc,
            Hash32::zero(),
        )
    }

    /// Submits an add order command with an explicit time-in-force and user
    /// identity.
    ///
    /// This is the full-fidelity add: it carries the `tif` and `user_id` into the
    /// journaled [`OptionChainCommand::AddOrder`] so replay reproduces the same
    /// eviction behavior (for `Gtd`/`Day`, in concert with the injected engine
    /// clock) and the same by-user attribution. On success the receipt's
    /// [`OptionChainResult::OrderAdded`] carries the fills the add produced (see
    /// that variant's replay caveat).
    ///
    /// # Arguments
    ///
    /// * `symbol` - Target option symbol (e.g., "BTC-20240329-50000-C")
    /// * `order_id` - Unique order identifier
    /// * `side` - Buy or Sell
    /// * `price` - Limit price
    /// * `quantity` - Order quantity
    /// * `tif` - Time-in-force policy for the order
    /// * `user_id` - Owning user identity
    ///
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    // A full venue add is inherently 7 order attributes (symbol, id, side, price,
    // quantity, tif, user) — bundling them into a struct would only obscure the
    // call site, so the argument count is intrinsic here.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_add_order_with(
        &self,
        symbol: &str,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> Result<OptionChainReceipt, Error> {
        let command = OptionChainCommand::AddOrder {
            symbol: symbol.to_string(),
            order_id,
            side,
            price,
            quantity,
            tif,
            user_id,
        };
        self.submit(command)
    }

    /// Submits a cancel order command.
    ///
    /// # Arguments
    ///
    /// * `symbol` - Target option symbol
    /// * `order_id` - Order to cancel
    ///
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    pub fn submit_cancel_order(
        &self,
        symbol: &str,
        order_id: OrderId,
    ) -> Result<OptionChainReceipt, Error> {
        let command = OptionChainCommand::CancelOrder {
            symbol: symbol.to_string(),
            order_id,
        };
        self.submit(command)
    }

    /// Submits a mass cancel command.
    ///
    /// # Arguments
    ///
    /// * `scope` - Hierarchy level for cancellation
    /// * `cancel_type` - Type of cancellation
    ///
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    pub fn submit_mass_cancel(
        &self,
        scope: MassCancelScope,
        cancel_type: MassCancelType,
    ) -> Result<OptionChainReceipt, Error> {
        let command = OptionChainCommand::MassCancel { scope, cancel_type };
        self.submit(command)
    }

    /// Submits an instrument status-change command.
    ///
    /// The transition is journaled as an
    /// [`OptionChainCommand::SetInstrumentStatus`] so replay reconstructs the
    /// instrument's status instead of vivifying it as
    /// [`Active`](InstrumentStatus::Active). The target book is resolved through
    /// the same vivifying path as [`submit_add_order`](Self::submit_add_order)
    /// (underlying-mismatch check, then materialize the expiration and strike),
    /// and the transition is validated against the lifecycle state machine via
    /// the leaf book's [`set_status`](crate::orderbook::OptionOrderBook::set_status).
    ///
    /// An illegal transition, a cross-underlying symbol, or a malformed symbol
    /// all yield [`OptionChainResult::Rejected`].
    ///
    /// # Arguments
    ///
    /// * `symbol` - Target option symbol
    /// * `status` - The lifecycle status to transition the instrument to
    ///
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    pub fn submit_set_instrument_status(
        &self,
        symbol: &str,
        status: InstrumentStatus,
    ) -> Result<OptionChainReceipt, Error> {
        let command = OptionChainCommand::SetInstrumentStatus {
            symbol: symbol.to_string(),
            status,
        };
        self.submit(command)
    }

    /// Submits an expiry-sweep command across the whole underlying.
    ///
    /// Journals the sweep as an [`OptionChainCommand::EvictExpiredOrders`] so
    /// replay reproduces the exact evictions by re-applying the journaled
    /// `now_ms` (never the replay clock). The receipt's
    /// [`OptionChainResult::ExpiredEvicted`] carries the evicted order ids in the
    /// hierarchy's deterministic sweep order.
    ///
    /// # Arguments
    ///
    /// * `now_ms` - Caller-supplied Unix-milliseconds cutoff. An order whose
    ///   time-in-force has expired at `now_ms` is evicted.
    ///
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    pub fn submit_evict_expired_orders(
        &self,
        now_ms: TimestampMs,
    ) -> Result<OptionChainReceipt, Error> {
        let command = OptionChainCommand::EvictExpiredOrders { now_ms };
        self.submit(command)
    }

    /// Submits an atomic replace command for a resting order.
    ///
    /// Journals the replace as an [`OptionChainCommand::ReplaceOrder`]. The
    /// target order must already exist: the command resolves through the
    /// non-creating book lookup and never vivifies an expiration or strike, so a
    /// replace against a book that does not exist is rejected. Replace semantics
    /// are validate-first and atomic — a rejected replacement leaves the original
    /// order untouched (see the leaf
    /// [`replace_order`](crate::orderbook::OptionOrderBook::replace_order)).
    ///
    /// The receipt is [`OptionChainResult::OrderReplaced`] on success,
    /// [`OptionChainResult::BookNotFound`] when the symbol names no existing
    /// book, or [`OptionChainResult::Rejected`] when the order is not resting,
    /// the symbol crosses underlyings, or the engine rejects the replacement.
    ///
    /// # Arguments
    ///
    /// * `symbol` - Target option symbol
    /// * `order_id` - Identifier of the resting order to replace
    /// * `price` - New limit price in smallest units
    /// * `quantity` - New order quantity
    /// * `side` - New side (Buy or Sell)
    ///
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    pub fn submit_replace_order(
        &self,
        symbol: &str,
        order_id: OrderId,
        price: u128,
        quantity: u64,
        side: Side,
    ) -> Result<OptionChainReceipt, Error> {
        let command = OptionChainCommand::ReplaceOrder {
            symbol: symbol.to_string(),
            order_id,
            price,
            quantity,
            side,
        };
        self.submit(command)
    }

    // ── Command Execution ────────────────────────────────────────────────

    /// Executes a command against the underlying order book.
    fn execute_command(&self, command: &OptionChainCommand) -> OptionChainResult {
        match command {
            OptionChainCommand::AddOrder {
                symbol,
                order_id,
                side,
                price,
                quantity,
                tif,
                user_id,
            } => {
                self.execute_add_order(symbol, *order_id, *side, *price, *quantity, *tif, *user_id)
            }
            OptionChainCommand::CancelOrder { symbol, order_id } => {
                self.execute_cancel_order(symbol, *order_id)
            }
            OptionChainCommand::MassCancel { scope, cancel_type } => {
                self.execute_mass_cancel(scope, cancel_type)
            }
            OptionChainCommand::SetInstrumentStatus { symbol, status } => {
                self.execute_set_instrument_status(symbol, *status)
            }
            OptionChainCommand::EvictExpiredOrders { now_ms } => {
                self.execute_evict_expired_orders(*now_ms)
            }
            OptionChainCommand::ReplaceOrder {
                symbol,
                order_id,
                price,
                quantity,
                side,
            } => self.execute_replace_order(symbol, *order_id, *price, *quantity, *side),
        }
    }

    /// Executes an add order operation.
    ///
    /// Resolution goes through `find_or_create_book_by_symbol`,
    /// which materializes the target expiration and strike from the parsed
    /// symbol. This is the determinism-critical path: an `AddOrder` vivifies its
    /// book the same way during live execution and during replay, so a journal
    /// prefix rebuilds identical structural state in a fresh book. A malformed
    /// symbol still yields [`OptionChainResult::BookNotFound`]; a cross-underlying
    /// symbol is rejected before anything is created.
    ///
    /// # Fills and the error-after-fills caveat
    ///
    /// On success the result's `trade` is `Some` iff the add crossed and executed
    /// at least one trade, `None` if it rested unfilled. The engine can also
    /// return an error *after* executing real fills — an `Ioc`/`Fok` remainder
    /// that cannot rest, or an STP taker-cancel — in which case the add is
    /// journaled as [`OptionChainResult::Rejected`] even though fills reached the
    /// trade listener (and the NATS publisher). Those fills are listener-visible
    /// only; they are deterministic, so replay reproduces the same rejection and
    /// the same fills, but a venue consumer must not read `Rejected` as "nothing
    /// happened".
    // Mirrors the full add attribute set (see `submit_add_order_with`); the count
    // is intrinsic to a limit-order add, not accidental.
    #[allow(clippy::too_many_arguments)]
    fn execute_add_order(
        &self,
        symbol: &str,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
        tif: TimeInForce,
        user_id: Hash32,
    ) -> OptionChainResult {
        let book = match self.find_or_create_book_by_symbol(symbol) {
            Ok(book) => book,
            // A cross-underlying command must be rejected with the typed reason,
            // never silently routed or masked as a missing book. The mismatch is
            // checked before any expiration/strike is materialized.
            Err(e @ Error::UnderlyingMismatch { .. }) => {
                return OptionChainResult::Rejected {
                    reason: e.to_string(),
                };
            }
            // A malformed symbol cannot name a book to create.
            Err(_) => {
                return OptionChainResult::BookNotFound {
                    symbol: symbol.to_string(),
                };
            }
        };

        match book
            .add_limit_order_with_tif_and_user_full(order_id, side, price, quantity, tif, user_id)
        {
            Ok(trade) => OptionChainResult::OrderAdded {
                order_id,
                // Carry the fills only when the add actually crossed; an empty
                // trade list means the order rested, which is `None`.
                trade: (!trade.match_result.trades().is_empty()).then_some(trade),
            },
            Err(e) => OptionChainResult::Rejected {
                reason: e.to_string(),
            },
        }
    }

    /// Executes an instrument status-change operation.
    ///
    /// Resolution goes through `find_or_create_book_by_symbol`, the SAME
    /// vivifying path as [`execute_add_order`](Self::execute_add_order), so the
    /// target leaf is materialized identically during live execution and during
    /// replay. The transition is then validated against the lifecycle state
    /// machine via the leaf book's
    /// [`set_status`](crate::orderbook::OptionOrderBook::set_status).
    ///
    /// # Determinism
    ///
    /// Fully deterministic: the target status is carried in the (deterministic)
    /// command, and the book is materialized as a pure function of the symbol —
    /// no wall-clock, RNG, or map-iteration order is consulted. Because the same
    /// path runs live and on replay, replaying a journal prefix reconstructs the
    /// same per-contract status. Consequently an `AddOrder` that the live run
    /// rejected because its strike had been halted is rejected on replay too, so
    /// replayed state matches live.
    ///
    /// An illegal transition, a cross-underlying symbol, or a malformed symbol
    /// all yield [`OptionChainResult::Rejected`] with the typed reason.
    fn execute_set_instrument_status(
        &self,
        symbol: &str,
        status: InstrumentStatus,
    ) -> OptionChainResult {
        let book = match self.find_or_create_book_by_symbol(symbol) {
            Ok(book) => book,
            // A cross-underlying or malformed symbol cannot name a book to
            // transition; both surface as a typed rejection rather than a
            // silent no-op so the journal records why.
            Err(e) => {
                return OptionChainResult::Rejected {
                    reason: e.to_string(),
                };
            }
        };

        match book.set_status(status) {
            Ok(()) => OptionChainResult::StatusChanged {
                symbol: symbol.to_string(),
                status,
            },
            // Illegal lifecycle edge (e.g. reactivating an Expired book): the
            // state machine left the status unchanged.
            Err(e) => OptionChainResult::Rejected {
                reason: e.to_string(),
            },
        }
    }

    /// Executes a cancel order operation.
    fn execute_cancel_order(&self, symbol: &str, order_id: OrderId) -> OptionChainResult {
        let book = match self.find_book_by_symbol(symbol) {
            Ok(book) => book,
            // A cross-underlying command must be rejected with the typed reason,
            // never silently routed or masked as a missing book.
            Err(e @ Error::UnderlyingMismatch { .. }) => {
                return OptionChainResult::Rejected {
                    reason: e.to_string(),
                };
            }
            Err(_) => {
                return OptionChainResult::BookNotFound {
                    symbol: symbol.to_string(),
                };
            }
        };

        match book.cancel_order(order_id) {
            Ok(_) => OptionChainResult::OrderCancelled { order_id },
            Err(e) => OptionChainResult::Rejected {
                reason: e.to_string(),
            },
        }
    }

    /// Executes an atomic replace operation.
    ///
    /// Resolution goes through the NON-creating
    /// [`find_book_by_symbol`](Self::find_book_by_symbol) — the same resolver as
    /// [`execute_cancel_order`](Self::execute_cancel_order) — so a replace never
    /// vivifies an expiration or strike. The error mapping mirrors the cancel
    /// path: a cross-underlying symbol is [`Rejected`](OptionChainResult::Rejected)
    /// with the typed reason, any other resolution failure (malformed symbol, or
    /// a book that was never created) is
    /// [`BookNotFound`](OptionChainResult::BookNotFound). The leaf replace then
    /// distinguishes replaced (`Ok(true)`), order-not-resting (`Ok(false)`, a
    /// deterministic rejection), and an engine rejection (`Err`, with the
    /// original order left untouched).
    fn execute_replace_order(
        &self,
        symbol: &str,
        order_id: OrderId,
        price: u128,
        quantity: u64,
        side: Side,
    ) -> OptionChainResult {
        let book = match self.find_book_by_symbol(symbol) {
            Ok(book) => book,
            // A cross-underlying command must be rejected with the typed reason,
            // never silently routed or masked as a missing book.
            Err(e @ Error::UnderlyingMismatch { .. }) => {
                return OptionChainResult::Rejected {
                    reason: e.to_string(),
                };
            }
            Err(_) => {
                return OptionChainResult::BookNotFound {
                    symbol: symbol.to_string(),
                };
            }
        };

        match book.replace_order(order_id, price, quantity, side) {
            Ok(true) => OptionChainResult::OrderReplaced { order_id },
            // The book exists but no order with that id is resting: a
            // deterministic rejection (not a book-not-found).
            Ok(false) => OptionChainResult::Rejected {
                reason: format!("order not found: {order_id}"),
            },
            Err(e) => OptionChainResult::Rejected {
                reason: e.to_string(),
            },
        }
    }

    /// Executes a mass cancel operation at the specified scope.
    fn execute_mass_cancel(
        &self,
        scope: &MassCancelScope,
        cancel_type: &MassCancelType,
    ) -> OptionChainResult {
        // Execute the mass cancel based on scope and type
        let cancelled_count: usize = match (scope, cancel_type) {
            (MassCancelScope::Underlying, MassCancelType::All) => match self.inner.cancel_all() {
                Ok(r) => r.total_cancelled(),
                Err(e) => {
                    return OptionChainResult::Rejected {
                        reason: e.to_string(),
                    };
                }
            },
            (MassCancelScope::Underlying, MassCancelType::BySide(side)) => {
                match self.inner.cancel_by_side(*side) {
                    Ok(r) => r.total_cancelled(),
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Underlying, MassCancelType::ByUser(user_id)) => {
                match self.inner.cancel_by_user(*user_id) {
                    Ok(r) => r.total_cancelled(),
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Expiration(expiry), MassCancelType::All) => {
                match self.inner.get_expiration(expiry) {
                    Ok(exp) => match exp.cancel_all() {
                        Ok(r) => r.total_cancelled(),
                        Err(e) => {
                            return OptionChainResult::Rejected {
                                reason: e.to_string(),
                            };
                        }
                    },
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Expiration(expiry), MassCancelType::BySide(side)) => {
                match self.inner.get_expiration(expiry) {
                    Ok(exp) => match exp.cancel_by_side(*side) {
                        Ok(r) => r.total_cancelled(),
                        Err(e) => {
                            return OptionChainResult::Rejected {
                                reason: e.to_string(),
                            };
                        }
                    },
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Expiration(expiry), MassCancelType::ByUser(user_id)) => {
                match self.inner.get_expiration(expiry) {
                    Ok(exp) => match exp.cancel_by_user(*user_id) {
                        Ok(r) => r.total_cancelled(),
                        Err(e) => {
                            return OptionChainResult::Rejected {
                                reason: e.to_string(),
                            };
                        }
                    },
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Strike { expiration, strike }, MassCancelType::All) => {
                match self.inner.get_expiration(expiration) {
                    Ok(exp) => match exp.get_strike(*strike) {
                        Ok(s) => {
                            let call_count = s
                                .call()
                                .cancel_all()
                                .map(|r| r.cancelled_count())
                                .unwrap_or(0);
                            let put_count = s
                                .put()
                                .cancel_all()
                                .map(|r| r.cancelled_count())
                                .unwrap_or(0);
                            match call_count.checked_add(put_count) {
                                Some(total) => total,
                                None => {
                                    return OptionChainResult::Rejected {
                                        reason: "mass-cancel total overflow".to_string(),
                                    };
                                }
                            }
                        }
                        Err(e) => {
                            return OptionChainResult::Rejected {
                                reason: e.to_string(),
                            };
                        }
                    },
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Strike { expiration, strike }, MassCancelType::BySide(side)) => {
                match self.inner.get_expiration(expiration) {
                    Ok(exp) => match exp.get_strike(*strike) {
                        Ok(s) => {
                            let call_count = s
                                .call()
                                .cancel_by_side(*side)
                                .map(|r| r.cancelled_count())
                                .unwrap_or(0);
                            let put_count = s
                                .put()
                                .cancel_by_side(*side)
                                .map(|r| r.cancelled_count())
                                .unwrap_or(0);
                            match call_count.checked_add(put_count) {
                                Some(total) => total,
                                None => {
                                    return OptionChainResult::Rejected {
                                        reason: "mass-cancel total overflow".to_string(),
                                    };
                                }
                            }
                        }
                        Err(e) => {
                            return OptionChainResult::Rejected {
                                reason: e.to_string(),
                            };
                        }
                    },
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Strike { expiration, strike }, MassCancelType::ByUser(user_id)) => {
                match self.inner.get_expiration(expiration) {
                    Ok(exp) => match exp.get_strike(*strike) {
                        Ok(s) => {
                            let call_count = s
                                .call()
                                .cancel_by_user(*user_id)
                                .map(|r| r.cancelled_count())
                                .unwrap_or(0);
                            let put_count = s
                                .put()
                                .cancel_by_user(*user_id)
                                .map(|r| r.cancelled_count())
                                .unwrap_or(0);
                            match call_count.checked_add(put_count) {
                                Some(total) => total,
                                None => {
                                    return OptionChainResult::Rejected {
                                        reason: "mass-cancel total overflow".to_string(),
                                    };
                                }
                            }
                        }
                        Err(e) => {
                            return OptionChainResult::Rejected {
                                reason: e.to_string(),
                            };
                        }
                    },
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Book(symbol), MassCancelType::All) => {
                match self.find_book_by_symbol(symbol) {
                    Ok(book) => book.cancel_all().map(|r| r.cancelled_count()).unwrap_or(0),
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Book(symbol), MassCancelType::BySide(side)) => {
                match self.find_book_by_symbol(symbol) {
                    Ok(book) => book
                        .cancel_by_side(*side)
                        .map(|r| r.cancelled_count())
                        .unwrap_or(0),
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            (MassCancelScope::Book(symbol), MassCancelType::ByUser(user_id)) => {
                match self.find_book_by_symbol(symbol) {
                    Ok(book) => book
                        .cancel_by_user(*user_id)
                        .map(|r| r.cancelled_count())
                        .unwrap_or(0),
                    Err(e) => {
                        return OptionChainResult::Rejected {
                            reason: e.to_string(),
                        };
                    }
                }
            }
        };

        // Return success with the count
        OptionChainResult::MassCancelled { cancelled_count }
    }

    /// Executes an expiry sweep across the whole underlying.
    ///
    /// Ferries through [`UnderlyingOrderBook::evict_expired_orders`] (the #141
    /// surface) with the journaled `now_ms`, then flattens the per-expiration /
    /// per-strike / per-book tree into a single id list in the hierarchy's
    /// deterministic sweep order. The sweep reads no clock, so it is a pure
    /// function of `now_ms` and the resting books and replays identically.
    fn execute_evict_expired_orders(&self, now_ms: TimestampMs) -> OptionChainResult {
        let result = self.inner.evict_expired_orders(now_ms);

        // Flatten in the container-nested deterministic order the sweep already
        // walked: expirations by key, strikes ascending, call book then put, and
        // within each leaf the engine's eviction order. Each level's `per_child`
        // / `per_book` vector preserves that traversal order, so a plain
        // concatenation is the deterministic, replay-stable id stream.
        let mut evicted_ids = Vec::with_capacity(result.total_evicted());
        for (_, expiration) in &result.per_child {
            for (_, chain) in &expiration.per_child {
                for (_, strike) in &chain.per_child {
                    for (_, book_ids) in &strike.per_book {
                        evicted_ids.extend_from_slice(book_ids);
                    }
                }
            }
        }

        OptionChainResult::ExpiredEvicted { evicted_ids }
    }

    /// Finds an option book by symbol.
    ///
    /// The symbol grammar is parsed by [`SymbolParser`], the single source of
    /// truth, so the derived expiration instant (and therefore the
    /// `ExpirationKey` used to look up the chain) matches whatever created the
    /// chain. The parsed underlying is validated against this book's underlying
    /// — a mismatch is rejected with [`Error::UnderlyingMismatch`] rather than
    /// silently routing the order into the wrong book.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSymbol`] for a malformed symbol,
    /// [`Error::UnderlyingMismatch`] when the symbol's underlying differs from
    /// this book's, or a not-found error when the expiration or strike does not
    /// exist in this book.
    fn find_book_by_symbol(
        &self,
        symbol: &str,
    ) -> Result<Arc<crate::orderbook::OptionOrderBook>, Error> {
        let parsed = SymbolParser::parse(symbol)?;

        let expected = self.inner.underlying();
        if parsed.underlying() != expected {
            return Err(Error::underlying_mismatch(
                symbol,
                parsed.underlying(),
                expected,
            ));
        }

        let exp_book = self.inner.get_expiration(parsed.expiration())?;
        let strike_book = exp_book.get_strike(parsed.strike())?;

        let book = match parsed.option_style() {
            OptionStyle::Call => strike_book.call_arc(),
            OptionStyle::Put => strike_book.put_arc(),
        };

        Ok(book)
    }

    /// Resolves an option book by symbol, materializing the expiration and
    /// strike if they do not already exist.
    ///
    /// This is the resolver used by the `AddOrder` path. It mirrors
    /// [`find_book_by_symbol`](Self::find_book_by_symbol) but vivifies the
    /// target book through the hierarchy's idempotent
    /// [`get_or_create_expiration`](UnderlyingOrderBook::get_or_create_expiration)
    /// / `get_or_create_strike` instead of the non-creating `get_*` lookups.
    ///
    /// # Determinism
    ///
    /// The materialization is a pure function of the (deterministic) command
    /// symbol — no wall-clock, RNG, or map-iteration order is consulted. Because
    /// the same path runs during live execution and during replay, a journal
    /// prefix rebuilds identical structural state in a fresh book. Created books
    /// inherit the hierarchy's shared configuration (validation / contract specs
    /// / STP mode / fee schedule), so a fresh book configured identically
    /// rebuilds an identical leaf. Registry-assigned instrument ids are NOT
    /// journaled, so they are not preserved when replaying into a non-empty or
    /// differently-seeded registry. They ARE, however, reconstructed
    /// deterministically when the same journal is replayed into a FRESH registry:
    /// strike creation (the sole id-allocation site) runs in the same command
    /// order on replay as live, so a fresh registry re-derives byte-identical
    /// ids. The replay==live oracle relies on this fresh-registry determinism.
    ///
    /// The parsed underlying is validated against this book's underlying BEFORE
    /// any materialization — a mismatch is rejected and creates nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSymbol`] for a malformed symbol or
    /// [`Error::UnderlyingMismatch`] when the symbol's underlying differs from
    /// this book's.
    fn find_or_create_book_by_symbol(
        &self,
        symbol: &str,
    ) -> Result<Arc<crate::orderbook::OptionOrderBook>, Error> {
        let parsed = SymbolParser::parse(symbol)?;

        let expected = self.inner.underlying();
        if parsed.underlying() != expected {
            return Err(Error::underlying_mismatch(
                symbol,
                parsed.underlying(),
                expected,
            ));
        }

        // Idempotent vivification: a second call with the same key returns the
        // same handle, so replay re-executing the same AddOrder targets the
        // exact leaf that live execution did.
        let exp_book = self.inner.get_or_create_expiration(*parsed.expiration());
        let strike_book = exp_book.get_or_create_strike(parsed.strike());

        let book = match parsed.option_style() {
            OptionStyle::Call => strike_book.call_arc(),
            OptionStyle::Put => strike_book.put_arc(),
        };

        Ok(book)
    }

    /// Replays events from the journal starting at `from_sequence`, rebuilding
    /// the hierarchy state deterministically.
    ///
    /// # Ordering & atomicity
    ///
    /// Replay acquires the same serialization gate that
    /// [`submit`](Self::submit) holds, so a replay and a concurrent live
    /// submission cannot interleave: the whole rebuild — re-executing every
    /// command and advancing the sequence counter — runs as one critical
    /// section. A submit therefore never observes a half-rebuilt hierarchy, and
    /// replay never races the sequence advance. Never call `submit` from within
    /// `replay` or vice versa (both take the same gate, which would deadlock).
    ///
    /// The gate guarantees safety, not reproducibility, for replays on a live
    /// instance: replaying into a book that already holds state or is
    /// concurrently receiving submits is well-formed but yields an outcome
    /// that depends on where the replay lands in the submit stream. The
    /// deterministic contract — the one the replay-equals-live oracle tests —
    /// is replaying a journal prefix into a *fresh*, identically-configured
    /// (and identically-clocked) instance.
    ///
    /// Each event's command is re-executed against the underlying order book.
    /// Replay re-runs the `AddOrder` / `CancelOrder` / `MassCancel` stream
    /// exactly as it was recorded: every `AddOrder` deterministically vivifies
    /// its target expiration and strike from the parsed symbol via the atomic
    /// `get_or_create_*` path (see
    /// `find_or_create_book_by_symbol`),
    /// so replaying a journal prefix into a freshly constructed book rebuilds
    /// the same resting orders and the same top-of-book per contract. The
    /// sequencer is then advanced past the highest replayed sequence number so
    /// that new commands receive non-conflicting ids.
    ///
    /// # Equality oracle
    ///
    /// Replayed state equals live state on the same command prefix at the level
    /// of *structural / order state* — the set of resting orders (carried by id
    /// in each `AddOrder` command) and the top-of-book on each contract.
    /// Registry-assigned instrument ids are allocated at strike creation and are
    /// NOT journaled. They are therefore not preserved when replaying into a
    /// non-empty or differently-seeded registry, but they ARE reconstructed
    /// deterministically when the same journal is replayed into a FRESH registry,
    /// because strike creation runs in the identical command order on replay as
    /// live. The replay==live oracle exercises exactly this fresh-registry case
    /// and so does assert id equality. For a fresh book to
    /// rebuild identical structural state it must be configured identically
    /// (validation / contract specs / STP mode / fee schedule), since those
    /// shared settings are applied to books created during replay.
    ///
    /// Re-execution results are intentionally discarded: this method is a
    /// state-rebuild tool, not a validation tool. The authoritative result of
    /// each command is the one already recorded in the journal at live time.
    ///
    /// **Instrument-status transitions are reconstructed when journaled.** A
    /// status change submitted as an
    /// [`OptionChainCommand::SetInstrumentStatus`] (via
    /// [`submit_set_instrument_status`](Self::submit_set_instrument_status)) is
    /// part of the replayable stream: replay re-applies it through the leaf
    /// book's [`set_status`](crate::orderbook::OptionOrderBook::set_status), so a
    /// strike the live run halted / set to settling / expired replays into the
    /// same status rather than defaulting to
    /// [`Active`](crate::orderbook::InstrumentStatus). Consequently an `AddOrder`
    /// the live run rejected because its strike had been halted is rejected on
    /// replay too, keeping replayed state equal to live.
    ///
    /// **Out-of-band status / lifecycle mutations are still not reconstructed.**
    /// Status changes applied directly to the hierarchy — calling `halt` /
    /// `resume` / `set_status` / `expire` on a leaf book outside the sequencer,
    /// or the expiry-lifecycle manager advancing books on its own schedule — are
    /// not journaled `OptionChainCommand`s, so replay does not reproduce them.
    /// Route status changes through the sequencer to keep replay faithful.
    ///
    /// **Expiry sweeps are reconstructed when journaled.** An
    /// [`OptionChainCommand::EvictExpiredOrders`] (via
    /// [`submit_evict_expired_orders`](Self::submit_evict_expired_orders))
    /// re-applies the journaled `now_ms` — never the replay clock — so the sweep
    /// evicts exactly the orders it evicted live; the sweep is idempotent, so a
    /// duplicate replay is a no-op.
    ///
    /// **Replaces are reconstructed when journaled.** An
    /// [`OptionChainCommand::ReplaceOrder`] (via
    /// [`submit_replace_order`](Self::submit_replace_order)) replays through the
    /// SAME non-creating resolution and the same engine validate-first replace
    /// path (the leaf
    /// [`replace_order`](crate::orderbook::OptionOrderBook::replace_order)) as
    /// live, so a replace that re-priced an order to a crossing level and
    /// rematched live rematches identically on replay, rebuilding the same
    /// resting book. Journaled `trade` payloads carried in
    /// [`OptionChainResult::OrderAdded`] results are ignored by replay: replay
    /// re-executes commands and keeps the freshly produced results, so the
    /// non-replay-stable trade ids / engine-seqs in the journal never influence
    /// the rebuilt state.
    ///
    /// Returns the number of events replayed.
    ///
    /// # Errors
    ///
    /// Returns an error if the journal cannot be read.
    pub fn replay(&self, from_sequence: u64) -> Result<usize, Error> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| Error::journal_error("replay requires a journal"))?;

        // Hold the same gate `submit` uses so a replay cannot interleave with a
        // concurrent live submission: the rebuild runs as one critical section
        // and no submit can observe a half-rebuilt hierarchy or race the
        // sequence advance. Acquired after the journal-presence check so a
        // journal-less book fails fast without taking the gate.
        let _serialized = self.sequencer.acquire_gate();

        let events = journal.read_from(from_sequence)?;
        let count = events.len();

        let mut max_next: u64 = 0;

        for event in &events {
            // Re-execute command to rebuild order book state.
            // Results are discarded — see method doc for rationale.
            let _ = self.execute_command(&event.command);

            // checked_add, not saturating_add: a sequence number at u64::MAX is
            // a protocol-state corruption that must fail loudly, never silently
            // stall the sequence.
            let next = event.sequence_num.checked_add(1).ok_or_else(|| {
                Error::journal_error("replay sequence number overflow at u64::MAX")
            })?;
            if next > max_next {
                max_next = next;
            }
        }

        // Advance sequencer past the replayed range in a single atomic
        // operation instead of one fetch_max per event.
        if max_next > 0 {
            self.sequencer.advance_to(max_next);
        }

        Ok(count)
    }

    // ── Delegated Read Operations ────────────────────────────────────────

    /// Returns the underlying symbol.
    #[must_use]
    #[inline]
    pub fn underlying_symbol(&self) -> &str {
        self.inner.underlying()
    }

    /// Returns the number of expirations.
    #[must_use]
    #[inline]
    pub fn expiration_count(&self) -> usize {
        self.inner.expiration_count()
    }

    /// Returns the total order count.
    #[must_use]
    #[inline]
    pub fn total_order_count(&self) -> usize {
        self.inner.total_order_count()
    }

    /// Returns a summary of terminal order transitions.
    #[must_use]
    #[inline]
    pub fn terminal_order_summary(&self) -> TerminalOrderSummary {
        self.inner.terminal_order_summary()
    }
}

impl std::fmt::Debug for SequencedUnderlyingOrderBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SequencedUnderlyingOrderBook")
            .field("underlying", &self.inner.underlying())
            .field("sequence", &self.sequencer.current_sequence())
            .field("has_journal", &self.journal.is_some())
            .finish()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequencer_assigns_monotonic_sequence() {
        let seq = OptionChainSequencer::new();
        let (s1, _) = seq.assign();
        let (s2, _) = seq.assign();
        let (s3, _) = seq.assign();

        assert_eq!(s1, 0);
        assert_eq!(s2, 1);
        assert_eq!(s3, 2);
    }

    #[test]
    fn test_sequencer_with_start_sequence() {
        let seq = OptionChainSequencer::with_start_sequence(100);
        let (s1, _) = seq.assign();
        let (s2, _) = seq.assign();

        assert_eq!(s1, 100);
        assert_eq!(s2, 101);
    }

    #[test]
    fn test_sequencer_metrics() {
        let seq = OptionChainSequencer::new();

        seq.record_success();
        seq.record_success();
        seq.record_reject();

        assert_eq!(seq.success_count(), 2);
        assert_eq!(seq.reject_count(), 1);
    }

    #[test]
    fn test_option_chain_result_is_error() {
        let success = OptionChainResult::OrderAdded {
            order_id: OrderId::new(),
            trade: None,
        };
        let rejected = OptionChainResult::Rejected {
            reason: "test".to_string(),
        };
        let not_found = OptionChainResult::BookNotFound {
            symbol: "BTC".to_string(),
        };

        assert!(!success.is_error());
        assert!(rejected.is_error());
        assert!(not_found.is_error());
    }

    #[test]
    fn test_sequenced_book_creation() {
        let book = SequencedUnderlyingOrderBook::new("BTC");

        assert_eq!(book.underlying_symbol(), "BTC");
        assert_eq!(book.current_sequence(), 0);
        assert!(!book.has_journal());
    }

    #[test]
    fn test_mass_cancel_scope_serialization() {
        let scope = MassCancelScope::Underlying;
        let json = serde_json::to_string(&scope).expect("serialize");
        assert!(json.contains("Underlying"));
    }

    // ── InMemoryOptionChainJournal ──────────────────────────────────────

    #[test]
    fn test_in_memory_journal_empty() {
        let journal = InMemoryOptionChainJournal::new();
        assert!(journal.is_empty());
        assert_eq!(journal.len(), 0);
        assert_eq!(journal.last_sequence(), None);
    }

    #[test]
    fn test_in_memory_journal_append_and_read() {
        let journal = InMemoryOptionChainJournal::new();

        let event = OptionChainEvent {
            sequence_num: 0,
            timestamp_ns: 1000,
            command: OptionChainCommand::CancelOrder {
                symbol: "BTC-20240329-50000-C".to_string(),
                order_id: OrderId::new(),
            },
            result: OptionChainResult::Rejected {
                reason: "test".to_string(),
            },
        };

        journal.append(&event).expect("append");
        assert_eq!(journal.len(), 1);
        assert_eq!(journal.last_sequence(), Some(0));

        let events = journal.read_from(0).expect("read");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence_num, 0);
    }

    #[test]
    fn test_in_memory_journal_read_from_filters() {
        let journal = InMemoryOptionChainJournal::new();

        for i in 0..5 {
            let event = OptionChainEvent {
                sequence_num: i,
                timestamp_ns: i * 1000,
                command: OptionChainCommand::CancelOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: OrderId::new(),
                },
                result: OptionChainResult::Rejected {
                    reason: "test".to_string(),
                },
            };
            journal.append(&event).expect("append");
        }

        assert_eq!(journal.len(), 5);
        assert_eq!(journal.last_sequence(), Some(4));

        let from_2 = journal.read_from(2).expect("read");
        assert_eq!(from_2.len(), 3); // seq 2, 3, 4

        let from_10 = journal.read_from(10).expect("read");
        assert!(from_10.is_empty());
    }

    #[test]
    fn test_in_memory_journal_entry_count_empty() {
        let journal = InMemoryOptionChainJournal::new();
        assert_eq!(journal.entry_count().expect("count"), Some(0));
    }

    #[test]
    fn test_in_memory_journal_entry_count_with_entries() {
        let journal = InMemoryOptionChainJournal::new();

        for i in 0..5 {
            let event = OptionChainEvent {
                sequence_num: i,
                timestamp_ns: i * 1000,
                command: OptionChainCommand::CancelOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: OrderId::new(),
                },
                result: OptionChainResult::Rejected {
                    reason: "test".to_string(),
                },
            };
            journal.append(&event).expect("append");
        }

        assert_eq!(journal.entry_count().expect("count"), Some(5));
    }

    #[test]
    fn test_in_memory_journal_read_from_with_limit() {
        let journal = InMemoryOptionChainJournal::new();

        for i in 0..10 {
            let event = OptionChainEvent {
                sequence_num: i,
                timestamp_ns: i * 1000,
                command: OptionChainCommand::CancelOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: OrderId::new(),
                },
                result: OptionChainResult::Rejected {
                    reason: "test".to_string(),
                },
            };
            journal.append(&event).expect("append");
        }

        // Read with limit smaller than available
        let limited = journal.read_from_with_limit(0, 3).expect("read");
        assert_eq!(limited.len(), 3);
        assert_eq!(limited[0].sequence_num, 0);
        assert_eq!(limited[2].sequence_num, 2);

        // Read with limit larger than available
        let all = journal.read_from_with_limit(0, 100).expect("read");
        assert_eq!(all.len(), 10);

        // Read with offset and limit
        let offset = journal.read_from_with_limit(5, 3).expect("read");
        assert_eq!(offset.len(), 3);
        assert_eq!(offset[0].sequence_num, 5);
        assert_eq!(offset[2].sequence_num, 7);

        // Read beyond end returns empty
        let empty = journal.read_from_with_limit(100, 5).expect("read");
        assert!(empty.is_empty());
    }

    // ── Journaled sequenced book ────────────────────────────────────────

    #[test]
    fn test_sequenced_book_with_journal() {
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let book = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));

        assert!(book.has_journal());

        // A valid AddOrder vivifies its expiration+strike (option (b)), so it
        // succeeds even on a fresh book. The event is journaled regardless.
        let receipt = book
            .submit_add_order("BTC-20240329-50000-C", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");

        assert!(receipt.result.is_success());
        assert_eq!(receipt.sequence_num, 0);

        // Event should be in the journal
        assert_eq!(journal.last_sequence(), Some(0));
        let events = journal.read_from(0).expect("read");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_replay_without_journal_errors() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        let result = book.replay(0);
        assert!(result.is_err());
    }

    #[test]
    fn test_replay_empty_journal() {
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let book = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));

        let count = book.replay(0).expect("replay");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_replay_advances_sequence() {
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());

        // Pre-populate journal with events
        for i in 0..3 {
            let event = OptionChainEvent {
                sequence_num: i,
                timestamp_ns: i * 1000,
                command: OptionChainCommand::CancelOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: OrderId::new(),
                },
                result: OptionChainResult::BookNotFound {
                    symbol: "BTC-20240329-50000-C".to_string(),
                },
            };
            journal.append(&event).expect("append");
        }

        let book = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));
        assert_eq!(book.current_sequence(), 0);

        let count = book.replay(0).expect("replay");
        assert_eq!(count, 3);

        // Sequence should be advanced past the replayed range
        assert!(book.current_sequence() >= 3);
    }

    #[test]
    fn test_replay_sequence_at_capacity_errors_not_caps() {
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());

        // A journaled event at u64::MAX must make replay fail loudly via
        // checked_add(1), not silently saturate the next sequence.
        let event = OptionChainEvent {
            sequence_num: u64::MAX,
            timestamp_ns: 0,
            command: OptionChainCommand::CancelOrder {
                symbol: "BTC-20240329-50000-C".to_string(),
                order_id: OrderId::new(),
            },
            result: OptionChainResult::BookNotFound {
                symbol: "BTC-20240329-50000-C".to_string(),
            },
        };
        journal.append(&event).expect("append");

        let book = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));
        let result = book.replay(0);
        assert!(
            result.is_err(),
            "replay of a u64::MAX sequence must error, not cap"
        );
        assert!(
            result.unwrap_err().to_string().contains("overflow"),
            "expected a sequence-overflow error"
        );
    }

    #[test]
    fn test_replay_rebuilds_resting_orders_and_top_of_book_deterministically() {
        // Probe the structural state of one contract: top-of-book on each side,
        // the count of resting orders, and the per-contract instrument status.
        // This — not the registry-assigned instrument ids — is the equality
        // oracle for live-vs-replayed state. Status is included so a journaled
        // SetInstrumentStatus transition is part of the equality contract.
        fn probe(
            book: &SequencedUnderlyingOrderBook,
            symbol: &str,
        ) -> (Option<u128>, Option<u128>, usize, InstrumentStatus) {
            let leaf = book
                .find_book_by_symbol(symbol)
                .expect("contract must resolve");
            (
                leaf.best_bid(),
                leaf.best_ask(),
                leaf.order_count(),
                leaf.status(),
            )
        }

        // Contracts across two expirations and three strikes, calls and puts.
        let sym_50c = "BTC-20240329-50000-C";
        let sym_50p = "BTC-20240329-50000-P";
        let sym_55c = "BTC-20240329-55000-C";
        let sym_60c = "BTC-20240628-60000-C";
        let symbols = [sym_50c, sym_50p, sym_55c, sym_60c];

        // Canonical expiration for the 55000 strike, used to scope a mass cancel.
        let exp_0329 = *SymbolParser::parse(sym_55c)
            .expect("parse 55c")
            .expiration();

        // ── Live run: build the journal via the full submit path ──
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let live = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));

        // Order ids are carried by the commands, so they replay identically.
        let oid_50c_buy = OrderId::new();
        let oid_50c_sell = OrderId::new();
        let oid_50p_buy = OrderId::new();
        let oid_55c_buy = OrderId::new();
        let oid_60c_sell = OrderId::new();

        live.submit_add_order(sym_50c, oid_50c_buy, Side::Buy, 100, 10)
            .expect("add 50c buy");
        live.submit_add_order(sym_50c, oid_50c_sell, Side::Sell, 110, 5)
            .expect("add 50c sell");
        live.submit_add_order(sym_50p, oid_50p_buy, Side::Buy, 50, 8)
            .expect("add 50p buy");
        live.submit_add_order(sym_55c, oid_55c_buy, Side::Buy, 120, 4)
            .expect("add 55c buy");
        live.submit_add_order(sym_60c, oid_60c_sell, Side::Sell, 200, 3)
            .expect("add 60c sell");

        // Cancel one resting order: 50c now rests bid-only.
        live.submit_cancel_order(sym_50c, oid_50c_sell)
            .expect("cancel 50c sell");

        // Mass cancel the whole 55000 strike: 55c becomes empty.
        live.submit_mass_cancel(
            MassCancelScope::Strike {
                expiration: exp_0329,
                strike: 55000,
            },
            MassCancelType::All,
        )
        .expect("mass cancel 55000");

        // Halt the 50p contract AFTER its order rested. Halt does not cancel
        // resting orders, so 50p keeps its bid but stops accepting new orders.
        // This journaled status transition must be reconstructed on replay.
        let status_receipt = live
            .submit_set_instrument_status(sym_50p, InstrumentStatus::Halted)
            .expect("halt 50p");
        assert!(status_receipt.result.is_success(), "{status_receipt:?}");

        // Expected live structure after the stream:
        //   50c: bid 100, no ask, 1 order, Active
        //   50p: bid 50,  no ask, 1 order, Halted
        //   55c: empty, Active
        //   60c: no bid, ask 200, 1 order, Active
        assert_eq!(
            probe(&live, sym_50c),
            (Some(100), None, 1, InstrumentStatus::Active)
        );
        assert_eq!(
            probe(&live, sym_50p),
            (Some(50), None, 1, InstrumentStatus::Halted)
        );
        assert_eq!(
            probe(&live, sym_55c),
            (None, None, 0, InstrumentStatus::Active)
        );
        assert_eq!(
            probe(&live, sym_60c),
            (None, Some(200), 1, InstrumentStatus::Active)
        );
        assert_eq!(live.expiration_count(), 2);
        assert_eq!(live.total_order_count(), 3);

        let event_count = journal.read_from(0).expect("read").len();

        // ── Fresh book, configured identically (same constructor), replays ──
        let replay_a = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));
        // The fresh book starts empty: replay must rebuild everything.
        assert_eq!(replay_a.expiration_count(), 0);
        assert_eq!(replay_a.total_order_count(), 0);

        let replayed = replay_a.replay(0).expect("replay a");
        assert_eq!(replayed, event_count);

        // Per-contract structural state matches the live book exactly.
        for sym in symbols {
            assert_eq!(
                probe(&live, sym),
                probe(&replay_a, sym),
                "replayed contract {sym} diverged from live"
            );
        }
        // Structural completeness: same expirations and same total resting count.
        assert_eq!(live.expiration_count(), replay_a.expiration_count());
        assert_eq!(live.total_order_count(), replay_a.total_order_count());

        // ── Determinism: a second fresh replay is byte-for-byte structural ──
        let replay_b = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));
        let replayed_b = replay_b.replay(0).expect("replay b");
        assert_eq!(replayed_b, event_count);

        for sym in symbols {
            assert_eq!(
                probe(&replay_a, sym),
                probe(&replay_b, sym),
                "second replay diverged for contract {sym}"
            );
        }
        assert_eq!(replay_a.expiration_count(), replay_b.expiration_count());
        assert_eq!(replay_a.total_order_count(), replay_b.total_order_count());
    }

    // ── Instrument-status transitions (issue #94) ──────────────────────────

    #[test]
    fn test_halt_then_add_order_replays_rejection() {
        // The core #94 scenario: a strike halted by a journaled
        // SetInstrumentStatus must replay Halted, so an AddOrder the live run
        // rejected (InstrumentNotActive) is rejected on replay too — replayed
        // state equals live instead of vivifying a fresh Active strike that
        // accepts the order.
        let sym = "BTC-20240329-50000-C";
        let oid = OrderId::new();

        // ── Live run ──
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let live = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));

        // Halt the strike: vivifies it (Active) then transitions Active -> Halted.
        let halt = live
            .submit_set_instrument_status(sym, InstrumentStatus::Halted)
            .expect("submit halt");
        match &halt.result {
            OptionChainResult::StatusChanged { symbol, status } => {
                assert_eq!(symbol, sym);
                assert_eq!(*status, InstrumentStatus::Halted);
            }
            other => panic!("expected StatusChanged, got {other:?}"),
        }

        // AddOrder against the halted strike: rejected with InstrumentNotActive.
        let add = live
            .submit_add_order(sym, oid, Side::Buy, 100, 10)
            .expect("submit add");
        match &add.result {
            OptionChainResult::Rejected { reason } => {
                assert!(
                    reason.contains("instrument not active"),
                    "expected InstrumentNotActive, got: {reason}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }

        // Live state: strike Halted, no resting order.
        let live_leaf = live.find_book_by_symbol(sym).expect("resolve live");
        assert_eq!(live_leaf.status(), InstrumentStatus::Halted);
        assert_eq!(live_leaf.order_count(), 0);
        assert_eq!(live.total_order_count(), 0);

        // ── Replay into a fresh, identically configured book ──
        let replay = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));
        assert_eq!(replay.total_order_count(), 0);
        let replayed = replay.replay(0).expect("replay");
        assert_eq!(replayed, 2, "two journaled events: halt + rejected add");

        // Replay reconstructs Halted (NOT the default Active), so the AddOrder is
        // rejected on replay too: no resting order, status matches live.
        let replay_leaf = replay.find_book_by_symbol(sym).expect("resolve replay");
        assert_eq!(replay_leaf.status(), InstrumentStatus::Halted);
        assert_eq!(replay_leaf.order_count(), 0);

        // Per-contract status + resting orders + total match live exactly.
        assert_eq!(live_leaf.status(), replay_leaf.status());
        assert_eq!(live_leaf.order_count(), replay_leaf.order_count());
        assert_eq!(live.total_order_count(), replay.total_order_count());
    }

    #[test]
    fn test_replay_reconstructs_instrument_status() {
        // Replaying a journal that contains a sequence of status transitions
        // reconstructs the final per-contract status deterministically.
        let sym = "BTC-20240329-50000-C";
        let oid = OrderId::new();

        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let live = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));

        // Rest an order while Active, then walk the lifecycle: halt, resume,
        // settle. None of these cancel the resting order.
        live.submit_add_order(sym, oid, Side::Buy, 100, 10)
            .expect("add");
        live.submit_set_instrument_status(sym, InstrumentStatus::Halted)
            .expect("halt");
        live.submit_set_instrument_status(sym, InstrumentStatus::Active)
            .expect("resume");
        live.submit_set_instrument_status(sym, InstrumentStatus::Settling)
            .expect("settle");

        let live_leaf = live.find_book_by_symbol(sym).expect("resolve live");
        assert_eq!(live_leaf.status(), InstrumentStatus::Settling);
        assert_eq!(live_leaf.order_count(), 1);

        // Fresh book replays the same status path to the same final status.
        let replay = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));
        replay.replay(0).expect("replay");

        let replay_leaf = replay.find_book_by_symbol(sym).expect("resolve replay");
        assert_eq!(replay_leaf.status(), InstrumentStatus::Settling);
        assert_eq!(replay_leaf.order_count(), 1);
        assert_eq!(live_leaf.status(), replay_leaf.status());
    }

    #[test]
    fn test_set_instrument_status_illegal_transition_rejected() {
        // An illegal lifecycle edge is rejected and leaves the status unchanged.
        let sym = "BTC-20240329-50000-C";

        let book = SequencedUnderlyingOrderBook::new("BTC");

        // Vivify Active, then expire (Active -> Expired is legal).
        let expire = book
            .submit_set_instrument_status(sym, InstrumentStatus::Expired)
            .expect("submit expire");
        assert!(expire.result.is_success(), "{:?}", expire.result);

        // Expired is terminal: resuming it (Expired -> Active) is illegal.
        let illegal = book
            .submit_set_instrument_status(sym, InstrumentStatus::Active)
            .expect("submit illegal");
        match &illegal.result {
            OptionChainResult::Rejected { reason } => {
                assert!(
                    reason.contains("illegal status transition"),
                    "expected IllegalStatusTransition, got: {reason}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }

        // The book remains Expired — the rejected transition was a no-op.
        let leaf = book.find_book_by_symbol(sym).expect("resolve");
        assert_eq!(leaf.status(), InstrumentStatus::Expired);

        // Counters reflect one success (expire) and one reject (illegal resume).
        assert_eq!(book.success_count(), 1);
        assert_eq!(book.reject_count(), 1);
    }

    #[test]
    fn test_set_instrument_status_cross_underlying_rejected() {
        // A cross-underlying symbol is rejected by the underlying-mismatch check
        // before any book is materialized.
        let book = SequencedUnderlyingOrderBook::new("BTC");

        let receipt = book
            .submit_set_instrument_status("ETH-20240329-50000-C", InstrumentStatus::Halted)
            .expect("submit");
        match &receipt.result {
            OptionChainResult::Rejected { reason } => {
                assert!(reason.contains("underlying mismatch"), "reason: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        // Nothing was created for the foreign symbol.
        assert_eq!(book.expiration_count(), 0);
    }

    #[test]
    fn test_set_instrument_status_command_result_wire_shape() {
        // The new variants inherit the journal container attrs: PascalCase
        // variant tags, snake_case struct fields. The InstrumentStatus value is
        // its own variant name ("Halted").
        let command = OptionChainCommand::SetInstrumentStatus {
            symbol: "BTC-20240329-50000-C".to_string(),
            status: InstrumentStatus::Halted,
        };
        let cmd_json: serde_json::Value =
            serde_json::to_value(&command).expect("serialize command");
        let expected_cmd = serde_json::json!({
            "SetInstrumentStatus": {
                "symbol": "BTC-20240329-50000-C",
                "status": "Halted"
            }
        });
        assert_eq!(cmd_json, expected_cmd);

        let result = OptionChainResult::StatusChanged {
            symbol: "BTC-20240329-50000-C".to_string(),
            status: InstrumentStatus::Settling,
        };
        let res_json: serde_json::Value = serde_json::to_value(&result).expect("serialize result");
        let expected_res = serde_json::json!({
            "StatusChanged": {
                "symbol": "BTC-20240329-50000-C",
                "status": "Settling"
            }
        });
        assert_eq!(res_json, expected_res);

        // StatusChanged is a success, not an error.
        assert!(result.is_success());
        assert!(!result.is_error());

        // Round-trip both back through their typed forms.
        let cmd_back: OptionChainCommand =
            serde_json::from_value(cmd_json).expect("deserialize command");
        assert!(matches!(
            cmd_back,
            OptionChainCommand::SetInstrumentStatus {
                status: InstrumentStatus::Halted,
                ..
            }
        ));
        let res_back: OptionChainResult =
            serde_json::from_value(res_json).expect("deserialize result");
        assert!(matches!(
            res_back,
            OptionChainResult::StatusChanged {
                status: InstrumentStatus::Settling,
                ..
            }
        ));
    }

    #[test]
    fn test_event_serialization_roundtrip() {
        let event = OptionChainEvent {
            sequence_num: 42,
            timestamp_ns: 1_000_000,
            command: OptionChainCommand::AddOrder {
                symbol: "BTC-20240329-50000-C".to_string(),
                order_id: OrderId::new(),
                side: Side::Buy,
                price: 100,
                quantity: 10,
                tif: TimeInForce::Gtc,
                user_id: Hash32::zero(),
            },
            result: OptionChainResult::OrderAdded {
                order_id: OrderId::new(),
                trade: None,
            },
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: OptionChainEvent = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.sequence_num, 42);
        assert_eq!(deserialized.timestamp_ns, 1_000_000);
    }

    // ── Expiry sweep command (issue #144) ─────────────────────────────────

    // Far-future GTD deadline (Unix ms): admission reads the real wall clock and
    // accepts it, while the sweep — driven purely by the caller-supplied
    // `now_ms` — treats it as expired at `now_ms == GTD_EXPIRED`.
    const GTD_EXPIRED: u64 = 10_000_000_000_000;

    /// Seeds GTD orders across two strikes (call + put) of one expiration so the
    /// flatten walks multiple leaves. Returns the evicted-id order the sweep MUST
    /// produce: expirations by key, strikes ascending, call book before put, and
    /// within each leaf bids-ascending then asks.
    fn seed_expiring_orders(underlying: &UnderlyingOrderBook) -> Vec<OrderId> {
        let expiry = SymbolParser::parse_yyyymmdd("20240329", "BTC-20240329-50000-C")
            .expect("canonical expiry");
        let exp_book = underlying.get_or_create_expiration(expiry);

        // Strike 50000: call has a bid (100) and an ask (110); put has a bid (50).
        let s50 = exp_book.get_or_create_strike(50000);
        let c50_bid = OrderId::sequential(1001);
        let c50_ask = OrderId::sequential(1002);
        let p50_bid = OrderId::sequential(1003);
        s50.call()
            .add_limit_order_with_tif(
                c50_bid,
                Side::Buy,
                100,
                10,
                crate::TimeInForce::Gtd(GTD_EXPIRED),
            )
            .expect("seed 50c bid");
        s50.call()
            .add_limit_order_with_tif(
                c50_ask,
                Side::Sell,
                110,
                5,
                crate::TimeInForce::Gtd(GTD_EXPIRED),
            )
            .expect("seed 50c ask");
        s50.put()
            .add_limit_order_with_tif(
                p50_bid,
                Side::Buy,
                50,
                8,
                crate::TimeInForce::Gtd(GTD_EXPIRED),
            )
            .expect("seed 50p bid");

        // Strike 55000: call has a single bid (120).
        let s55 = exp_book.get_or_create_strike(55000);
        let c55_bid = OrderId::sequential(1004);
        s55.call()
            .add_limit_order_with_tif(
                c55_bid,
                Side::Buy,
                120,
                4,
                crate::TimeInForce::Gtd(GTD_EXPIRED),
            )
            .expect("seed 55c bid");

        // Deterministic sweep order: strike 50000 (call bids-then-asks, then put),
        // then strike 55000.
        vec![c50_bid, c50_ask, p50_bid, c55_bid]
    }

    #[test]
    fn test_evict_expired_orders_command_reports_deterministic_ids() {
        let underlying = UnderlyingOrderBook::new("BTC");
        let expected = seed_expiring_orders(&underlying);

        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let book = SequencedUnderlyingOrderBook::from_underlying_with_journal(
            underlying,
            Arc::clone(&journal),
        );
        assert_eq!(book.total_order_count(), 4, "four GTD orders seeded");

        let receipt = book
            .submit_evict_expired_orders(TimestampMs::new(GTD_EXPIRED))
            .expect("submit evict");
        assert!(receipt.result.is_success());

        // The command reports every evicted id in the hierarchy's deterministic
        // sweep order — not insertion order.
        match &receipt.result {
            OptionChainResult::ExpiredEvicted { evicted_ids } => {
                assert_eq!(*evicted_ids, expected, "evicted ids out of sweep order");
            }
            other => panic!("expected ExpiredEvicted, got {other:?}"),
        }
        assert_eq!(book.total_order_count(), 0, "all GTD orders evicted");

        // The journaled event carries the same result shape.
        let events = journal.read_from(0).expect("read");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].command,
            OptionChainCommand::EvictExpiredOrders { .. }
        ));
        match &events[0].result {
            OptionChainResult::ExpiredEvicted { evicted_ids } => {
                assert_eq!(*evicted_ids, expected);
            }
            other => panic!("expected journaled ExpiredEvicted, got {other:?}"),
        }

        // Idempotent: a second sweep at the same instant evicts nothing.
        let again = book
            .submit_evict_expired_orders(TimestampMs::new(GTD_EXPIRED))
            .expect("submit evict again");
        match &again.result {
            OptionChainResult::ExpiredEvicted { evicted_ids } => assert!(evicted_ids.is_empty()),
            other => panic!("expected empty ExpiredEvicted, got {other:?}"),
        }
    }

    #[test]
    fn test_replay_evict_expired_orders_matches_live_and_payload_roundtrips() {
        // Probe structural state of one contract: top-of-book both sides and the
        // resting-order count. This is the replay-equality oracle.
        fn probe(
            book: &SequencedUnderlyingOrderBook,
            symbol: &str,
        ) -> (Option<u128>, Option<u128>, usize) {
            let leaf = book
                .find_book_by_symbol(symbol)
                .expect("contract must resolve");
            (leaf.best_bid(), leaf.best_ask(), leaf.order_count())
        }

        // GTC survivors that bracket the sweep; the GTD orders are seeded
        // out-of-band (they model resting state a checkpoint already held, since
        // AddOrder journals only GTC) and are recreated identically on replay.
        let sym_gtc_before = "BTC-20240329-60000-C";
        let sym_gtc_after = "BTC-20240329-65000-P";
        let oid_before = OrderId::new();
        let oid_after = OrderId::new();

        // ── Live run: seed GTD, then journal add / evict / add ──
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let live_underlying = UnderlyingOrderBook::new("BTC");
        let expected_evicted = seed_expiring_orders(&live_underlying);
        let live = SequencedUnderlyingOrderBook::from_underlying_with_journal(
            live_underlying,
            Arc::clone(&journal),
        );

        live.submit_add_order(sym_gtc_before, oid_before, Side::Buy, 100, 10)
            .expect("gtc add before");
        let evict_receipt = live
            .submit_evict_expired_orders(TimestampMs::new(GTD_EXPIRED))
            .expect("evict");
        live.submit_add_order(sym_gtc_after, oid_after, Side::Sell, 200, 5)
            .expect("gtc add after");

        // The evict receipt reports the seeded GTD orders in deterministic order.
        match &evict_receipt.result {
            OptionChainResult::ExpiredEvicted { evicted_ids } => {
                assert_eq!(*evicted_ids, expected_evicted);
            }
            other => panic!("expected ExpiredEvicted, got {other:?}"),
        }

        // Contracts to compare: the swept ones (now empty) and the GTC survivors.
        let symbols = [
            "BTC-20240329-50000-C",
            "BTC-20240329-50000-P",
            "BTC-20240329-55000-C",
            sym_gtc_before,
            sym_gtc_after,
        ];

        // ── Replay: seed the SAME GTD state, then replay the journal ──
        let replay_underlying = UnderlyingOrderBook::new("BTC");
        let _ = seed_expiring_orders(&replay_underlying);
        let replay = SequencedUnderlyingOrderBook::from_underlying_with_journal(
            replay_underlying,
            Arc::clone(&journal),
        );
        let replayed = replay.replay(0).expect("replay");
        assert_eq!(replayed, 3, "three journaled events: add, evict, add");

        // Final state equality: the journaled EvictExpiredOrders re-applied the
        // same `now_ms`, so replay evicted the same GTD orders and kept the GTCs.
        for sym in symbols {
            assert_eq!(
                probe(&live, sym),
                probe(&replay, sym),
                "replayed contract {sym} diverged from live"
            );
        }
        assert_eq!(live.total_order_count(), replay.total_order_count());
        assert_eq!(live.total_order_count(), 2, "two GTC survivors remain");

        // ── The evicted-ids payload round-trips byte-identically ──
        let events = journal.read_from(0).expect("read");
        let evict_event = events
            .iter()
            .find(|e| matches!(e.command, OptionChainCommand::EvictExpiredOrders { .. }))
            .expect("evict event journaled");
        let json = serde_json::to_string(&evict_event.result).expect("serialize");
        let back: OptionChainResult = serde_json::from_str(&json).expect("deserialize");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json, json2, "evicted-ids payload changed across round-trip");
        match back {
            OptionChainResult::ExpiredEvicted { evicted_ids } => {
                assert_eq!(evicted_ids, expected_evicted);
            }
            other => panic!("expected ExpiredEvicted after round-trip, got {other:?}"),
        }
    }

    // ── Journal format pinning (deny_unknown_fields + back-compat) ──────────

    /// Builds one representative `OptionChainEvent` per command variant and per
    /// result variant, using deterministic ids so the encoding is stable. This
    /// drives the round-trip guard
    /// ([`test_option_chain_event_roundtrip_all_variants`]).
    ///
    /// The first six events mirror the frozen `v0.5.0` back-compat fixture at
    /// `tests/fixtures/journal_event_v0.5.0.json`. The trailing
    /// `SetInstrumentStatus` / `StatusChanged` event was added after that fixture
    /// was frozen: the variant is additive, so it is covered by the round-trip
    /// guard here but is intentionally absent from the frozen fixture, which must
    /// still decode unchanged.
    fn representative_journal_events() -> Vec<OptionChainEvent> {
        // `OrderId::sequential` / a fixed `Hash32` keep the wire encoding stable
        // (random ids would make the JSON non-reproducible).
        let oid = OrderId::sequential(42);
        let user = Hash32::from([7u8; 32]);
        let expiry = SymbolParser::parse_yyyymmdd("20240329", "BTC-20240329-50000-C")
            .expect("canonical expiry");

        vec![
            // AddOrder + OrderAdded (default tif/user, rested unfilled → trade None)
            OptionChainEvent {
                sequence_num: 1,
                timestamp_ns: 1_700_000_000_000_000_000,
                command: OptionChainCommand::AddOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: oid,
                    side: Side::Buy,
                    price: 100,
                    quantity: 10,
                    tif: TimeInForce::Gtc,
                    user_id: Hash32::zero(),
                },
                result: OptionChainResult::OrderAdded {
                    order_id: oid,
                    trade: None,
                },
            },
            // CancelOrder + OrderCancelled
            OptionChainEvent {
                sequence_num: 2,
                timestamp_ns: 1_700_000_000_000_000_001,
                command: OptionChainCommand::CancelOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: oid,
                },
                result: OptionChainResult::OrderCancelled { order_id: oid },
            },
            // MassCancel(Strike, BySide) + MassCancelled
            OptionChainEvent {
                sequence_num: 3,
                timestamp_ns: 1_700_000_000_000_000_002,
                command: OptionChainCommand::MassCancel {
                    scope: MassCancelScope::Strike {
                        expiration: expiry,
                        strike: 50000,
                    },
                    cancel_type: MassCancelType::BySide(Side::Sell),
                },
                result: OptionChainResult::MassCancelled { cancelled_count: 4 },
            },
            // MassCancel(Underlying, ByUser) + Rejected
            OptionChainEvent {
                sequence_num: 4,
                timestamp_ns: 1_700_000_000_000_000_003,
                command: OptionChainCommand::MassCancel {
                    scope: MassCancelScope::Underlying,
                    cancel_type: MassCancelType::ByUser(user),
                },
                result: OptionChainResult::Rejected {
                    reason: "boom".to_string(),
                },
            },
            // MassCancel(Book, All) + BookNotFound
            OptionChainEvent {
                sequence_num: 5,
                timestamp_ns: 1_700_000_000_000_000_004,
                command: OptionChainCommand::MassCancel {
                    scope: MassCancelScope::Book("BTC-20240329-50000-C".to_string()),
                    cancel_type: MassCancelType::All,
                },
                result: OptionChainResult::BookNotFound {
                    symbol: "ETH-20240329-50000-C".to_string(),
                },
            },
            // MassCancel(Expiration, All) + MassCancelled
            OptionChainEvent {
                sequence_num: 6,
                timestamp_ns: 1_700_000_000_000_000_005,
                command: OptionChainCommand::MassCancel {
                    scope: MassCancelScope::Expiration(expiry),
                    cancel_type: MassCancelType::All,
                },
                result: OptionChainResult::MassCancelled { cancelled_count: 0 },
            },
            // SetInstrumentStatus + StatusChanged (added after the v0.5.0 fixture
            // was frozen; covered here but intentionally absent from the fixture).
            OptionChainEvent {
                sequence_num: 7,
                timestamp_ns: 1_700_000_000_000_000_006,
                command: OptionChainCommand::SetInstrumentStatus {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    status: InstrumentStatus::Halted,
                },
                result: OptionChainResult::StatusChanged {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    status: InstrumentStatus::Halted,
                },
            },
            // EvictExpiredOrders + ExpiredEvicted (0.7.0 breaking release;
            // appended after every prior variant, absent from the v0.5.0 fixture).
            OptionChainEvent {
                sequence_num: 8,
                timestamp_ns: 1_700_000_000_000_000_007,
                command: OptionChainCommand::EvictExpiredOrders {
                    now_ms: TimestampMs::new(10_000_000_000_000),
                },
                result: OptionChainResult::ExpiredEvicted {
                    evicted_ids: vec![OrderId::sequential(42), OrderId::sequential(43)],
                },
            },
            // AddOrder with a NON-default tif (Gtd) + nonzero user_id, and an
            // OrderAdded carrying deterministic fills (#148). Exercises the
            // enriched wire shape end to end.
            OptionChainEvent {
                sequence_num: 9,
                timestamp_ns: 1_700_000_000_000_000_008,
                command: OptionChainCommand::AddOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: oid,
                    side: Side::Buy,
                    price: 100,
                    quantity: 4,
                    tif: TimeInForce::Gtd(1_700_000_000_000),
                    user_id: user,
                },
                result: OptionChainResult::OrderAdded {
                    order_id: oid,
                    trade: Some(deterministic_trade_result()),
                },
            },
            // ReplaceOrder + OrderReplaced (#148).
            OptionChainEvent {
                sequence_num: 10,
                timestamp_ns: 1_700_000_000_000_000_009,
                command: OptionChainCommand::ReplaceOrder {
                    symbol: "BTC-20240329-50000-C".to_string(),
                    order_id: oid,
                    price: 105,
                    quantity: 7,
                    side: Side::Buy,
                },
                result: OptionChainResult::OrderReplaced { order_id: oid },
            },
        ]
    }

    /// Builds a deterministic single-fill [`TradeResult`] for fixtures and wire
    /// tests.
    ///
    /// The inner [`Trade`](pricelevel::Trade) is deserialized from a pinned JSON
    /// literal — never `Trade::new`, which stamps a wall-clock timestamp and
    /// would make the encoding non-reproducible — so the whole `TradeResult`, and
    /// any journal event carrying it, is byte-stable across runs. Represents a
    /// taker of 4 units fully filled at price 100 by maker order 43.
    fn deterministic_trade_result() -> TradeResult {
        // Pinned Trade wire form: `Id`s are strings, price/quantity/timestamp are
        // transparent numbers, `taker_side` is "BUY"/"SELL". The fixed timestamp
        // is what keeps this reproducible (Trade::new would read the clock).
        const TRADE_JSON: &str = r#"{
            "trade_id": "9001",
            "taker_order_id": "42",
            "maker_order_id": "43",
            "price": 100,
            "quantity": 4,
            "taker_side": "BUY",
            "timestamp": 1700000000000
        }"#;
        let trade: pricelevel::Trade =
            serde_json::from_str(TRADE_JSON).expect("pinned trade json must decode");
        let mut match_result =
            pricelevel::MatchResult::new(OrderId::sequential(42), pricelevel::Quantity::new(4));
        match_result
            .add_trade(trade)
            .expect("pinned trade must not overfill the match result");
        TradeResult::new("BTC-20240329-50000-C".to_string(), match_result)
    }

    /// Back-compat / format-pin guard: a checked-in journal sample written under
    /// the current schema MUST still decode into `OptionChainEvent`, and the
    /// re-encoding MUST be value-identical to the on-disk bytes. If a future
    /// change renames a variant tag or a field, or alters the casing, this test
    /// fails loudly instead of silently corrupting replay of older journals.
    ///
    /// The fixture lives at `tests/fixtures/journal_event_v0.5.0.json`.
    #[test]
    fn test_journal_event_v0_5_0_fixture_decodes_and_is_stable() {
        const FIXTURE: &str = include_str!("../../tests/fixtures/journal_event_v0.5.0.json");

        // Decodes without error under the current schema.
        let decoded: Vec<OptionChainEvent> =
            serde_json::from_str(FIXTURE).expect("v0.5.0 journal fixture must decode");
        assert_eq!(decoded.len(), 6, "fixture should hold six events");

        // Spot-check that the variants landed in the expected shapes.
        assert!(matches!(
            decoded[0].command,
            OptionChainCommand::AddOrder { .. }
        ));
        assert!(matches!(
            decoded[0].result,
            OptionChainResult::OrderAdded { .. }
        ));
        assert!(matches!(
            decoded[2].command,
            OptionChainCommand::MassCancel {
                scope: MassCancelScope::Strike { strike: 50000, .. },
                cancel_type: MassCancelType::BySide(Side::Sell),
            }
        ));
        assert!(matches!(
            decoded[3].command,
            OptionChainCommand::MassCancel {
                scope: MassCancelScope::Underlying,
                cancel_type: MassCancelType::ByUser(_),
            }
        ));

        // Re-encoding: the #148 additive fields (`tif` / `user_id` on `AddOrder`,
        // `trade` on `OrderAdded`) are absent from the frozen v0.5.0 fixture but
        // are ALWAYS emitted by the current schema (no `skip_serializing_if`), so
        // a naive `to_value(decoded) == fixture` comparison would fail purely on
        // the new fields. That is expected and correct: the additive-field
        // precedent is that an old journal decodes to the field DEFAULTS, and
        // re-encoding then materializes those defaults. To keep this test a true
        // wire-stability pin (nothing OTHER than the known additive fields
        // changed), we patch the parsed fixture with exactly those defaults —
        // `"tif":"GTC"`, the zero-hex `user_id`, `"trade":null` — and require the
        // patched fixture to match the re-encode byte-for-byte. Any drift in a
        // pre-existing field still fails loudly. The frozen fixture file itself
        // is NOT edited.
        let reencoded: serde_json::Value =
            serde_json::to_value(&decoded).expect("re-encode decoded events");
        let mut on_disk: serde_json::Value =
            serde_json::from_str(FIXTURE).expect("parse fixture as value");

        // Event 0 is the AddOrder + OrderAdded; inject the defaulted fields.
        let add_cmd = on_disk[0]["command"]["AddOrder"]
            .as_object_mut()
            .expect("fixture event 0 command is AddOrder");
        add_cmd.insert("tif".to_string(), serde_json::json!("GTC"));
        add_cmd.insert(
            "user_id".to_string(),
            serde_json::json!(Hash32::zero().to_hex()),
        );
        let added_result = on_disk[0]["result"]["OrderAdded"]
            .as_object_mut()
            .expect("fixture event 0 result is OrderAdded");
        added_result.insert("trade".to_string(), serde_json::Value::Null);

        assert_eq!(
            reencoded, on_disk,
            "re-encoded journal diverged from the checked-in v0.5.0 wire format \
             (beyond the known #148 additive fields)"
        );
    }

    /// Round-trip every command/result variant: serialize, deserialize, and
    /// re-serialize, asserting the two encodings are byte-identical. A lossless
    /// round-trip proves the pinned casing (`PascalCase` variant tags,
    /// `snake_case` fields) is internally consistent.
    #[test]
    fn test_option_chain_event_roundtrip_all_variants() {
        for event in representative_journal_events() {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: OptionChainEvent =
                serde_json::from_str(&json).expect("deserialize round-trip");
            let json2 = serde_json::to_string(&back).expect("re-serialize");
            assert_eq!(json, json2, "round-trip changed the encoding for {event:?}");
        }
    }

    /// Format-pin guard for the CURRENT (#148) schema: a checked-in journal
    /// sample written under the enriched schema MUST decode into
    /// `OptionChainEvent` and re-encode value-identically. Unlike the v0.5.0
    /// fixture (which predates the additive fields and therefore needs the
    /// defaults patched in), this fixture already carries `tif` / `user_id` /
    /// `trade` and the `ReplaceOrder` / `OrderReplaced` variants, so the
    /// comparison is strict with no patching. It covers `AddOrder` with the
    /// default and with a non-default `Gtd` tif + nonzero user, `OrderAdded` both
    /// with `trade: null` and with a deterministic fill payload, and the replace
    /// pair.
    ///
    /// The fixture lives at `tests/fixtures/journal_event_v0.8.0.json`.
    #[test]
    fn test_journal_event_v0_8_0_fixture_decodes_and_is_stable() {
        const FIXTURE: &str = include_str!("../../tests/fixtures/journal_event_v0.8.0.json");

        let decoded: Vec<OptionChainEvent> =
            serde_json::from_str(FIXTURE).expect("v0.8.0 journal fixture must decode");
        assert_eq!(decoded.len(), 10, "fixture should hold ten events");

        // Spot-check the #148 shapes landed as expected.
        assert!(matches!(
            decoded[0].result,
            OptionChainResult::OrderAdded { trade: None, .. }
        ));
        assert!(matches!(
            decoded[8].command,
            OptionChainCommand::AddOrder {
                tif: TimeInForce::Gtd(1_700_000_000_000),
                ..
            }
        ));
        assert!(matches!(
            decoded[8].result,
            OptionChainResult::OrderAdded { trade: Some(_), .. }
        ));
        assert!(matches!(
            decoded[9].command,
            OptionChainCommand::ReplaceOrder { price: 105, .. }
        ));
        assert!(matches!(
            decoded[9].result,
            OptionChainResult::OrderReplaced { .. }
        ));

        // Strict re-encode: the current schema is fully materialized in the
        // fixture, so no additive-field patching is needed.
        let reencoded: serde_json::Value =
            serde_json::to_value(&decoded).expect("re-encode decoded events");
        let on_disk: serde_json::Value =
            serde_json::from_str(FIXTURE).expect("parse fixture as value");
        assert_eq!(
            reencoded, on_disk,
            "re-encoded journal diverged from the checked-in v0.8.0 wire format"
        );
    }

    /// Negative guard proving `deny_unknown_fields` is active at every journal
    /// layer: an extra field anywhere in the graph (top-level struct, a command
    /// struct-variant, a result struct-variant, or the `Strike` scope variant)
    /// MUST make decoding fail. Without `deny_unknown_fields` these would be
    /// silently ignored and corrupt replay.
    #[test]
    fn test_option_chain_event_rejects_unknown_fields() {
        // Extra field on the top-level OptionChainEvent struct.
        let top_level = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "CancelOrder": { "symbol": "BTC-20240329-50000-C", "order_id": "42" } },
            "result": { "OrderCancelled": { "order_id": "42" } },
            "bogus": true
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(top_level).is_err(),
            "unknown top-level field must be rejected"
        );

        // Extra field inside an AddOrder command struct-variant.
        let in_command = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "AddOrder": {
                "symbol": "BTC-20240329-50000-C",
                "order_id": "42",
                "side": "BUY",
                "price": 100,
                "quantity": 10,
                "extra": 1
            } },
            "result": { "OrderAdded": { "order_id": "42" } }
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(in_command).is_err(),
            "unknown field inside a command struct-variant must be rejected"
        );

        // Extra field inside a Rejected result struct-variant.
        let in_result = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "CancelOrder": { "symbol": "BTC-20240329-50000-C", "order_id": "42" } },
            "result": { "Rejected": { "reason": "x", "extra": "y" } }
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(in_result).is_err(),
            "unknown field inside a result struct-variant must be rejected"
        );

        // Extra field inside the Strike mass-cancel scope struct-variant.
        let in_scope = r#"{ "Strike": {
            "expiration": { "datetime": "2024-03-29T23:59:59Z" },
            "strike": 50000,
            "extra": 0
        } }"#;
        assert!(
            serde_json::from_str::<MassCancelScope>(in_scope).is_err(),
            "unknown field inside the Strike scope variant must be rejected"
        );

        // Extra field inside the 0.7.0 EvictExpiredOrders command struct-variant.
        let in_evict_command = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "EvictExpiredOrders": { "now_ms": 10000000000000, "extra": 1 } },
            "result": { "ExpiredEvicted": { "evicted_ids": [] } }
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(in_evict_command).is_err(),
            "unknown field inside EvictExpiredOrders must be rejected"
        );

        // Extra field inside the 0.7.0 ExpiredEvicted result struct-variant.
        let in_evict_result = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "EvictExpiredOrders": { "now_ms": 10000000000000 } },
            "result": { "ExpiredEvicted": { "evicted_ids": ["42"], "extra": 1 } }
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(in_evict_result).is_err(),
            "unknown field inside ExpiredEvicted must be rejected"
        );

        // Extra field inside the #148 enriched AddOrder command struct-variant
        // (alongside the new tif/user_id fields, to prove they did not weaken
        // deny_unknown_fields).
        let in_enriched_add = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "AddOrder": {
                "symbol": "BTC-20240329-50000-C",
                "order_id": "42",
                "side": "BUY",
                "price": 100,
                "quantity": 10,
                "tif": "IOC",
                "user_id": "0707070707070707070707070707070707070707070707070707070707070707",
                "extra": 1
            } },
            "result": { "OrderAdded": { "order_id": "42", "trade": null } }
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(in_enriched_add).is_err(),
            "unknown field inside the enriched AddOrder must be rejected"
        );

        // Extra field inside the #148 ReplaceOrder command struct-variant.
        let in_replace_command = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "ReplaceOrder": {
                "symbol": "BTC-20240329-50000-C",
                "order_id": "42",
                "price": 105,
                "quantity": 7,
                "side": "BUY",
                "extra": 1
            } },
            "result": { "OrderReplaced": { "order_id": "42" } }
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(in_replace_command).is_err(),
            "unknown field inside ReplaceOrder must be rejected"
        );

        // Extra field inside the #148 OrderReplaced result struct-variant.
        let in_replaced_result = r#"{
            "sequence_num": 1,
            "timestamp_ns": 2,
            "command": { "CancelOrder": { "symbol": "BTC-20240329-50000-C", "order_id": "42" } },
            "result": { "OrderReplaced": { "order_id": "42", "extra": 1 } }
        }"#;
        assert!(
            serde_json::from_str::<OptionChainEvent>(in_replaced_result).is_err(),
            "unknown field inside OrderReplaced must be rejected"
        );
    }

    /// Pins the 0.7.0 wire format of the appended `EvictExpiredOrders` /
    /// `ExpiredEvicted` variants, and proves the forward-compat asymmetry
    /// [`deny_unknown_fields`](https://serde.rs/container-attrs.html#deny_unknown_fields)
    /// creates for journaled payloads:
    ///
    /// - **New binary reads old journal → OK.** A journal written before this
    ///   variant existed carries only known tags, so it still decodes (covered
    ///   directly by [`test_journal_event_v0_5_0_fixture_decodes_and_is_stable`],
    ///   whose fixture predates this variant).
    /// - **Old binary reads new journal → hard error.** A journal carrying the
    ///   new tag fails to decode against a binary that predates it. `serde`
    ///   rejects an unknown *variant tag* independently of
    ///   `deny_unknown_fields` (which governs unknown *fields*), so the failure is
    ///   guaranteed. This test simulates that binary by decoding the frozen
    ///   `EvictExpiredOrders` bytes into an enum whose `EvictExpiredOrders` arm is
    ///   renamed away, and asserting the decode fails.
    #[test]
    fn test_evict_expired_orders_wire_format_pinned() {
        // Exact on-the-wire shape (PascalCase tag, snake_case fields, TimestampMs
        // transparent over u64, ids as strings). A rename/recasing breaks this.
        let event = OptionChainEvent {
            sequence_num: 9,
            timestamp_ns: 42,
            command: OptionChainCommand::EvictExpiredOrders {
                now_ms: TimestampMs::new(10_000_000_000_000),
            },
            result: OptionChainResult::ExpiredEvicted {
                evicted_ids: vec![OrderId::sequential(7), OrderId::sequential(8)],
            },
        };
        let value = serde_json::to_value(&event).expect("serialize");
        let expected = serde_json::json!({
            "sequence_num": 9,
            "timestamp_ns": 42,
            "command": { "EvictExpiredOrders": { "now_ms": 10_000_000_000_000_u64 } },
            "result": { "ExpiredEvicted": { "evicted_ids": ["7", "8"] } }
        });
        assert_eq!(value, expected, "EvictExpiredOrders wire format drifted");

        // Old-binary-reads-new-journal: an enum missing the EvictExpiredOrders arm
        // must reject the journaled bytes (unknown variant tag).
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        #[allow(dead_code)]
        enum OldCommand {
            AddOrder,
            CancelOrder,
        }
        let cmd_bytes = r#"{ "EvictExpiredOrders": { "now_ms": 10000000000000 } }"#;
        assert!(
            serde_json::from_str::<OldCommand>(cmd_bytes).is_err(),
            "a binary predating EvictExpiredOrders must reject the new variant tag"
        );
    }

    /// Pins the #148 enriched `AddOrder` wire shape: the appended `tif` /
    /// `user_id` fields, the pricelevel casing of the `Gtd` payload
    /// (`{"GTD": ms}`, not `snake_case`), and the hex-string `user_id`. Also pins
    /// that the default `tif`/`user` still serialize explicitly (no
    /// `skip_serializing_if`), which is what keeps the JSON/bincode encodings
    /// symmetric across a decode/encode round-trip.
    #[test]
    fn test_add_order_wire_format_pinned() {
        // Non-default tif (Gtd) + nonzero user.
        let gtd = OptionChainCommand::AddOrder {
            symbol: "BTC-20240329-50000-C".to_string(),
            order_id: OrderId::sequential(42),
            side: Side::Buy,
            price: 100,
            quantity: 4,
            tif: TimeInForce::Gtd(1_700_000_000_000),
            user_id: Hash32::from([7u8; 32]),
        };
        let value = serde_json::to_value(&gtd).expect("serialize");
        let expected = serde_json::json!({
            "AddOrder": {
                "symbol": "BTC-20240329-50000-C",
                "order_id": "42",
                "side": "BUY",
                "price": 100,
                "quantity": 4,
                "tif": { "GTD": 1_700_000_000_000_u64 },
                "user_id": "0707070707070707070707070707070707070707070707070707070707070707"
            }
        });
        assert_eq!(value, expected, "enriched AddOrder wire format drifted");

        // Default tif/user still serialize explicitly.
        let default = OptionChainCommand::AddOrder {
            symbol: "BTC-20240329-50000-C".to_string(),
            order_id: OrderId::sequential(42),
            side: Side::Sell,
            price: 110,
            quantity: 5,
            tif: TimeInForce::Gtc,
            user_id: Hash32::zero(),
        };
        let default_value = serde_json::to_value(&default).expect("serialize default");
        assert_eq!(
            default_value["AddOrder"]["tif"],
            serde_json::json!("GTC"),
            "default tif must serialize as \"GTC\""
        );
        assert_eq!(
            default_value["AddOrder"]["user_id"],
            serde_json::json!(Hash32::zero().to_hex()),
            "default user_id must serialize as the zero hex string"
        );
    }

    /// Pins the wire shape of an `OrderAdded` that carries a fill payload: the
    /// nested `TradeResult` / `MatchResult` / `Trade` structure, with `Id`s as
    /// strings and numeric price/quantity/timestamp.
    #[test]
    fn test_order_added_with_trade_wire_format_pinned() {
        let result = OptionChainResult::OrderAdded {
            order_id: OrderId::sequential(42),
            trade: Some(deterministic_trade_result()),
        };
        let value = serde_json::to_value(&result).expect("serialize");
        let expected = serde_json::json!({
            "OrderAdded": {
                "order_id": "42",
                "trade": {
                    "symbol": "BTC-20240329-50000-C",
                    "match_result": {
                        "order_id": "42",
                        "trades": {
                            "trades": [
                                {
                                    "trade_id": "9001",
                                    "taker_order_id": "42",
                                    "maker_order_id": "43",
                                    "price": 100,
                                    "quantity": 4,
                                    "taker_side": "BUY",
                                    "timestamp": 1_700_000_000_000_u64
                                }
                            ]
                        },
                        "remaining_quantity": 0,
                        "is_complete": true,
                        "filled_order_ids": [],
                        "outcome": "filled"
                    },
                    "total_maker_fees": 0,
                    "total_taker_fees": 0,
                    "engine_seq": 0,
                    "quote_notional": 400
                }
            }
        });
        assert_eq!(value, expected, "OrderAdded-with-trade wire format drifted");
    }

    /// Pins the #148 `ReplaceOrder` / `OrderReplaced` wire format and proves the
    /// old-binary-reads-new-journal asymmetry: a binary that predates the
    /// variant rejects the new tag (unknown variant), independent of
    /// `deny_unknown_fields`.
    #[test]
    fn test_replace_order_wire_format_pinned() {
        let event = OptionChainEvent {
            sequence_num: 10,
            timestamp_ns: 42,
            command: OptionChainCommand::ReplaceOrder {
                symbol: "BTC-20240329-50000-C".to_string(),
                order_id: OrderId::sequential(42),
                price: 105,
                quantity: 7,
                side: Side::Buy,
            },
            result: OptionChainResult::OrderReplaced {
                order_id: OrderId::sequential(42),
            },
        };
        let value = serde_json::to_value(&event).expect("serialize");
        let expected = serde_json::json!({
            "sequence_num": 10,
            "timestamp_ns": 42,
            "command": { "ReplaceOrder": {
                "symbol": "BTC-20240329-50000-C",
                "order_id": "42",
                "price": 105,
                "quantity": 7,
                "side": "BUY"
            } },
            "result": { "OrderReplaced": { "order_id": "42" } }
        });
        assert_eq!(value, expected, "ReplaceOrder wire format drifted");

        // Old-binary-reads-new-journal: an enum missing the ReplaceOrder arm must
        // reject the journaled bytes (unknown variant tag).
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        #[allow(dead_code)]
        enum OldCommand {
            AddOrder,
            CancelOrder,
        }
        let cmd_bytes = r#"{ "ReplaceOrder": {
            "symbol": "BTC-20240329-50000-C",
            "order_id": "42",
            "price": 105,
            "quantity": 7,
            "side": "BUY"
        } }"#;
        assert!(
            serde_json::from_str::<OldCommand>(cmd_bytes).is_err(),
            "a binary predating ReplaceOrder must reject the new variant tag"
        );
    }

    // ── Helper ────────────────────────────────────────────────────────────

    /// Builds a `SequencedUnderlyingOrderBook` with a real expiration, strike,
    /// and resting orders so that command execution paths are exercisable.
    fn make_book_with_orders() -> (SequencedUnderlyingOrderBook, ExpirationDate, String) {
        // Build the chain via the canonical parser so its `ExpirationKey`
        // matches what `find_book_by_symbol` derives when routing the symbol.
        let expiry = SymbolParser::parse_yyyymmdd("20240329", "BTC-20240329-50000-C")
            .expect("canonical expiry");

        let underlying = UnderlyingOrderBook::new("BTC");
        let exp_book = underlying.get_or_create_expiration(expiry);
        let strike = exp_book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Buy, 100, 10)
            .expect("seed call buy");
        strike
            .call()
            .add_limit_order(OrderId::new(), Side::Sell, 110, 5)
            .expect("seed call sell");
        strike
            .put()
            .add_limit_order(OrderId::new(), Side::Buy, 50, 10)
            .expect("seed put buy");
        drop(strike);
        drop(exp_book);

        let book = SequencedUnderlyingOrderBook::from_underlying(underlying);
        let symbol = "BTC-20240329-50000-C".to_string();
        (book, expiry, symbol)
    }

    fn make_book_with_user_orders() -> (SequencedUnderlyingOrderBook, ExpirationDate) {
        let expiry = SymbolParser::parse_yyyymmdd("20240329", "BTC-20240329-50000-C")
            .expect("canonical expiry");

        let user_a = Hash32::from([1u8; 32]);

        let underlying = UnderlyingOrderBook::new("BTC");
        let exp_book = underlying.get_or_create_expiration(expiry);
        let strike = exp_book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
            .expect("seed user order");
        drop(strike);
        drop(exp_book);

        let book = SequencedUnderlyingOrderBook::from_underlying(underlying);
        (book, expiry)
    }

    // ── SequencedUnderlyingOrderBook constructors ─────────────────────────

    #[test]
    fn test_sequenced_book_from_underlying() {
        let underlying = UnderlyingOrderBook::new("ETH");
        let book = SequencedUnderlyingOrderBook::from_underlying(underlying);

        assert_eq!(book.underlying_symbol(), "ETH");
        assert_eq!(book.current_sequence(), 0);
        assert!(!book.has_journal());
    }

    #[test]
    fn test_sequenced_book_from_underlying_with_journal() {
        let underlying = UnderlyingOrderBook::new("ETH");
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let book = SequencedUnderlyingOrderBook::from_underlying_with_journal(underlying, journal);

        assert_eq!(book.underlying_symbol(), "ETH");
        assert!(book.has_journal());
    }

    // ── Accessor & delegation tests ──────────────────────────────────────

    #[test]
    fn test_sequenced_book_underlying_accessor() {
        let (book, _, _) = make_book_with_orders();
        let inner = book.underlying();
        assert_eq!(inner.underlying(), "BTC");
    }

    #[test]
    fn test_sequenced_book_success_reject_counts() {
        let (book, _, symbol) = make_book_with_orders();

        // Successful add
        let receipt = book
            .submit_add_order(&symbol, OrderId::new(), Side::Buy, 90, 5)
            .expect("submit");
        assert!(receipt.result.is_success());
        assert_eq!(book.success_count(), 1);
        assert_eq!(book.reject_count(), 0);

        // Rejected: book not found
        let receipt2 = book
            .submit_add_order("INVALID", OrderId::new(), Side::Buy, 90, 5)
            .expect("submit");
        assert!(receipt2.result.is_error());
        assert_eq!(book.success_count(), 1);
        assert_eq!(book.reject_count(), 1);
    }

    #[test]
    fn test_sequenced_book_expiration_count() {
        let (book, _, _) = make_book_with_orders();
        assert_eq!(book.expiration_count(), 1);
    }

    #[test]
    fn test_sequenced_book_total_order_count() {
        let (book, _, _) = make_book_with_orders();
        // 3 seeded orders: call buy, call sell, put buy
        assert_eq!(book.total_order_count(), 3);
    }

    #[test]
    fn test_sequenced_book_terminal_order_summary() {
        let (book, _, _) = make_book_with_orders();
        let summary = book.terminal_order_summary();
        // No fills occurred during seeding (no matching), so filled == 0
        assert_eq!(summary.total(), 0);
    }

    #[test]
    fn test_sequenced_book_debug() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        let debug = format!("{:?}", book);
        assert!(debug.contains("SequencedUnderlyingOrderBook"));
        assert!(debug.contains("BTC"));
    }

    // ── Submit: add order ────────────────────────────────────────────────

    #[test]
    fn test_submit_add_order_success() {
        let (book, _, symbol) = make_book_with_orders();

        let receipt = book
            .submit_add_order(&symbol, OrderId::new(), Side::Buy, 95, 5)
            .expect("submit");
        assert!(receipt.result.is_success());
        assert_eq!(receipt.sequence_num, 0);
    }

    #[test]
    fn test_submit_add_order_vivifies_missing_strike_returns_success() {
        // Option (b): a valid AddOrder against a contract that has not yet been
        // listed materializes its expiration+strike on the deterministic
        // get_or_create_* path rather than rejecting with BookNotFound. This is
        // the live semantic that makes replay rebuild AddOrder state.
        let book = SequencedUnderlyingOrderBook::new("BTC");
        assert_eq!(book.expiration_count(), 0);

        let receipt = book
            .submit_add_order("BTC-20240329-50000-C", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");

        assert!(receipt.result.is_success(), "got {:?}", receipt.result);
        // The strike materialized and the resting order is present.
        assert_eq!(book.expiration_count(), 1);
        assert_eq!(book.total_order_count(), 1);
    }

    #[test]
    fn test_submit_add_order_malformed_symbol_returns_book_not_found() {
        // A symbol that cannot be parsed names no book to create, so AddOrder
        // still yields BookNotFound (vivification is impossible without a key).
        let book = SequencedUnderlyingOrderBook::new("BTC");

        let receipt = book
            .submit_add_order("NOT-A-VALID-SYMBOL", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");
        assert!(receipt.result.is_error());
        match &receipt.result {
            OptionChainResult::BookNotFound { symbol } => {
                assert!(symbol.contains("NOT-A-VALID-SYMBOL"));
            }
            other => panic!("expected BookNotFound, got {:?}", other),
        }
        // Nothing was materialized for an unparseable symbol.
        assert_eq!(book.expiration_count(), 0);
    }

    // ── Submit: add order with tif / user, and fill attribution (#148) ────

    #[test]
    fn test_submit_add_order_with_tif_and_user_executes_and_journals() {
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let book = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));
        let user = Hash32::from([9u8; 32]);
        // A far-future GTD deadline (Unix ms): admission accepts it, so the order
        // rests rather than being evicted on entry.
        let tif = TimeInForce::Gtd(10_000_000_000_000);

        let receipt = book
            .submit_add_order_with(
                "BTC-20240329-50000-C",
                OrderId::sequential(1),
                Side::Buy,
                100,
                10,
                tif,
                user,
            )
            .expect("submit");
        assert!(receipt.result.is_success(), "got {:?}", receipt.result);

        // The journaled command carries the tif and user identity verbatim.
        let events = journal.read_from(0).expect("read journal");
        assert_eq!(events.len(), 1);
        match &events[0].command {
            OptionChainCommand::AddOrder {
                tif: journaled_tif,
                user_id,
                ..
            } => {
                assert_eq!(*journaled_tif, tif);
                assert_eq!(*user_id, user);
            }
            other => panic!("expected AddOrder, got {other:?}"),
        }
    }

    #[test]
    fn test_submit_add_order_defaults_to_gtc_and_zero_user() {
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let book = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&journal));

        book.submit_add_order(
            "BTC-20240329-50000-C",
            OrderId::sequential(1),
            Side::Buy,
            100,
            10,
        )
        .expect("submit");

        let events = journal.read_from(0).expect("read journal");
        match &events[0].command {
            OptionChainCommand::AddOrder { tif, user_id, .. } => {
                assert_eq!(*tif, TimeInForce::Gtc);
                assert_eq!(*user_id, Hash32::zero());
            }
            other => panic!("expected AddOrder, got {other:?}"),
        }

        // And they materialize on the wire as "GTC" + the zero hex string.
        let value = serde_json::to_value(&events[0]).expect("serialize event");
        assert_eq!(
            value["command"]["AddOrder"]["tif"],
            serde_json::json!("GTC")
        );
        assert_eq!(
            value["command"]["AddOrder"]["user_id"],
            serde_json::json!(Hash32::zero().to_hex())
        );
    }

    #[test]
    fn test_add_order_old_wire_decodes_with_gtc_and_zero_user_defaults() {
        // A pre-#148 AddOrder command has no tif / user_id fields. It must decode
        // to the good-till-cancelled, zero-user defaults so old journals replay
        // identically.
        let old_wire = r#"{ "AddOrder": {
            "symbol": "BTC-20240329-50000-C",
            "order_id": "1",
            "side": "BUY",
            "price": 100,
            "quantity": 10
        } }"#;
        let command: OptionChainCommand =
            serde_json::from_str(old_wire).expect("old AddOrder wire must decode");
        match command {
            OptionChainCommand::AddOrder { tif, user_id, .. } => {
                assert_eq!(tif, TimeInForce::Gtc);
                assert_eq!(user_id, Hash32::zero());
            }
            other => panic!("expected AddOrder, got {other:?}"),
        }
    }

    #[test]
    fn test_order_added_carries_trade_when_add_fills() {
        // The fixture seeds a resting call sell@110 qty5; a marketable buy@110
        // crosses it and executes a fill, so OrderAdded carries the trade.
        let (book, _, symbol) = make_book_with_orders();

        let receipt = book
            .submit_add_order(&symbol, OrderId::new(), Side::Buy, 110, 5)
            .expect("submit");
        match &receipt.result {
            OptionChainResult::OrderAdded { trade, .. } => {
                let trade = trade.as_ref().expect("a crossing add must carry fills");
                assert!(
                    !trade.match_result.trades().is_empty(),
                    "trade payload must record at least one fill"
                );
            }
            other => panic!("expected OrderAdded, got {other:?}"),
        }
    }

    #[test]
    fn test_order_added_trade_none_when_add_rests() {
        // A buy@95 sits below the resting sell@110, so it rests without crossing
        // and OrderAdded carries no trade.
        let (book, _, symbol) = make_book_with_orders();

        let receipt = book
            .submit_add_order(&symbol, OrderId::new(), Side::Buy, 95, 5)
            .expect("submit");
        assert!(matches!(
            receipt.result,
            OptionChainResult::OrderAdded { trade: None, .. }
        ));
    }

    // ── Submit: replace order (#148) ─────────────────────────────────────

    #[test]
    fn test_submit_replace_order_success_returns_order_replaced() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        let symbol = "BTC-20240329-50000-C";
        let oid = OrderId::sequential(1);
        book.submit_add_order(symbol, oid, Side::Buy, 100, 10)
            .expect("seed add");

        let receipt = book
            .submit_replace_order(symbol, oid, 105, 7, Side::Buy)
            .expect("submit replace");
        match &receipt.result {
            OptionChainResult::OrderReplaced { order_id } => assert_eq!(*order_id, oid),
            other => panic!("expected OrderReplaced, got {other:?}"),
        }
    }

    #[test]
    fn test_submit_replace_order_book_not_found() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        // A valid, well-formed symbol whose book was never created: replace uses
        // the non-creating resolver, so it reports BookNotFound.
        let receipt = book
            .submit_replace_order(
                "BTC-20240329-50000-C",
                OrderId::sequential(1),
                105,
                7,
                Side::Buy,
            )
            .expect("submit");
        assert!(matches!(
            receipt.result,
            OptionChainResult::BookNotFound { .. }
        ));
    }

    #[test]
    fn test_submit_replace_order_unknown_order_rejected_deterministically() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        let symbol = "BTC-20240329-50000-C";
        // The book exists (one resting order), but the replaced id is not resting.
        book.submit_add_order(symbol, OrderId::sequential(1), Side::Buy, 100, 10)
            .expect("seed");

        let unknown = OrderId::sequential(999);
        let receipt = book
            .submit_replace_order(symbol, unknown, 105, 7, Side::Buy)
            .expect("submit");
        match &receipt.result {
            OptionChainResult::Rejected { reason } => {
                assert!(
                    reason.contains("order not found"),
                    "reason should name the missing order, got {reason:?}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn test_submit_replace_order_does_not_vivify_expiration_or_strike() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        assert_eq!(book.expiration_count(), 0);

        let receipt = book
            .submit_replace_order(
                "BTC-20240329-50000-C",
                OrderId::sequential(1),
                105,
                7,
                Side::Buy,
            )
            .expect("submit");
        assert!(matches!(
            receipt.result,
            OptionChainResult::BookNotFound { .. }
        ));
        // The non-creating resolution left the hierarchy untouched — no expiration
        // or strike was materialized by the replace.
        assert_eq!(book.expiration_count(), 0);
        assert_eq!(book.total_order_count(), 0);
    }

    #[test]
    fn test_submit_replace_order_underlying_mismatch_rejected() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        // An ETH symbol against a BTC book is a cross-underlying command, rejected
        // with the typed reason before any book resolution.
        let receipt = book
            .submit_replace_order(
                "ETH-20240329-50000-C",
                OrderId::sequential(1),
                105,
                7,
                Side::Buy,
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::Rejected { reason } => {
                assert!(
                    reason.contains("ETH") || reason.contains("BTC"),
                    "reason should name the mismatched underlyings, got {reason:?}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    // ── Submit: cancel order ─────────────────────────────────────────────

    #[test]
    fn test_submit_cancel_order_book_not_found() {
        let book = SequencedUnderlyingOrderBook::new("BTC");

        let receipt = book
            .submit_cancel_order("BTC-20240329-50000-C", OrderId::new())
            .expect("submit");
        assert!(receipt.result.is_error());
    }

    #[test]
    fn test_submit_cancel_order_nonexistent() {
        let (book, _, symbol) = make_book_with_orders();

        // Cancel a random order_id that doesn't exist in the book.
        // The underlying orderbook may treat this as a no-op success
        // or a rejection depending on the implementation.
        let receipt = book
            .submit_cancel_order(&symbol, OrderId::new())
            .expect("submit");
        // Verify a receipt is returned with a valid sequence number
        assert_eq!(receipt.sequence_num, 0);
    }

    // ── Mass cancel: Underlying scope ────────────────────────────────────

    #[test]
    fn test_mass_cancel_underlying_all() {
        let (book, _, _) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(MassCancelScope::Underlying, MassCancelType::All)
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 3);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_underlying_by_side() {
        let (book, _, _) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Underlying,
                MassCancelType::BySide(Side::Buy),
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                // 2 buy orders: call buy + put buy
                assert!(*cancelled_count >= 2);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_underlying_by_user() {
        let (book, _) = make_book_with_user_orders();
        let user_a = Hash32::from([1u8; 32]);

        let receipt = book
            .submit_mass_cancel(MassCancelScope::Underlying, MassCancelType::ByUser(user_a))
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 1);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    // ── Mass cancel: Expiration scope ────────────────────────────────────

    #[test]
    fn test_mass_cancel_expiration_all() {
        let (book, expiry, _) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(MassCancelScope::Expiration(expiry), MassCancelType::All)
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 3);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_expiration_by_side() {
        let (book, expiry, _) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Expiration(expiry),
                MassCancelType::BySide(Side::Sell),
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                // 1 sell order: call sell
                assert!(*cancelled_count >= 1);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_expiration_by_user() {
        let (book, expiry) = make_book_with_user_orders();
        let user_a = Hash32::from([1u8; 32]);

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Expiration(expiry),
                MassCancelType::ByUser(user_a),
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 1);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_expiration_not_found() {
        let book = SequencedUnderlyingOrderBook::new("BTC");

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Expiration(ExpirationDate::Days(
                    optionstratlib::prelude::pos_or_panic!(30.0),
                )),
                MassCancelType::All,
            )
            .expect("submit");
        assert!(receipt.result.is_error());
    }

    // ── Mass cancel: Strike scope ────────────────────────────────────────

    #[test]
    fn test_mass_cancel_strike_all() {
        let (book, expiry, _) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Strike {
                    expiration: expiry,
                    strike: 50000,
                },
                MassCancelType::All,
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 3);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_strike_by_side() {
        let (book, expiry, _) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Strike {
                    expiration: expiry,
                    strike: 50000,
                },
                MassCancelType::BySide(Side::Buy),
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 2);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_strike_by_user() {
        let (book, expiry) = make_book_with_user_orders();
        let user_a = Hash32::from([1u8; 32]);

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Strike {
                    expiration: expiry,
                    strike: 50000,
                },
                MassCancelType::ByUser(user_a),
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 1);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_strike_not_found() {
        let (book, expiry, _) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Strike {
                    expiration: expiry,
                    strike: 99999,
                },
                MassCancelType::All,
            )
            .expect("submit");
        assert!(receipt.result.is_error());
    }

    // ── Mass cancel: Book scope ──────────────────────────────────────────

    #[test]
    fn test_mass_cancel_book_all() {
        let (book, _, symbol) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(MassCancelScope::Book(symbol), MassCancelType::All)
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                // Call book has 2 orders (buy + sell)
                assert!(*cancelled_count >= 2);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_book_by_side() {
        let (book, _, symbol) = make_book_with_orders();

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Book(symbol),
                MassCancelType::BySide(Side::Buy),
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 1);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_book_by_user() {
        let user_a = Hash32::from([1u8; 32]);
        let expiry = SymbolParser::parse_yyyymmdd("20240329", "BTC-20240329-50000-C")
            .expect("canonical expiry");

        let underlying = UnderlyingOrderBook::new("BTC");
        let exp_book = underlying.get_or_create_expiration(expiry);
        let strike = exp_book.get_or_create_strike(50000);
        strike
            .call()
            .add_limit_order_with_user(OrderId::new(), Side::Buy, 100, 10, user_a)
            .expect("seed");
        drop(strike);
        drop(exp_book);

        let book = SequencedUnderlyingOrderBook::from_underlying(underlying);
        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Book("BTC-20240329-50000-C".to_string()),
                MassCancelType::ByUser(user_a),
            )
            .expect("submit");
        match &receipt.result {
            OptionChainResult::MassCancelled { cancelled_count } => {
                assert!(*cancelled_count >= 1);
            }
            other => panic!("expected MassCancelled, got {:?}", other),
        }
    }

    #[test]
    fn test_mass_cancel_book_not_found() {
        let book = SequencedUnderlyingOrderBook::new("BTC");

        let receipt = book
            .submit_mass_cancel(
                MassCancelScope::Book("BTC-20240329-50000-C".to_string()),
                MassCancelType::All,
            )
            .expect("submit");
        assert!(receipt.result.is_error());
    }

    // ── find_book_by_symbol error paths ──────────────────────────────────

    #[test]
    fn test_find_book_invalid_symbol_format() {
        let book = SequencedUnderlyingOrderBook::new("BTC");

        let receipt = book
            .submit_add_order("INVALID-FORMAT", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");
        assert!(receipt.result.is_error());
    }

    #[test]
    fn test_find_book_invalid_option_type() {
        let (book, _, _) = make_book_with_orders();

        let receipt = book
            .submit_add_order("BTC-20240329-50000-X", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");
        assert!(receipt.result.is_error());
    }

    // ── symbol expiration parsing (delegated to SymbolParser) ─────────────

    #[test]
    fn test_parse_expiration_invalid() {
        let book = SequencedUnderlyingOrderBook::new("BTC");

        let receipt = book
            .submit_add_order("BTC-NOTADATE-50000-C", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");
        assert!(receipt.result.is_error());
    }

    // ── underlying-mismatch routing (cross-book safety) ───────────────────

    #[test]
    fn test_find_book_by_symbol_cross_underlying_rejected_with_typed_error() {
        // BTC book already holds 20240329 / 50000 — the same expiry+strike the
        // ETH symbol names. Before consolidation the underlying was never
        // checked, so this would have silently routed into the BTC book.
        let (book, _, _) = make_book_with_orders();

        // `Arc<OptionOrderBook>` is not `Debug`, so match instead of `expect_err`.
        match book.find_book_by_symbol("ETH-20240329-50000-C") {
            Err(Error::UnderlyingMismatch { .. }) => {}
            Err(other) => panic!("expected UnderlyingMismatch, got {other:?}"),
            Ok(_) => panic!("expected UnderlyingMismatch, got Ok"),
        }
    }

    #[test]
    fn test_submit_add_order_cross_underlying_rejected() {
        let (book, _, _) = make_book_with_orders();

        let receipt = book
            .submit_add_order("ETH-20240329-50000-C", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");
        match &receipt.result {
            OptionChainResult::Rejected { reason } => {
                assert!(reason.contains("underlying mismatch"), "reason: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    // ── chain created via SymbolParser resolves a sequencer-routed order ──

    #[test]
    fn test_sequencer_resolves_chain_created_via_symbol_parser_same_yyyymmdd() {
        let symbol = "BTC-20240329-50000-C";

        // Create the chain straight from the canonical parser output.
        let parsed = SymbolParser::parse(symbol).expect("parse");
        let underlying = UnderlyingOrderBook::new("BTC");
        let exp_book = underlying.get_or_create_expiration(*parsed.expiration());
        let strike = exp_book.get_or_create_strike(parsed.strike());
        let created_call = strike.call_arc();
        drop(strike);
        drop(exp_book);

        let book = SequencedUnderlyingOrderBook::from_underlying(underlying);

        // The sequencer must route the same YYYYMMDD symbol to that exact book.
        let receipt = book
            .submit_add_order(symbol, OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");
        assert!(
            receipt.result.is_success(),
            "routed order must resolve: {:?}",
            receipt.result
        );

        // Identical resolution: same leaf `Arc`, which can only happen if the
        // derived `ExpirationKey` matched the one used to create the chain.
        let resolved = book
            .find_book_by_symbol(symbol)
            .expect("symbol must resolve");
        assert!(Arc::ptr_eq(&resolved, &created_call));
    }

    // ── put book path ────────────────────────────────────────────────────

    #[test]
    fn test_submit_add_order_to_put_book() {
        let (book, _, _) = make_book_with_orders();

        let receipt = book
            .submit_add_order("BTC-20240329-50000-P", OrderId::new(), Side::Buy, 40, 5)
            .expect("submit");
        assert!(receipt.result.is_success());
    }

    // ── Registry / SymbolIndex integration ──────────────────────────────

    #[test]
    fn test_sequenced_book_new_without_registry() {
        let book = SequencedUnderlyingOrderBook::new("BTC");
        assert!(book.registry().is_none());
        assert!(book.symbol_index().is_none());
    }

    #[test]
    fn test_sequenced_book_new_with_registry_and_index() {
        let registry = Arc::new(InstrumentRegistry::new());
        let symbol_index = Arc::new(SymbolIndex::new());

        let book = SequencedUnderlyingOrderBook::new_with_registry_and_index(
            "BTC",
            Arc::clone(&registry),
            Arc::clone(&symbol_index),
        );

        assert!(book.registry().is_some());
        assert!(book.symbol_index().is_some());
        assert_eq!(book.underlying_symbol(), "BTC");
        assert_eq!(book.current_sequence(), 0);
        assert!(!book.has_journal());
    }

    #[test]
    fn test_sequenced_book_with_journal_registry_and_index() {
        let registry = Arc::new(InstrumentRegistry::new());
        let symbol_index = Arc::new(SymbolIndex::new());
        let journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());

        let book = SequencedUnderlyingOrderBook::with_journal_registry_and_index(
            "BTC",
            journal,
            Arc::clone(&registry),
            Arc::clone(&symbol_index),
        );

        assert!(book.registry().is_some());
        assert!(book.symbol_index().is_some());
        assert!(book.has_journal());
    }

    #[test]
    fn test_sequenced_book_registry_populated_after_add_order() {
        let registry = Arc::new(InstrumentRegistry::new());
        let symbol_index = Arc::new(SymbolIndex::new());

        let underlying = UnderlyingOrderBook::new_with_registry_and_index(
            "BTC",
            Arc::clone(&registry),
            Arc::clone(&symbol_index),
        );

        assert!(registry.is_empty());
        assert!(symbol_index.is_empty());

        // Create the hierarchy via the canonical parser so the chain key matches
        // what the routed symbol resolves to.
        let expiry = SymbolParser::parse_yyyymmdd("20240329", "BTC-20240329-50000-C")
            .expect("canonical expiry");

        let exp_book = underlying.get_or_create_expiration(expiry);
        let strike = exp_book.get_or_create_strike(50000);
        drop(strike);
        drop(exp_book);

        // Registry and symbol index should be populated after hierarchy creation
        assert!(!registry.is_empty());
        assert!(!symbol_index.is_empty());
        assert!(symbol_index.contains("BTC-20240329-50000-C"));

        // Wrap in sequencer and submit an order
        let book = SequencedUnderlyingOrderBook::from_underlying(underlying);
        let receipt = book
            .submit_add_order("BTC-20240329-50000-C", OrderId::new(), Side::Buy, 100, 10)
            .expect("submit");
        assert!(receipt.result.is_success());

        // Verify iter() returns the registered entries
        let entries = registry.iter();
        assert!(!entries.is_empty());

        // Verify entries() on symbol index
        let sym_entries = symbol_index.entries();
        assert!(!sym_entries.is_empty());
    }

    #[test]
    fn test_sequenced_book_from_underlying_with_registry() {
        let registry = Arc::new(InstrumentRegistry::new());
        let symbol_index = Arc::new(SymbolIndex::new());

        let underlying = UnderlyingOrderBook::new_with_registry_and_index(
            "ETH",
            Arc::clone(&registry),
            Arc::clone(&symbol_index),
        );

        let book = SequencedUnderlyingOrderBook::from_underlying(underlying);

        assert!(book.registry().is_some());
        assert!(book.symbol_index().is_some());
    }

    #[test]
    fn test_replay_serialized_against_concurrent_submits_no_deadlock() {
        use std::sync::Barrier;
        use std::thread;

        // A journal pre-populated by a first live run: replaying it must be
        // able to run concurrently with fresh submits on a second book without
        // deadlocking, because submit and replay share the same gate but never
        // nest.
        let seed_journal: Arc<dyn OptionChainJournal> = Arc::new(InMemoryOptionChainJournal::new());
        let seed = SequencedUnderlyingOrderBook::with_journal("BTC", Arc::clone(&seed_journal));
        for i in 0..20u64 {
            seed.submit_add_order(
                "BTC-20240329-50000-C",
                OrderId::from_u64(1 + i),
                Side::Buy,
                100 + u128::from(i),
                10,
            )
            .expect("seed submit");
        }
        let seed_events = seed_journal.read_from(0).expect("read seed");
        assert_eq!(seed_events.len(), 20);

        // The book under test carries the seeded events in a fresh journal so
        // the replaying thread has something to rebuild while the other thread
        // submits new commands.
        let replay_journal: Arc<dyn OptionChainJournal> =
            Arc::new(InMemoryOptionChainJournal::new());
        for event in &seed_events {
            replay_journal.append(event).expect("append seed");
        }
        let book = Arc::new(SequencedUnderlyingOrderBook::with_journal(
            "BTC",
            Arc::clone(&replay_journal),
        ));

        // Lockstep start so the replay and the submits genuinely contend for
        // the gate rather than running one-after-the-other.
        let barrier = Arc::new(Barrier::new(2));

        let replay_book = Arc::clone(&book);
        let replay_barrier = Arc::clone(&barrier);
        let replayer = thread::spawn(move || {
            replay_barrier.wait();
            replay_book.replay(0).expect("replay")
        });

        let submit_book = Arc::clone(&book);
        let submit_barrier = Arc::clone(&barrier);
        let submitter = thread::spawn(move || {
            submit_barrier.wait();
            for i in 0..20u64 {
                submit_book
                    .submit_add_order(
                        "BTC-20240329-55000-C",
                        OrderId::from_u64(1_000 + i),
                        Side::Sell,
                        200 + u128::from(i),
                        5,
                    )
                    .expect("live submit");
            }
        });

        // Both threads must complete; a deadlock would hang the join forever.
        let replayed = replayer.join().expect("replayer thread");
        submitter.join().expect("submitter thread");

        // Replay and submit share one journal, and replay reads it atomically
        // under the gate: it sees the 20 seeded events plus however many of the
        // 20 live submits had already been appended when it acquired the gate.
        // So the replayed count lands in [20, 40] depending on interleaving.
        assert!(
            (20..=40).contains(&replayed),
            "replay must return between the seeded count and seeded+submits, got {replayed}"
        );
        // The 20 live submits each `fetch_add(1)` unconditionally, so the
        // sequence has advanced by at least 20 regardless of how replay's
        // `fetch_max` interleaves with them (which can only raise it).
        assert!(
            book.current_sequence() >= 20,
            "sequence must advance past the live submits, got {}",
            book.current_sequence()
        );
    }
}
