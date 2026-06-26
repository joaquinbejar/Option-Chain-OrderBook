//! NATS JetStream integration for option chain events.
//!
//! This module provides NATS event publishing for the option chain hierarchy,
//! with hierarchical subjects encoding the full instrument path. Events are
//! published to subjects like:
//!
//! - `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
//! - `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
//!
//! Subscribers can use NATS wildcards to filter by any level:
//!
//! - `optionchain.trades.BTC.>` — all BTC option trades
//! - `optionchain.book.ETH.20240329.>` — all ETH March 2024 book changes
//! - `optionchain.trades.*.*.50000.C` — all 50000-strike calls across underlyings
//!
//! # Feature Gate
//!
//! This module is only available when the `nats` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! option-chain-orderbook = { version = "0.7", features = ["nats"] }
//! ```
//!
//! # Example
//!
//! ```rust,no_run
//! use option_chain_orderbook::orderbook::nats::{
//!     build_option_order_book_with_nats, OptionChainNatsConfig,
//! };
//! use optionstratlib::OptionStyle;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = async_nats::connect("nats://localhost:4222").await?;
//! let jetstream = async_nats::jetstream::new(client);
//! let handle = tokio::runtime::Handle::current();
//!
//! // `try_new` rejects an empty prefix or one carrying a NATS wildcard.
//! let config = OptionChainNatsConfig::try_new(jetstream, "optionchain", handle)?;
//! // Build a contract order book with trade + book-change publishers attached
//! // *before* the inner book is wrapped in `Arc` (the only valid install point).
//! let (book, handles) =
//!     build_option_order_book_with_nats("BTC-20240329-50000-C", OptionStyle::Call, &config)?;
//! // `handles` exposes publish metrics and an async `shutdown()`.
//! # let _ = (book, handles);
//! # Ok(())
//! # }
//! ```

use super::book::{
    BookConfig, ContractNatsListenerFactory, OptionOrderBook, PreparedNatsListeners,
};
use super::underlying::UnderlyingOrderBookManager;
use crate::error::Error;
use crate::utils::SymbolParser;
use optionstratlib::OptionStyle;
use orderbook_rs::prelude::{NatsBookChangePublisher, NatsTradePublisher};
use std::sync::Arc;

/// Reserved NATS subject characters that must never appear inside a single
/// subject token: the level separator `.`, the single-token wildcard `*`, and
/// the multi-token wildcard `>`. Allowing any of them inside a token would
/// inject an extra subject level or a wildcard and silently break the
/// level-scoped subscriptions the subject scheme is designed for.
const RESERVED_SUBJECT_CHARS: [char; 3] = ['.', '*', '>'];

/// Validates a single NATS subject token (one path component).
///
/// Rejects an empty token or one containing a [reserved subject
/// character](RESERVED_SUBJECT_CHARS). This is the shared validation primitive
/// behind [`OptionChainSubjectBuilder::from_symbol`],
/// [`OptionChainSubjectBuilder::try_new`], and the prefix validation in
/// [`OptionChainNatsConfig::try_new`], so every construction path enforces the
/// exact same rule.
///
/// # Errors
///
/// Returns [`Error::NatsSubject`] naming `field` if `value` is empty or carries
/// a reserved subject character.
fn validate_subject_token(field: &str, value: &str) -> Result<(), Error> {
    if value.is_empty() {
        return Err(Error::nats_subject(field, "must not be empty"));
    }
    if let Some(bad) = value.chars().find(|c| RESERVED_SUBJECT_CHARS.contains(c)) {
        return Err(Error::nats_subject(
            field,
            format!("must not contain reserved NATS character '{bad}'"),
        ));
    }
    Ok(())
}

/// Validates a NATS subject prefix.
///
/// A prefix may itself span multiple dot-separated tokens (e.g.
/// `"exchange.optionchain"`), so it is validated token-by-token: each token
/// must be non-empty — rejecting an empty prefix and any leading, trailing, or
/// doubled `.` — and free of the `*`/`>` wildcards.
///
/// # Errors
///
/// Returns [`Error::NatsSubject`] if the prefix is empty, has an empty token
/// (leading/trailing/doubled `.`), or carries a `*`/`>` wildcard.
fn validate_subject_prefix(prefix: &str) -> Result<(), Error> {
    for token in prefix.split('.') {
        validate_subject_token("prefix", token)?;
    }
    Ok(())
}

