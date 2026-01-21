//! Binance exchange integration.
//!
//! Provides both REST API and WebSocket connectivity for:
//! - Market data (funding rates, orderbook, trades)
//! - Account operations (orders, positions, balance)
//! - User data streams (order updates, position changes)

mod client;
pub mod mock;
mod types;
mod websocket;

pub use client::BinanceClient;
pub use mock::MockBinanceClient;
pub use types::*;
pub use websocket::BinanceWebSocket;
