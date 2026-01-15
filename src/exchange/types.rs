//! Type definitions for Binance API responses.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Trading pair symbol information.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInfo {
    pub symbol: String,
    pub pair: String,
    pub contract_type: String,
    pub status: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub price_precision: u8,
    pub quantity_precision: u8,
}

/// Funding rate information for a perpetual contract.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingRate {
    pub symbol: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub funding_rate: Decimal,
    pub funding_time: i64,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub mark_price: Option<Decimal>,
}

/// 24-hour ticker statistics.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticker24h {
    pub symbol: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub price_change: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub price_change_percent: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub last_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub high_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub low_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub volume: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub quote_volume: Decimal,
    pub open_time: i64,
    pub close_time: i64,
}

/// Best bid/ask prices and quantities.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookTicker {
    pub symbol: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub bid_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub bid_qty: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub ask_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub ask_qty: Decimal,
}

/// Account balance information.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBalance {
    pub asset: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub wallet_balance: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unrealized_profit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub margin_balance: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub available_balance: Decimal,
}

/// Futures position information.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub symbol: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub position_amt: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub entry_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub mark_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unrealized_profit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub liquidation_price: Decimal,
    pub leverage: u8,
    pub position_side: PositionSide,
    #[serde(with = "rust_decimal::serde::str")]
    pub notional: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub isolated_margin: Decimal,
    pub margin_type: MarginType,
}

/// Position side (long, short, or both for hedge mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PositionSide {
    Both,
    Long,
    Short,
}

/// Margin type for positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarginType {
    Isolated,
    Cross,
}

/// Order side (buy or sell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderType {
    Limit,
    Market,
    StopMarket,
    TakeProfitMarket,
    TrailingStopMarket,
}

/// Time in force for limit orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TimeInForce {
    Gtc, // Good Till Cancel
    Ioc, // Immediate or Cancel
    Fok, // Fill or Kill
    Gtx, // Post Only (Good Till Crossing)
}

/// Order status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    Expired,
    ExpiredInMatch,
}

/// New order request.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewOrder {
    pub symbol: String,
    pub side: OrderSide,
    pub position_side: Option<PositionSide>,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    pub quantity: Option<Decimal>,
    pub price: Option<Decimal>,
    pub time_in_force: Option<TimeInForce>,
    pub reduce_only: Option<bool>,
    pub new_client_order_id: Option<String>,
}

/// Order response from the exchange.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    pub order_id: i64,
    pub symbol: String,
    pub status: OrderStatus,
    pub client_order_id: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub avg_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub orig_qty: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub executed_qty: Decimal,
    pub side: OrderSide,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    pub time_in_force: Option<TimeInForce>,
    pub update_time: i64,
}

/// Open interest for a symbol.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenInterest {
    pub symbol: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub open_interest: Decimal,
}

/// Qualified trading pair with all required metrics.
#[derive(Debug, Clone)]
pub struct QualifiedPair {
    pub symbol: String,
    pub funding_rate: Decimal,
    pub volume_24h: Decimal,
    pub spread: Decimal,
    pub open_interest: Decimal,
    pub score: Decimal,
}
