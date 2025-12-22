//! Error types for the Option-Chain-OrderBook library.
//!
//! This module provides a unified error type for all operations in the library,
//! using `thiserror` for ergonomic error handling.

use rust_decimal::Decimal;
use thiserror::Error;

/// Result type alias using the library's error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for the Option-Chain-OrderBook library.
#[derive(Error, Debug)]
pub enum Error {
    /// Error when an option contract is not found.
    #[error("option contract not found: {symbol}")]
    ContractNotFound {
        /// The symbol that was not found.
        symbol: String,
    },

    /// Error when an expiration date is not found.
    #[error("expiration not found: {expiration}")]
    ExpirationNotFound {
        /// The expiration date that was not found.
        expiration: String,
    },

    /// Error when a strike price is not found.
    #[error("strike not found: {strike}")]
    StrikeNotFound {
        /// The strike price that was not found.
        strike: u64,
    },

    /// Error when an underlying is not found.
    #[error("underlying not found: {underlying}")]
    UnderlyingNotFound {
        /// The underlying that was not found.
        underlying: String,
    },

    /// Error when no data is available.
    #[error("no data available: {message}")]
    NoDataAvailable {
        /// Description of what data is missing.
        message: String,
    },

    /// Error when an order book operation fails.
    #[error("order book error: {message}")]
    OrderBookError {
        /// Description of the order book error.
        message: String,
    },

    /// Error when pricing calculation fails.
    #[error("pricing error: {message}")]
    PricingError {
        /// Description of the pricing error.
        message: String,
    },

    /// Error when Greeks calculation fails.
    #[error("greeks calculation error: {message}")]
    GreeksError {
        /// Description of the Greeks error.
        message: String,
    },

    /// Error when inventory limits are exceeded.
    #[error(
        "inventory limit exceeded: {limit_type} limit of {limit} exceeded with value {current}"
    )]
    InventoryLimitExceeded {
        /// Type of limit that was exceeded.
        limit_type: String,
        /// The configured limit value.
        limit: Decimal,
        /// The current value that exceeded the limit.
        current: Decimal,
    },

    /// Error when risk limits are breached.
    #[error("risk limit breached: {limit_type}")]
    RiskLimitBreached {
        /// Type of risk limit that was breached.
        limit_type: String,
    },

    /// Error when hedging operation fails.
    #[error("hedging error: {message}")]
    HedgingError {
        /// Description of the hedging error.
        message: String,
    },

    /// Error when quote generation fails.
    #[error("quoting error: {message}")]
    QuotingError {
        /// Description of the quoting error.
        message: String,
    },

    /// Error when market data is invalid or missing.
    #[error("market data error: {message}")]
    MarketDataError {
        /// Description of the market data error.
        message: String,
    },

    /// Error when exchange adapter operation fails.
    #[error("adapter error for {exchange}: {message}")]
    AdapterError {
        /// The exchange where the error occurred.
        exchange: String,
        /// Description of the adapter error.
        message: String,
    },

    /// Error when configuration is invalid.
    #[error("configuration error: {message}")]
    ConfigurationError {
        /// Description of the configuration error.
        message: String,
    },

    /// Error when a validation check fails.
    #[error("validation error: {message}")]
    ValidationError {
        /// Description of the validation error.
        message: String,
    },

    /// Error when serialization/deserialization fails.
    #[error("serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// Error when a decimal conversion fails.
    #[error("decimal conversion error: {message}")]
    DecimalError {
        /// Description of the decimal error.
        message: String,
    },

    /// Error from optionstratlib decimal operations.
    #[error("optionstratlib decimal error: {0}")]
    OptionStratLibDecimal(#[from] optionstratlib::error::decimal::DecimalError),
}

impl Error {
    /// Creates a new contract not found error.
    #[must_use]
    pub fn contract_not_found(symbol: impl Into<String>) -> Self {
        Self::ContractNotFound {
            symbol: symbol.into(),
        }
    }

    /// Creates a new expiration not found error.
    #[must_use]
    pub fn expiration_not_found(expiration: impl Into<String>) -> Self {
        Self::ExpirationNotFound {
            expiration: expiration.into(),
        }
    }

    /// Creates a new strike not found error.
    #[must_use]
    pub fn strike_not_found(strike: u64) -> Self {
        Self::StrikeNotFound { strike }
    }

    /// Creates a new underlying not found error.
    #[must_use]
    pub fn underlying_not_found(underlying: impl Into<String>) -> Self {
        Self::UnderlyingNotFound {
            underlying: underlying.into(),
        }
    }

    /// Creates a new no data available error.
    #[must_use]
    pub fn no_data(message: impl Into<String>) -> Self {
        Self::NoDataAvailable {
            message: message.into(),
        }
    }

    /// Creates a new order book error.
    #[must_use]
    pub fn orderbook(message: impl Into<String>) -> Self {
        Self::OrderBookError {
            message: message.into(),
        }
    }

    /// Creates a new pricing error.
    #[must_use]
    pub fn pricing(message: impl Into<String>) -> Self {
        Self::PricingError {
            message: message.into(),
        }
    }

