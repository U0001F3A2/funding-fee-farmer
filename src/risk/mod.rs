//! Risk management for funding fee farming.
//!
//! Provides:
//! - Margin monitoring and alerts
//! - Liquidation prevention
//! - Maximum drawdown tracking

mod liquidation;
mod margin;
mod mdd;

pub use liquidation::LiquidationGuard;
pub use margin::MarginMonitor;
pub use mdd::DrawdownTracker;