/// Normalizes a case-insensitive option-type string into an [`OptionStyle`].
///
/// Accepts `"C"`/`"c"`/`"Call"` (any case) as a call and `"P"`/`"p"`/`"Put"`
/// (any case) as a put, so the derived subject token is always the canonical
/// `C`/`P` regardless of how the caller spelled it.
///
/// # Errors
///
/// Returns [`Error::NatsSubject`] if `value` is not a recognized option type.
fn parse_option_style(value: &str) -> Result<OptionStyle, Error> {
    match value.to_ascii_lowercase().as_str() {
        "c" | "call" => Ok(OptionStyle::Call),
        "p" | "put" => Ok(OptionStyle::Put),
        _ => Err(Error::nats_subject(
            "option_type",
            format!("expected one of C/Call/P/Put (case-insensitive), got '{value}'"),
        )),
    }
}

/// Configuration for connecting NATS publishers to the option chain hierarchy.
///
/// This struct holds the JetStream context, subject prefix, and Tokio runtime
/// handle needed to create NATS publishers at any level of the hierarchy.
///
/// # Subject Format
///
/// Events are published with hierarchical subjects:
///
/// - Trades: `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
/// - Book changes: `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
///
/// Where:
/// - `prefix` is the configured subject prefix (e.g., `"optionchain"`)
/// - `underlying` is the underlying asset symbol (e.g., `"BTC"`)
/// - `expiry` is the expiration date in YYYYMMDD format (e.g., `"20240329"`)
/// - `strike` is the strike price (e.g., `"50000"`)
/// - `type` is `"C"` for call or `"P"` for put
#[derive(Clone)]
pub struct OptionChainNatsConfig {
    /// JetStream context for publishing messages.
    jetstream: async_nats::jetstream::Context,

    /// Subject prefix (e.g., `"optionchain"`).
    subject_prefix: String,

    /// Handle to the Tokio runtime used for spawning async publish tasks.
    runtime: tokio::runtime::Handle,
}

impl OptionChainNatsConfig {
    /// Creates a new NATS configuration for option chain event publishing,
    /// validating the subject prefix.
    ///
    /// # Arguments
    ///
    /// * `jetstream` - JetStream context obtained from an `async_nats` client
    /// * `subject_prefix` - prefix for all NATS subjects (e.g., `"optionchain"`)
    /// * `runtime` - handle to the Tokio runtime for spawning publish tasks
    ///
    /// # Errors
    ///
    /// Returns [`Error::NatsSubject`] if `subject_prefix` is empty, has an empty
    /// token (a leading, trailing, or doubled `.`), or carries a `*`/`>`
    /// wildcard — any of which would corrupt the subject scheme or inject a
    /// wildcard into published subjects.
    pub fn try_new(
        jetstream: async_nats::jetstream::Context,
        subject_prefix: impl Into<String>,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, Error> {
        let subject_prefix = subject_prefix.into();
        validate_subject_prefix(&subject_prefix)?;
        Ok(Self {
            jetstream,
            subject_prefix,
            runtime,
        })
    }

    /// Returns a reference to the JetStream context.
    #[must_use]
    #[inline]
    pub fn jetstream(&self) -> &async_nats::jetstream::Context {
        &self.jetstream
    }

    /// Returns the subject prefix.
    #[must_use]
    #[inline]
    pub fn subject_prefix(&self) -> &str {
        &self.subject_prefix
    }

    /// Returns the Tokio runtime handle.
    #[must_use]
    #[inline]
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }
}

impl std::fmt::Debug for OptionChainNatsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionChainNatsConfig")
            .field("subject_prefix", &self.subject_prefix)
            .finish_non_exhaustive()
    }
}

/// Builds hierarchical NATS subjects from option symbol components.
///
/// This struct parses an option symbol (e.g., `"BTC-20240329-50000-C"`) and
/// generates appropriate NATS subjects for trade and book change events.
///
/// # Subject Format
///
/// - Trades: `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
/// - Book: `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
#[derive(Debug, Clone)]
pub struct OptionChainSubjectBuilder {
    /// Underlying asset symbol (e.g., `"BTC"`).
    underlying: String,
    /// Expiration date in YYYYMMDD format (e.g., `"20240329"`).
    expiry: String,
    /// Strike price as string (e.g., `"50000"`).
    strike: String,
    /// Option style; the subject token is always derived as `C`/`P`.
    option_type: OptionStyle,
}

