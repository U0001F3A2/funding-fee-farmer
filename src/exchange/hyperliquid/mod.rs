//! Hyperliquid exchange integration.
//!
//! Provides read-only access to Hyperliquid perpetuals market data for:
//! - Funding rate comparison with other venues
//! - Arbitrage opportunity detection
//!
//! # Funding Rate Notes
//!
//! Hyperliquid funding is paid **hourly** at 1/8th of the computed 8-hour rate.
//! This differs from Binance which pays every 8 hours.
//!
//! When comparing rates:
//! - Hyperliquid hourly rate × 8 = equivalent 8-hour rate
//! - Funding cap on HL is 4%/hour (vs tighter caps on CEXs)

mod client;
mod types;

pub use client::HyperliquidClient;
pub use types::*;
