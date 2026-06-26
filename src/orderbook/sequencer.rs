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
use crate::orderbook::symbol_index::SymbolIndex;
use crate::orderbook::underlying::UnderlyingOrderBook;
use crate::utils::{SymbolParser, nanos_since_epoch};
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::{OrderId, Side};
use pricelevel::Hash32;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Scope for mass cancel operations in the option chain hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MassCancelType {
    /// Cancel all orders.
    All,
    /// Cancel orders on a specific side.
    BySide(Side),
    /// Cancel orders belonging to a specific user.
    ByUser(Hash32),
}

/// Command for the option chain sequencer with hierarchy routing.
///
/// Each variant represents an operation that can be sequenced through
/// the option chain sequencer. Commands are serializable for journal
/// persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// Result of executing an option chain command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OptionChainResult {
    /// An order was successfully added.
    OrderAdded {
        /// The identifier of the newly added order.
        order_id: OrderId,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl OptionChainSequencer {
    /// Creates a new sequencer starting from sequence 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            reject_count: AtomicU64::new(0),
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
    /// # Errors
    ///
    /// Returns an error if journaling fails.
    pub fn submit(&self, command: OptionChainCommand) -> Result<OptionChainReceipt, Error> {
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
        let command = OptionChainCommand::AddOrder {
            symbol: symbol.to_string(),
            order_id,
            side,
            price,
            quantity,
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
            } => self.execute_add_order(symbol, *order_id, *side, *price, *quantity),
            OptionChainCommand::CancelOrder { symbol, order_id } => {
                self.execute_cancel_order(symbol, *order_id)
            }
            OptionChainCommand::MassCancel { scope, cancel_type } => {
                self.execute_mass_cancel(scope, cancel_type)
            }
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
    fn execute_add_order(
        &self,
        symbol: &str,
        order_id: OrderId,
        side: Side,
        price: u128,
        quantity: u64,
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

        match book.add_limit_order(order_id, side, price, quantity) {
            Ok(_) => OptionChainResult::OrderAdded { order_id },
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
    /// journaled and need not match across runs; the equality oracle is the
    /// structural / order state (resting orders + top-of-book), not numeric ids.
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
    /// NOT journaled, so they are free to differ between the live run and a
    /// replay; they are not part of the equality contract. For a fresh book to
    /// rebuild identical structural state it must be configured identically
    /// (validation / contract specs / STP mode / fee schedule), since those
    /// shared settings are applied to books created during replay.
    ///
    /// Re-execution results are intentionally discarded: this method is a
    /// state-rebuild tool, not a validation tool. The authoritative result of
    /// each command is the one already recorded in the journal at live time.
    ///
    /// **Out-of-band state is not reconstructed.** Instrument-status transitions
    /// (`halt` / `set_status`) and expiry-lifecycle actions are applied directly
    /// to the hierarchy, not as journaled `OptionChainCommand`s, so replay does
    /// not reproduce them: a strike vivified during replay defaults to
    /// [`Active`](crate::orderbook::InstrumentStatus). Consequently an `AddOrder`
    /// that the live run rejected because its strike had been halted will instead
    /// rest during replay. Journaling status / lifecycle transitions as commands
    /// is required to close this gap (tracked separately).
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
            self.sequencer
                .sequence
                .fetch_max(max_next, Ordering::Release);
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
        // Probe the structural state of one contract: top-of-book on each side
        // plus the count of resting orders. This — not the registry-assigned
        // instrument ids — is the equality oracle for live-vs-replayed state.
        fn probe(
            book: &SequencedUnderlyingOrderBook,
            symbol: &str,
        ) -> (Option<u128>, Option<u128>, usize) {
            let leaf = book
                .find_book_by_symbol(symbol)
                .expect("contract must resolve");
            (leaf.best_bid(), leaf.best_ask(), leaf.order_count())
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

        // Expected live structure after the stream:
        //   50c: bid 100, no ask, 1 order   50p: bid 50, no ask, 1 order
        //   55c: empty                       60c: no bid, ask 200, 1 order
        assert_eq!(probe(&live, sym_50c), (Some(100), None, 1));
        assert_eq!(probe(&live, sym_50p), (Some(50), None, 1));
        assert_eq!(probe(&live, sym_55c), (None, None, 0));
        assert_eq!(probe(&live, sym_60c), (None, Some(200), 1));
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
            },
            result: OptionChainResult::OrderAdded {
                order_id: OrderId::new(),
            },
        };

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: OptionChainEvent = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.sequence_num, 42);
        assert_eq!(deserialized.timestamp_ns, 1_000_000);
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
}