impl OptionChainSubjectBuilder {
    /// Parses an option symbol into its components.
    ///
    /// Expected format: `{underlying}-{expiry}-{strike}-{type}`
    ///
    /// # Arguments
    ///
    /// * `symbol` - Option symbol (e.g., `"BTC-20240329-50000-C"`)
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSymbol`](crate::error::Error::InvalidSymbol) if
    /// the symbol does not match the expected grammar, or
    /// [`Error::NatsSubject`](crate::error::Error::NatsSubject) if the
    /// (already grammar-valid) underlying carries a reserved NATS subject
    /// character (`.`, `*`, `>`).
    ///
    /// # Example
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::nats::OptionChainSubjectBuilder;
    ///
    /// let builder = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000-C")
    ///     .expect("well-formed contract symbol should parse");
    /// assert_eq!(builder.underlying(), "BTC");
    /// assert_eq!(builder.expiry(), "20240329");
    /// assert_eq!(builder.strike(), "50000");
    /// assert_eq!(builder.option_type(), "C");
    /// ```
    pub fn from_symbol(symbol: &str) -> Result<Self, Error> {
        // The `{underlying}-{expiry}-{strike}-{type}` grammar is owned by
        // `SymbolParser` (the single source of truth): 4 parts, an 8-digit
        // canonical date, a positive integer strike and a case-insensitive
        // `C`/`P` type. NATS adds only subject-character escaping on top, via
        // the same `try_new` path so both constructors validate identically.
        let parsed = SymbolParser::parse(symbol)?;
        Self::try_new(
            parsed.underlying(),
            parsed.expiration_str(),
            parsed.strike().to_string(),
            match parsed.option_style() {
                OptionStyle::Call => "C",
                OptionStyle::Put => "P",
            },
        )
    }

    /// Creates a subject builder from explicit components, validating each one.
    ///
    /// Every component is checked against the NATS subject grammar so no
    /// construction path can inject an extra subject level or a wildcard. The
    /// `option_type` is normalized case-insensitively into an [`OptionStyle`]
    /// (identically to [`from_symbol`](Self::from_symbol)), so the emitted
    /// subject token is always the canonical `C`/`P`.
    ///
    /// # Arguments
    ///
    /// * `underlying` - Underlying asset symbol (e.g. `"BTC"`)
    /// * `expiry` - Expiration date (YYYYMMDD format)
    /// * `strike` - Strike price
    /// * `option_type` - `C`/`Call` or `P`/`Put` (case-insensitive)
    ///
    /// # Errors
    ///
    /// Returns [`Error::NatsSubject`](crate::error::Error::NatsSubject) if any
    /// of `underlying`, `expiry`, or `strike` is empty or carries a reserved
    /// NATS subject character (`.`, `*`, `>`), or if `option_type` is not a
    /// recognized `C`/`Call`/`P`/`Put` value.
    pub fn try_new(
        underlying: impl Into<String>,
        expiry: impl Into<String>,
        strike: impl Into<String>,
        option_type: impl AsRef<str>,
    ) -> Result<Self, Error> {
        let underlying = underlying.into();
        let expiry = expiry.into();
        let strike = strike.into();

        validate_subject_token("underlying", &underlying)?;
        validate_subject_token("expiry", &expiry)?;
        validate_subject_token("strike", &strike)?;
        let option_type = parse_option_style(option_type.as_ref())?;

        Ok(Self {
            underlying,
            expiry,
            strike,
            option_type,
        })
    }

    /// Returns the underlying asset symbol.
    #[must_use]
    #[inline]
    pub fn underlying(&self) -> &str {
        &self.underlying
    }

    /// Returns the expiration date.
    #[must_use]
    #[inline]
    pub fn expiry(&self) -> &str {
        &self.expiry
    }

    /// Returns the strike price.
    #[must_use]
    #[inline]
    pub fn strike(&self) -> &str {
        &self.strike
    }

    /// Returns the option subject token (`"C"` for a call, `"P"` for a put).
    #[must_use]
    #[inline]
    pub fn option_type(&self) -> &'static str {
        match self.option_type {
            OptionStyle::Call => "C",
            OptionStyle::Put => "P",
        }
    }

    /// Returns the option style (`OptionStyle::Call` or `OptionStyle::Put`).
    #[must_use]
    #[inline]
    pub const fn option_style(&self) -> OptionStyle {
        self.option_type
    }

    /// Builds a trade event subject.
    ///
    /// Format: `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
    #[must_use]
    pub fn trade_subject(&self, prefix: &str) -> String {
        format!(
            "{}.trades.{}.{}.{}.{}",
            prefix,
            self.underlying,
            self.expiry,
            self.strike,
            self.option_type()
        )
    }

    /// Builds a book change event subject.
    ///
    /// Format: `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
    #[must_use]
    pub fn book_subject(&self, prefix: &str) -> String {
        format!(
            "{}.book.{}.{}.{}.{}",
            prefix,
            self.underlying,
            self.expiry,
            self.strike,
            self.option_type()
        )
    }

    /// Builds both trade and book subjects as a tuple.
    ///
    /// Returns `(trade_subject, book_subject)`.
    #[must_use]
    pub fn subjects(&self, prefix: &str) -> (String, String) {
        (self.trade_subject(prefix), self.book_subject(prefix))
    }
}

