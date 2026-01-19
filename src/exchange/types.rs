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

/// Exchange information for futures.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FuturesExchangeInfo {
    pub symbols: Vec<FuturesSymbolInfo>,
}

/// Symbol information for futures.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FuturesSymbolInfo {
    pub symbol: String,
    pub quantity_precision: u8,
    pub price_precision: u8,
    pub contract_type: String,
    pub status: String,
    pub base_asset: String,
    pub quote_asset: String,
}

/// Funding rate information for a perpetual contract.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingRate {
    pub symbol: String,
    #[serde(rename = "lastFundingRate", with = "rust_decimal::serde::str")]
    pub funding_rate: Decimal,
    #[serde(rename = "nextFundingTime")]
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
    /// Spot symbol (e.g., "BTCUSDT" for futures "BTCUSDT")
    pub spot_symbol: String,
    /// Base asset (e.g., "BTC")
    pub base_asset: String,
    pub funding_rate: Decimal,
    pub volume_24h: Decimal,
    pub spread: Decimal,
    pub open_interest: Decimal,
    /// Whether spot margin trading is available for hedging
    pub margin_available: bool,
    /// Hourly borrow rate for the base asset (for shorting)
    pub borrow_rate: Option<Decimal>,
    pub score: Decimal,
}

// ==================== Spot Margin Types ====================

/// Spot symbol information from exchange info.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotSymbolInfo {
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub status: String,
    /// Whether margin trading is permitted
    #[serde(default)]
    pub is_margin_trading_allowed: bool,
}

/// Margin asset information.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginAsset {
    #[serde(rename = "assetName")]
    pub asset: String,
    /// Whether the asset can be borrowed
    #[serde(rename = "isBorrowable")]
    pub borrowable: bool,
    /// Whether the asset can be used as collateral
    #[serde(rename = "isMortgageable")]
    pub collateral: bool,
    /// Margin interest rate (daily) - not always present in API response
    #[serde(default)]
    pub margin_interest_rate: Option<Decimal>,
}

/// Cross margin account details.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossMarginAccount {
    #[serde(with = "rust_decimal::serde::str")]
    pub total_asset_of_btc: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_liability_of_btc: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_net_asset_of_btc: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub margin_level: Decimal,
    pub user_assets: Vec<MarginAccountAsset>,
}

/// Asset balance in margin account.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginAccountAsset {
    pub asset: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub free: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub locked: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub borrowed: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub interest: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub net_asset: Decimal,
}

/// Margin borrow/repay request.
#[derive(Debug, Clone, Serialize)]
pub struct MarginLoanRequest {
    pub asset: String,
    pub amount: Decimal,
}

/// Margin order request (for spot margin trading).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginOrder {
    pub symbol: String,
    pub side: OrderSide,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    pub quantity: Option<Decimal>,
    pub price: Option<Decimal>,
    pub time_in_force: Option<TimeInForce>,
    /// For isolated margin, specify the symbol
    pub is_isolated: Option<bool>,
    /// MARGIN_BUY, AUTO_REPAY, etc.
    pub side_effect_type: Option<SideEffectType>,
}

/// Side effect type for margin orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SideEffectType {
    /// Normal trade
    NoSideEffect,
    /// Borrow to execute the trade
    MarginBuy,
    /// Repay debt with trade proceeds
    AutoRepay,
    /// Auto-borrow and auto-repay
    AutoBorrowRepay,
}

/// Represents a delta-neutral position (futures + spot hedge).
#[derive(Debug, Clone)]
pub struct DeltaNeutralPosition {
    pub symbol: String,
    pub spot_symbol: String,
    pub base_asset: String,
    /// Futures position amount (negative = short)
    pub futures_qty: Decimal,
    pub futures_entry_price: Decimal,
    /// Spot position amount (negative = short via margin)
    pub spot_qty: Decimal,
    pub spot_entry_price: Decimal,
    /// Net delta (should be ~0 for delta-neutral)
    pub net_delta: Decimal,
    /// Borrowed amount if shorting spot
    pub borrowed_amount: Decimal,
    /// Accumulated funding received/paid
    pub funding_pnl: Decimal,
    /// Accumulated borrow interest paid
    pub interest_paid: Decimal,
}

// ==================== Leverage Bracket Types ====================

/// Leverage bracket information for a symbol.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeverageBracket {
    pub symbol: String,
    pub brackets: Vec<NotionalBracket>,
}

/// Notional bracket with maintenance margin rate.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionalBracket {
    /// Bracket number (tier)
    pub bracket: u8,
    /// Initial leverage for this bracket
    pub initial_leverage: u8,
    /// Maximum notional value for this bracket
    #[serde(with = "rust_decimal::serde::str")]
    pub notional_cap: Decimal,
    /// Notional floor for this bracket
    #[serde(with = "rust_decimal::serde::str")]
    pub notional_floor: Decimal,
    /// Maintenance margin rate (e.g., 0.004 = 0.4%)
    #[serde(with = "rust_decimal::serde::str")]
    pub maint_margin_ratio: Decimal,
    /// Cumulative maintenance margin amount
    #[serde(with = "rust_decimal::serde::str")]
    pub cum: Decimal,
}
