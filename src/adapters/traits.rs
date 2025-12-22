//! Exchange adapter traits and types.

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Request to place an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    /// Symbol to trade.
    pub symbol: String,
    /// Order side (buy/sell).
    pub side: OrderSide,
    /// Order type.
    pub order_type: OrderType,
    /// Quantity to trade.
    pub quantity: Decimal,
    /// Limit price (for limit orders).
    pub price: Option<Decimal>,
    /// Client order ID.
    pub client_order_id: Option<String>,
}

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    /// Buy order.
    Buy,
    /// Sell order.
    Sell,
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// Market order.
    Market,
    /// Limit order.
    Limit,
    /// Post-only limit order.
    PostOnly,
}

/// Response from placing an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    /// Exchange order ID.
    pub order_id: String,
    /// Client order ID if provided.
    pub client_order_id: Option<String>,
    /// Order status.
    pub status: OrderStatus,
    /// Filled quantity.
    pub filled_quantity: Decimal,
    /// Average fill price.
    pub avg_price: Option<Decimal>,
    /// Timestamp in milliseconds.
    pub timestamp_ms: u64,
}

/// Order status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    /// Order is pending.
    Pending,
    /// Order is open/active.
    Open,
    /// Order is partially filled.
    PartiallyFilled,
    /// Order is fully filled.
    Filled,
    /// Order was cancelled.
    Cancelled,
    /// Order was rejected.
    Rejected,
}

/// Trait for exchange adapters.
#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    /// Returns the exchange name.
    fn name(&self) -> &str;

    /// Places an order on the exchange.
    async fn place_order(&self, request: OrderRequest) -> Result<OrderResponse, AdapterError>;

    /// Cancels an order by ID.
    async fn cancel_order(&self, order_id: &str) -> Result<bool, AdapterError>;

    /// Gets the status of an order.
    async fn get_order_status(&self, order_id: &str) -> Result<OrderResponse, AdapterError>;

    /// Checks if the adapter is connected.
    fn is_connected(&self) -> bool;
}

/// Error type for adapter operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AdapterError {
    /// Connection error.
    #[error("connection error: {0}")]
    Connection(String),
    /// Authentication error.
    #[error("authentication error: {0}")]
    Authentication(String),
    /// Order rejected.
    #[error("order rejected: {0}")]
    OrderRejected(String),
    /// Rate limited.
    #[error("rate limited")]
    RateLimited,
    /// Unknown error.
    #[error("unknown error: {0}")]
    Unknown(String),
}