/// Handles to the NATS publishers wired into an [`OptionOrderBook`].
///
/// Returned by [`build_option_order_book_with_nats`]. The listeners themselves
/// are already installed on the inner order book; these handles are retained so
/// the caller can read publish metrics and drive a clean async shutdown of the
/// background batch tasks the publishers spawn.
///
/// Dropping the handles does **not** stop the background tasks immediately —
/// call [`shutdown`](Self::shutdown) for a graceful drain.
pub struct NatsPublisherHandles {
    /// Handle to the trade publisher (metrics + shutdown).
    pub trade_handle: Arc<NatsTradePublisher>,
    /// Handle to the book-change publisher (metrics + shutdown).
    pub book_handle: Arc<NatsBookChangePublisher>,
}

impl NatsPublisherHandles {
    /// Returns the number of successfully published trades.
    #[must_use]
    #[inline]
    pub fn trade_publish_count(&self) -> u64 {
        self.trade_handle.publish_count()
    }

    /// Returns the number of permanently failed trade publishes.
    #[must_use]
    #[inline]
    pub fn trade_error_count(&self) -> u64 {
        self.trade_handle.error_count()
    }

    /// Returns the number of successfully published book-change events.
    #[must_use]
    #[inline]
    pub fn book_publish_count(&self) -> u64 {
        self.book_handle.publish_count()
    }

    /// Returns the number of permanently failed book-change publishes.
    #[must_use]
    #[inline]
    pub fn book_error_count(&self) -> u64 {
        self.book_handle.error_count()
    }

    /// Gracefully shuts down both publishers, draining and awaiting their
    /// background batch tasks. Provides the cancellation/await path required of
    /// every spawned task in this crate.
    pub async fn shutdown(&self) {
        self.trade_handle.shutdown().await;
        self.book_handle.shutdown().await;
    }
}

impl std::fmt::Debug for NatsPublisherHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsPublisherHandles")
            .field("trade_publish_count", &self.trade_publish_count())
            .field("trade_error_count", &self.trade_error_count())
            .field("book_publish_count", &self.book_publish_count())
            .field("book_error_count", &self.book_error_count())
            .finish_non_exhaustive()
    }
}

/// Builds an [`OptionOrderBook`] with NATS trade and book-change publishers
/// attached.
///
/// `orderbook_rs` only permits a single trade listener and a single book-change
/// listener, both of which must be registered while the inner `OrderBook<T>`
/// is still owned mutably — i.e. *before* it is wrapped in `Arc`. This function
/// constructs both publishers, converts them into their `orderbook_rs`-native
/// listener callbacks, and threads those callbacks into the contract order book
/// at construction time via [`OptionOrderBook::new_with_config`]. The trade
/// publisher listener is multiplexed with the internal trade-capture listener.
///
/// The dependency direction is `nats` → `book`: this eventing-layer function
/// consumes the leaf; the leaf never imports this module.
///
/// # Subject Format
///
/// - Trades: `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
/// - Book changes: `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
///
/// # Arguments
///
/// * `symbol` - The option contract symbol (e.g. `"BTC-20240329-50000-C"`)
/// * `option_style` - The option style (Call or Put)
/// * `config` - NATS configuration with JetStream context, subject prefix, and
///   Tokio runtime handle
///
/// # Returns
///
/// The constructed [`OptionOrderBook`] (with publishers already wired) and the
/// [`NatsPublisherHandles`] for metrics and shutdown.
///
/// # Errors
///
/// Returns [`Error::InvalidSymbol`](crate::error::Error::InvalidSymbol) if the
/// symbol cannot be parsed into its `{underlying}-{expiry}-{strike}-{type}`
/// components, or [`Error::NatsSubject`](crate::error::Error::NatsSubject) if a
/// (grammar-valid) component carries a reserved NATS subject character.
#[must_use = "the returned order book and publisher handles must be retained; \
              dropping them tears the publishers down"]
