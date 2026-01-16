//! Risk management for funding fee farming.
//!
//! Provides comprehensive risk monitoring and prevention:
//! - Margin health monitoring and alerts
//! - Liquidation prevention
//! - Maximum drawdown tracking
//! - Per-position loss detection
//! - Funding payment verification
//! - Malfunction detection

mod liquidation;
mod margin;
mod mdd;
mod position_tracker;
mod funding_verifier;
mod malfunction;
mod orchestrator;

pub use liquidation::{LiquidationGuard, LiquidationAction};
pub use margin::{MarginMonitor, MarginHealth};
pub use mdd::{DrawdownTracker, DrawdownStats};
pub use position_tracker::{
    PositionTracker, TrackedPosition, PositionEntry, PositionAction, PositionLossConfig,
};
pub use funding_verifier::{
    FundingVerifier, FundingRecord, FundingVerificationResult, FundingStats,
};
pub use malfunction::{
    MalfunctionDetector, MalfunctionAlert, MalfunctionType, MalfunctionConfig, AlertSeverity,
};
pub use orchestrator::{
    RiskOrchestrator, RiskOrchestratorConfig, RiskCheckResult, RiskAlert, RiskAlertType,
};