    /// Creates a new Greeks error.
    #[must_use]
    pub fn greeks(message: impl Into<String>) -> Self {
        Self::GreeksError {
            message: message.into(),
        }
    }

    /// Creates a new inventory limit exceeded error.
    #[must_use]
    pub fn inventory_limit_exceeded(
        limit_type: impl Into<String>,
        limit: Decimal,
        current: Decimal,
    ) -> Self {
        Self::InventoryLimitExceeded {
            limit_type: limit_type.into(),
            limit,
            current,
        }
    }

    /// Creates a new risk limit breached error.
    #[must_use]
    pub fn risk_limit_breached(limit_type: impl Into<String>) -> Self {
        Self::RiskLimitBreached {
            limit_type: limit_type.into(),
        }
    }

    /// Creates a new hedging error.
    #[must_use]
    pub fn hedging(message: impl Into<String>) -> Self {
        Self::HedgingError {
            message: message.into(),
        }
    }

    /// Creates a new quoting error.
    #[must_use]
    pub fn quoting(message: impl Into<String>) -> Self {
        Self::QuotingError {
            message: message.into(),
        }
    }

    /// Creates a new market data error.
    #[must_use]
    pub fn market_data(message: impl Into<String>) -> Self {
        Self::MarketDataError {
            message: message.into(),
        }
    }

    /// Creates a new adapter error.
    #[must_use]
    pub fn adapter(exchange: impl Into<String>, message: impl Into<String>) -> Self {
        Self::AdapterError {
            exchange: exchange.into(),
            message: message.into(),
        }
    }

    /// Creates a new configuration error.
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::ConfigurationError {
            message: message.into(),
        }
    }

    /// Creates a new validation error.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::ValidationError {
            message: message.into(),
        }
    }

    /// Creates a new decimal error.
    #[must_use]
    pub fn decimal(message: impl Into<String>) -> Self {
        Self::DecimalError {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_contract_not_found_error() {
        let err = Error::contract_not_found("BTC-20240329-50000-C");
        assert!(err.to_string().contains("BTC-20240329-50000-C"));
    }

    #[test]
    fn test_inventory_limit_exceeded_error() {
        let err = Error::inventory_limit_exceeded("delta", dec!(100000), dec!(150000));
        let msg = err.to_string();
        assert!(msg.contains("delta"));
        assert!(msg.contains("100000"));
        assert!(msg.contains("150000"));
    }

    #[test]
    fn test_adapter_error() {
        let err = Error::adapter("Deribit", "connection timeout");
        let msg = err.to_string();
        assert!(msg.contains("Deribit"));
        assert!(msg.contains("connection timeout"));
    }

    #[test]
    fn test_strike_not_found_error() {
        let err = Error::strike_not_found(50000);
        let msg = err.to_string();
        assert!(msg.contains("50000"));
    }

    #[test]
    fn test_underlying_not_found_error() {
        let err = Error::underlying_not_found("BTC");
        let msg = err.to_string();
        assert!(msg.contains("BTC"));
    }

    #[test]
    fn test_orderbook_error() {
        let err = Error::orderbook("order rejected");
        let msg = err.to_string();
        assert!(msg.contains("order rejected"));
    }

    #[test]
    fn test_pricing_error() {
        let err = Error::pricing("invalid volatility");
        let msg = err.to_string();
        assert!(msg.contains("invalid volatility"));
    }

    #[test]
    fn test_greeks_error() {
        let err = Error::greeks("delta calculation failed");
        let msg = err.to_string();
        assert!(msg.contains("delta calculation failed"));
    }

    #[test]
    fn test_risk_limit_breached_error() {
        let err = Error::risk_limit_breached("max_delta");
        let msg = err.to_string();
        assert!(msg.contains("max_delta"));
    }

    #[test]
    fn test_hedging_error() {
        let err = Error::hedging("hedge order failed");
        let msg = err.to_string();
        assert!(msg.contains("hedge order failed"));
    }

    #[test]
    fn test_quoting_error() {
        let err = Error::quoting("spread too wide");
        let msg = err.to_string();
        assert!(msg.contains("spread too wide"));
    }

    #[test]
    fn test_market_data_error() {
        let err = Error::market_data("stale data");
        let msg = err.to_string();
        assert!(msg.contains("stale data"));
    }

    #[test]
    fn test_configuration_error() {
        let err = Error::configuration("missing API key");
        let msg = err.to_string();
        assert!(msg.contains("missing API key"));
    }

    #[test]
    fn test_validation_error() {
        let err = Error::validation("invalid quantity");
        let msg = err.to_string();
        assert!(msg.contains("invalid quantity"));
    }

    #[test]
    fn test_decimal_error() {
        let err = Error::decimal("overflow");
        let msg = err.to_string();
        assert!(msg.contains("overflow"));
    }

    #[test]
    fn test_no_data_error() {
        let err = Error::no_data("no strikes available");
        let msg = err.to_string();
        assert!(msg.contains("no strikes available"));
    }

    #[test]
    fn test_expiration_not_found_error() {
        let err = Error::expiration_not_found("2024-03-29");
        let msg = err.to_string();
        assert!(msg.contains("2024-03-29"));
    }
}