pub fn build_option_order_book_with_nats(
    symbol: impl Into<String>,
    option_style: OptionStyle,
    config: &OptionChainNatsConfig,
) -> crate::Result<(OptionOrderBook, NatsPublisherHandles)> {
    let symbol = symbol.into();
    let subject = OptionChainSubjectBuilder::from_symbol(&symbol)?;
    let trade_subject = subject.trade_subject(config.subject_prefix());
    let book_subject = subject.book_subject(config.subject_prefix());

    // Build the trade publisher and convert it into its listener callback.
    let trade_publisher = NatsTradePublisher::new(
        config.jetstream().clone(),
        trade_subject,
        config.runtime().clone(),
    );
    let (trade_handle, trade_listener) = trade_publisher.into_listener();

    // Build the book-change publisher and convert it into its listener callback.
    let book_change_publisher = NatsBookChangePublisher::new(
        config.jetstream().clone(),
        symbol.clone(),
        book_subject,
        config.runtime().clone(),
    );
    let (book_handle, book_listener) = book_change_publisher.into_listener();

    // Install both listeners on the inner book pre-`Arc` via the constructor.
    let book = OptionOrderBook::new_with_config(
        symbol,
        option_style,
        BookConfig {
            nats_listeners: Some(PreparedNatsListeners {
                trade_listener,
                book_listener,
            }),
            ..BookConfig::default()
        },
    );

    tracing::info!(
        symbol = %book.symbol(),
        prefix = %config.subject_prefix(),
        "nats trade and book-change publishers attached to option order book"
    );

    Ok((
        book,
        NatsPublisherHandles {
            trade_handle,
            book_handle,
        },
    ))
}

/// Builds the per-contract NATS listener factory backing
/// [`build_underlying_manager_with_nats`].
///
/// The returned factory, when invoked by the hierarchy with a contract symbol,
/// derives that contract's hierarchical trade/book subjects via
/// [`OptionChainSubjectBuilder`], constructs the trade and book-change
/// publishers, and converts them into their `orderbook_rs`-native listeners for
/// the hierarchy to install pre-`Arc`. The publisher handles are intentionally
/// dropped, leaving each publisher owned by its background batch task and its
/// listener closure; teardown is therefore drop-driven (see
/// [`build_underlying_manager_with_nats`]).
///
/// A symbol that cannot be turned into a valid subject yields `None` (logged at
/// `ERROR`), so the contract book is still created — just without publishers —
/// rather than failing book creation.
#[must_use]
fn make_contract_nats_factory(config: OptionChainNatsConfig) -> ContractNatsListenerFactory {
    Arc::new(move |symbol: &str| {
        let subject = match OptionChainSubjectBuilder::from_symbol(symbol) {
            Ok(subject) => subject,
            Err(err) => {
                tracing::error!(
                    symbol,
                    error = %err,
                    "nats: cannot derive subject for lazily-created contract; \
                     publishers not attached"
                );
                return None;
            }
        };
        let trade_subject = subject.trade_subject(config.subject_prefix());
        let book_subject = subject.book_subject(config.subject_prefix());

        // Build the trade publisher and convert it into its listener callback.
        let trade_publisher = NatsTradePublisher::new(
            config.jetstream().clone(),
            trade_subject,
            config.runtime().clone(),
        );
        let (_trade_handle, trade_listener) = trade_publisher.into_listener();

        // Build the book-change publisher and convert it into its listener.
        let book_change_publisher = NatsBookChangePublisher::new(
            config.jetstream().clone(),
            symbol.to_string(),
            book_subject,
            config.runtime().clone(),
        );
        let (_book_handle, book_listener) = book_change_publisher.into_listener();

        tracing::debug!(
            symbol,
            prefix = %config.subject_prefix(),
            "nats publishers attached to lazily-created contract book"
        );

        Some(PreparedNatsListeners {
            trade_listener,
            book_listener,
        })
    })
}

