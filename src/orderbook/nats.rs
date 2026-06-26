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
//! option-chain-orderbook = { version = "0.4", features = ["nats"] }
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
//! let config = OptionChainNatsConfig::new(jetstream, "optionchain".to_string(), handle);
//! // Build a contract order book with trade + book-change publishers attached
//! // *before* the inner book is wrapped in `Arc` (the only valid install point).
//! let (book, handles) =
//!     build_option_order_book_with_nats("BTC-20240329-50000-C", OptionStyle::Call, &config)?;
//! // `handles` exposes publish metrics and an async `shutdown()`.
//! # let _ = (book, handles);
//! # Ok(())
//! # }
//! ```

use super::book::{BookConfig, OptionOrderBook, PreparedNatsListeners};
use crate::error::Error;
use crate::utils::SymbolParser;
use optionstratlib::OptionStyle;
use orderbook_rs::prelude::{NatsBookChangePublisher, NatsTradePublisher};
use std::sync::Arc;

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
    /// Creates a new NATS configuration for option chain event publishing.
    ///
    /// # Arguments
    ///
    /// * `jetstream` - JetStream context obtained from an `async_nats` client
    /// * `subject_prefix` - prefix for all NATS subjects (e.g., `"optionchain"`)
    /// * `runtime` - handle to the Tokio runtime for spawning publish tasks
    #[inline]
    #[must_use]
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        subject_prefix: String,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            jetstream,
            subject_prefix,
            runtime,
        }
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
    /// Option type: `"C"` for call, `"P"` for put.
    option_type: String,
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
    /// Returns an error if the symbol does not match the expected format, or if
    /// the (already grammar-valid) underlying contains a NATS subject-reserved
    /// character (`.`, `*`, `>`).
    ///
    /// # Example
    ///
    /// ```
    /// use option_chain_orderbook::orderbook::nats::OptionChainSubjectBuilder;
    ///
    /// let builder = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000-C").unwrap();
    /// assert_eq!(builder.underlying(), "BTC");
    /// assert_eq!(builder.expiry(), "20240329");
    /// assert_eq!(builder.strike(), "50000");
    /// assert_eq!(builder.option_type(), "C");
    /// ```
    pub fn from_symbol(symbol: &str) -> Result<Self, Error> {
        // The `{underlying}-{expiry}-{strike}-{type}` grammar is owned by
        // `SymbolParser` (the single source of truth): 4 parts, an 8-digit
        // canonical date, a positive integer strike and a case-insensitive
        // `C`/`P` type. NATS adds only subject-character escaping on top.
        let parsed = SymbolParser::parse(symbol)?;

        // The underlying is the only free-form segment that can carry a NATS
        // subject-reserved character; the expiry (digits) and strike (digits)
        // are already constrained by `SymbolParser`.
        let underlying = parsed.underlying();
        if underlying.contains('.') || underlying.contains('*') || underlying.contains('>') {
            return Err(Error::invalid_symbol(
                symbol,
                "underlying contains invalid NATS characters",
            ));
        }

        let option_type = match parsed.option_style() {
            OptionStyle::Call => "C",
            OptionStyle::Put => "P",
        };

        Ok(Self {
            underlying: underlying.to_string(),
            expiry: parsed.expiration_str().to_string(),
            strike: parsed.strike().to_string(),
            option_type: option_type.to_string(),
        })
    }

    /// Creates a subject builder from explicit components.
    ///
    /// # Arguments
    ///
    /// * `underlying` - Underlying asset symbol
    /// * `expiry` - Expiration date (YYYYMMDD format)
    /// * `strike` - Strike price
    /// * `option_type` - `"C"` for call, `"P"` for put
    #[must_use]
    pub fn new(
        underlying: impl Into<String>,
        expiry: impl Into<String>,
        strike: impl Into<String>,
        option_type: impl Into<String>,
    ) -> Self {
        let mut option_type_str = option_type.into();
        // Normalize single-character option types to uppercase for consistency
        if option_type_str.len() == 1 {
            option_type_str.make_ascii_uppercase();
        }

        Self {
            underlying: underlying.into(),
            expiry: expiry.into(),
            strike: strike.into(),
            option_type: option_type_str,
        }
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

    /// Returns the option type (`"C"` or `"P"`).
    #[must_use]
    #[inline]
    pub fn option_type(&self) -> &str {
        &self.option_type
    }

    /// Builds a trade event subject.
    ///
    /// Format: `{prefix}.trades.{underlying}.{expiry}.{strike}.{type}`
    #[must_use]
    pub fn trade_subject(&self, prefix: &str) -> String {
        format!(
            "{}.trades.{}.{}.{}.{}",
            prefix, self.underlying, self.expiry, self.strike, self.option_type
        )
    }

    /// Builds a book change event subject.
    ///
    /// Format: `{prefix}.book.{underlying}.{expiry}.{strike}.{type}`
    #[must_use]
    pub fn book_subject(&self, prefix: &str) -> String {
        format!(
            "{}.book.{}.{}.{}.{}",
            prefix, self.underlying, self.expiry, self.strike, self.option_type
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
/// components or carries a NATS subject-reserved character.
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
        let builder = OptionChainSubjectBuilder::new("BTC", "20240329", "50000", "C");
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
}
