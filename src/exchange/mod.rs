//! Exchange integrations for funding fee farming.
//!
//! ## Binance
//! Provides both REST API and WebSocket connectivity for:
//! - Market data (funding rates, orderbook, trades)
//! - Account operations (orders, positions, balance)
//! - User data streams (order updates, position changes)
//!
//! ## Hyperliquid
//! Read-only access to perpetuals market data for:
//! - Funding rate comparison
//! - Arbitrage opportunity detection

mod client;
pub mod hyperliquid;
pub mod mock;
mod types;
mod websocket;

pub use client::BinanceClient;
pub use hyperliquid::HyperliquidClient;
pub use mock::MockBinanceClient;
pub use types::*;
pub use websocket::BinanceWebSocket;