/// Builds an [`UnderlyingOrderBookManager`] that attaches per-contract NATS
/// publishers to **every** contract book the hierarchy lazily creates.
///
/// Unlike [`build_option_order_book_with_nats`], which wires a single standalone
/// [`OptionOrderBook`], this wires the whole hierarchy: each call/put book
/// created on demand via `get_or_create_*` installs its own trade and
/// book-change publishers — bound to that contract's hierarchical subjects —
/// *before* the inner book is wrapped in `Arc`. The wiring rides an
/// `orderbook_rs`-native listener factory threaded down to the leaf, so the core
/// hierarchy never imports this module.
///
/// # Subject Format
///
/// Each contract publishes to the same subjects as
/// [`build_option_order_book_with_nats`]:
///
/// - Trades: `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
/// - Book changes: `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
///
/// # Shutdown
///
/// The publishers attached to lazily-created books are bound to their book's
/// lifetime: dropping a contract book (strike/expiry cleanup, or dropping the
/// manager) closes the publish channels, and each publisher's background batch
/// task drains and exits deterministically. For standalone books that need
/// explicit publish metrics or an awaitable async shutdown, use
/// [`build_option_order_book_with_nats`], which returns [`NatsPublisherHandles`].
#[must_use = "the returned manager owns the hierarchy whose contract books \
              publish to NATS; dropping it tears the publishers down"]
