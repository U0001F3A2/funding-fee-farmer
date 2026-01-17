//! Risk management for funding fee farming.
//!
//! Provides comprehensive risk monitoring and prevention:
//! - Margin health monitoring and alerts
//! - Liquidation prevention
//! - Maximum drawdown tracking
//! - Per-position loss detection
//! - Funding payment verification
//! - Malfunction detection

mod funding_verifier;
mod liquidation;
mod malfunction;
mod margin;
mod mdd;
mod orchestrator;
mod position_tracker;

pub use funding_verifier::{
    FundingRecord, FundingStats, FundingVerificationResult, FundingVerifier,
};
pub use liquidation::{LiquidationAction, LiquidationGuard};
pub use malfunction::{
    AlertSeverity, MalfunctionAlert, MalfunctionConfig, MalfunctionDetector, MalfunctionType,
};
pub use margin::{MarginHealth, MarginMonitor};
pub use mdd::{DrawdownStats, DrawdownTracker};
pub use orchestrator::{
    RiskAlert, RiskAlertType, RiskCheckResult, RiskOrchestrator, RiskOrchestratorConfig,
};
pub use position_tracker::{
    PositionAction, PositionEntry, PositionLossConfig, PositionTracker, TrackedPosition,
};