pub fn build_underlying_manager_with_nats(
    config: OptionChainNatsConfig,
) -> UnderlyingOrderBookManager {
    let prefix = config.subject_prefix().to_string();
    let factory = make_contract_nats_factory(config);
    let manager = UnderlyingOrderBookManager::with_nats_factory(factory);
    tracing::info!(
        prefix = %prefix,
        "underlying order book manager wired with per-contract nats publishers"
    );
    manager
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subject_builder_from_symbol() {
        let builder = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000-C").unwrap();
        assert_eq!(builder.underlying(), "BTC");
        assert_eq!(builder.expiry(), "20240329");
        assert_eq!(builder.strike(), "50000");
        assert_eq!(builder.option_type(), "C");
    }

    #[test]
    fn test_subject_builder_from_symbol_put() {
        let builder = OptionChainSubjectBuilder::from_symbol("ETH-20240628-3000-P").unwrap();
        assert_eq!(builder.underlying(), "ETH");
        assert_eq!(builder.expiry(), "20240628");
        assert_eq!(builder.strike(), "3000");
        assert_eq!(builder.option_type(), "P");
    }

    #[test]
    fn test_subject_builder_lowercase_type() {
        let builder = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000-c").unwrap();
        assert_eq!(builder.option_type(), "C");
    }

    #[test]
    fn test_subject_builder_invalid_parts() {
        let result = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_symbol_matches_symbol_parser_parse() {
        for symbol in [
            "BTC-20240329-50000-C",
            "ETH-20240628-3000-P",
            "BTC-20240329-50000-c",
        ] {
            let parsed = SymbolParser::parse(symbol).expect("symbol parser must parse");
            let builder =
                OptionChainSubjectBuilder::from_symbol(symbol).expect("from_symbol must parse");

            assert_eq!(builder.underlying(), parsed.underlying());
            assert_eq!(builder.expiry(), parsed.expiration_str());
            assert_eq!(builder.strike(), parsed.strike().to_string());
            let expected_type = match parsed.option_style() {
                OptionStyle::Call => "C",
                OptionStyle::Put => "P",
            };
            assert_eq!(builder.option_type(), expected_type);
        }
    }

    #[test]
    fn test_subject_builder_invalid_type() {
        let result = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000-X");
        assert!(result.is_err());
    }

    #[test]
    fn test_trade_subject() {
        let builder = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000-C").unwrap();
        assert_eq!(
            builder.trade_subject("optionchain"),
            "optionchain.trades.BTC.20240329.50000.C"
        );
    }

    #[test]
    fn test_book_subject() {
        let builder = OptionChainSubjectBuilder::from_symbol("ETH-20240628-3000-P").unwrap();
        assert_eq!(
            builder.book_subject("optionchain"),
            "optionchain.book.ETH.20240628.3000.P"
        );
    }

    #[test]
    fn test_subjects_tuple() {
        let builder = OptionChainSubjectBuilder::try_new("BTC", "20240329", "50000", "C")
            .expect("valid components");
        let (trade, book) = builder.subjects("oc");
        assert_eq!(trade, "oc.trades.BTC.20240329.50000.C");
        assert_eq!(book, "oc.book.BTC.20240329.50000.C");
    }

    // ── from_symbol validation edge cases ────────────────────────────────

    #[test]
    fn test_from_symbol_empty_underlying() {
        let result = OptionChainSubjectBuilder::from_symbol("-20240329-50000-C");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_symbol_empty_strike() {
        let result = OptionChainSubjectBuilder::from_symbol("BTC-20240329--C");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_symbol_nats_wildcard_in_underlying() {
        let result = OptionChainSubjectBuilder::from_symbol("BT*C-20240329-50000-C");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_symbol_nats_dot_in_strike() {
        let result = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50.000-C");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_symbol_nats_greater_than_in_expiry() {
        let result = OptionChainSubjectBuilder::from_symbol("BTC-2024>329-50000-C");
        assert!(result.is_err());
    }

    // ── try_new component validation ─────────────────────────────────────

    #[test]
    fn test_subject_builder_try_new_dot_in_underlying_rejected() {
        let result = OptionChainSubjectBuilder::try_new("BT.C", "20240329", "50000", "C");
        assert!(matches!(result, Err(Error::NatsSubject { .. })));
    }

    #[test]
    fn test_subject_builder_try_new_wildcard_star_in_strike_rejected() {
        let result = OptionChainSubjectBuilder::try_new("BTC", "20240329", "500*0", "C");
        assert!(matches!(result, Err(Error::NatsSubject { .. })));
    }

    #[test]
    fn test_subject_builder_try_new_greater_than_in_expiry_rejected() {
        let result = OptionChainSubjectBuilder::try_new("BTC", "2024>329", "50000", "C");
        assert!(matches!(result, Err(Error::NatsSubject { .. })));
    }

    #[test]
    fn test_subject_builder_try_new_empty_underlying_rejected() {
        let result = OptionChainSubjectBuilder::try_new("", "20240329", "50000", "C");
        assert!(matches!(result, Err(Error::NatsSubject { .. })));
    }

    #[test]
    fn test_subject_builder_try_new_invalid_option_type_rejected() {
        let result = OptionChainSubjectBuilder::try_new("BTC", "20240329", "50000", "X");
        assert!(matches!(result, Err(Error::NatsSubject { .. })));
    }

    #[test]
    fn test_subject_builder_try_new_option_type_variants_serialize_to_token() {
        for (input, expected) in [
            ("Call", "C"),
            ("c", "C"),
            ("C", "C"),
            ("P", "P"),
            ("put", "P"),
            ("Put", "P"),
        ] {
            let builder = OptionChainSubjectBuilder::try_new("BTC", "20240329", "50000", input)
                .expect("valid option type");
            assert_eq!(
                builder.option_type(),
                expected,
                "option_type input {input:?} must serialize to {expected}"
            );
        }
    }

    #[test]
    fn test_subject_builder_try_new_valid_builds_documented_subject() {
        let builder = OptionChainSubjectBuilder::try_new("BTC", "20240329", "50000", "call")
            .expect("valid components");
        assert_eq!(
            builder.trade_subject("optionchain"),
            "optionchain.trades.BTC.20240329.50000.C"
        );
        assert_eq!(
            builder.book_subject("optionchain"),
            "optionchain.book.BTC.20240329.50000.C"
        );
    }

    #[test]
    fn test_subject_builder_try_new_matches_from_symbol() {
        let from_symbol =
            OptionChainSubjectBuilder::from_symbol("ETH-20240628-3000-P").expect("from_symbol");
        let try_new =
            OptionChainSubjectBuilder::try_new("ETH", "20240628", "3000", "p").expect("try_new");
        assert_eq!(from_symbol.trade_subject("oc"), try_new.trade_subject("oc"));
        assert_eq!(from_symbol.option_style(), try_new.option_style());
    }

    // ── subject prefix validation (config constructor path) ──────────────

    #[test]
    fn test_validate_subject_prefix_empty_rejected() {
        assert!(matches!(
            validate_subject_prefix(""),
            Err(Error::NatsSubject { .. })
        ));
    }

    #[test]
    fn test_validate_subject_prefix_wildcard_star_rejected() {
        assert!(matches!(
            validate_subject_prefix("opt*ion"),
            Err(Error::NatsSubject { .. })
        ));
    }

    #[test]
    fn test_validate_subject_prefix_greater_than_rejected() {
        assert!(matches!(
            validate_subject_prefix("option>"),
            Err(Error::NatsSubject { .. })
        ));
    }

    #[test]
    fn test_validate_subject_prefix_trailing_dot_rejected() {
        assert!(matches!(
            validate_subject_prefix("optionchain."),
            Err(Error::NatsSubject { .. })
        ));
    }

    #[test]
    fn test_validate_subject_prefix_leading_dot_rejected() {
        assert!(matches!(
            validate_subject_prefix(".optionchain"),
            Err(Error::NatsSubject { .. })
        ));
    }

    #[test]
    fn test_validate_subject_prefix_simple_ok() {
        assert!(validate_subject_prefix("optionchain").is_ok());
    }

    #[test]
    fn test_validate_subject_prefix_multi_token_ok() {
        // A dotted prefix is allowed: it simply prepends extra subject levels.
        assert!(validate_subject_prefix("exchange.optionchain").is_ok());
    }

    /// End-to-end (offline) check that a contract book created purely through
    /// the lazy hierarchy (`get_or_create_*`) has its per-contract publishers
    /// installed and that a matched trade reaches them on the configured
    /// per-contract subject.
    ///
    /// Mirrors the leaf `nats` seam test in `book.rs` (#58): the factory installs
    /// *capturable stand-in* listeners that derive the real per-contract subject
    /// via [`OptionChainSubjectBuilder`] — the same derivation the production
    /// factory uses — so no live NATS server is required. This exercises the
    /// full factory threading: root manager → underlying → expiration → chain →
    /// strike → leaf `OptionOrderBook`.
    #[test]
    fn test_lazily_created_contract_book_publishes_to_per_contract_subject() {
        use optionstratlib::ExpirationDate;
        use optionstratlib::prelude::pos_or_panic;
        use orderbook_rs::{
            OrderId, PriceLevelChangedEvent, PriceLevelChangedListener, Side, TradeListener,
            TradeResult,
        };
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU64, Ordering};

        let prefix = "optionchain";

        // symbol -> (derived trade subject, trade-listener fire count).
        type Captured = HashMap<String, (String, Arc<AtomicU64>)>;
        let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(HashMap::new()));

        // Stand-in factory: derives the same per-contract subject the production
        // factory would, and installs a capturing trade listener — exactly the
        // book.rs #58 seam, but reached through the lazy hierarchy.
        let captured_factory = Arc::clone(&captured);
        let prefix_owned = prefix.to_string();
        let factory: ContractNatsListenerFactory = Arc::new(move |symbol: &str| {
            let subject = OptionChainSubjectBuilder::from_symbol(symbol).ok()?;
            let trade_subject = subject.trade_subject(&prefix_owned);
            let count = Arc::new(AtomicU64::new(0));
            captured_factory
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(symbol.to_string(), (trade_subject, Arc::clone(&count)));
            let count_c = Arc::clone(&count);
            let trade_listener: TradeListener = Arc::new(move |_tr: &TradeResult| {
                count_c.fetch_add(1, Ordering::Relaxed);
            });
            let book_listener: PriceLevelChangedListener =
                Arc::new(|_ev: PriceLevelChangedEvent| {});
            Some(PreparedNatsListeners {
                trade_listener,
                book_listener,
            })
        });

        let manager = UnderlyingOrderBookManager::with_nats_factory(factory);

        // Create a contract book lazily through every hierarchy level.
        let btc = manager.get_or_create("BTC");
        let exp = btc.get_or_create_expiration(ExpirationDate::Days(pos_or_panic!(30.0)));
        let strike = exp.get_or_create_strike(50_000);
        let call = strike.call();
        let call_symbol = call.symbol().to_string();

        // The expected subject is derived from the actual lazily-built symbol,
        // so the assertion is independent of today's date.
        let expected_subject = OptionChainSubjectBuilder::from_symbol(&call_symbol)
            .expect("lazily-built call symbol must parse")
            .trade_subject(prefix);

        // Rest a sell, then cross it with a marketable buy to force a trade.
        call.add_limit_order(OrderId::new(), Side::Sell, 100, 5)
            .expect("rest sell");
        call.add_limit_order(OrderId::new(), Side::Buy, 100, 5)
            .expect("cross buy");

        let guard = captured.lock().unwrap_or_else(|p| p.into_inner());
        let (subject, count) = guard
            .get(&call_symbol)
            .expect("publishers must be wired into the lazily-created call book");
        assert_eq!(
            subject, &expected_subject,
            "lazily-created call book must publish to its per-contract subject"
        );
        assert!(
            count.load(Ordering::Relaxed) >= 1,
            "a matched trade on a get_or_create_* strike must reach its per-contract publisher"
        );
    }

    // `OptionChainNatsConfig::try_new` delegates its prefix validation entirely
    // to `validate_subject_prefix` (covered exhaustively above) before touching
    // the JetStream context, so the constructor's reject/accept behavior cannot
    // diverge from those cases. Exercising the constructor end-to-end needs a
    // live NATS server (no offline `jetstream::Context` exists) and the
    // `tokio/macros` runtime, neither of which is in the default test matrix.
}
