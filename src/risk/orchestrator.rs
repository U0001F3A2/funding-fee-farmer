//! Risk orchestration - unified coordination of all risk components.
//!
//! The RiskOrchestrator is the single entry point for risk management,
//! coordinating:
//! - DrawdownTracker (account-level MDD)
//! - MarginMonitor (margin health)
//! - LiquidationGuard (liquidation prevention)
//! - PositionTracker (per-position PnL)
//! - FundingVerifier (funding accuracy)
//! - MalfunctionDetector (operational health)

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

use crate::exchange::Position;

use super::{
    AlertSeverity, DrawdownTracker, FundingVerificationResult, FundingVerifier, LiquidationAction,
    LiquidationGuard, MalfunctionAlert, MalfunctionConfig, MalfunctionDetector, MarginHealth,
    MarginMonitor, PositionAction, PositionEntry, PositionLossConfig, PositionTracker,
    TrackedPosition,
};

/// Unified risk configuration.
#[derive(Debug, Clone)]
pub struct RiskOrchestratorConfig {
    // Drawdown
    pub max_drawdown: Decimal,

    // Margin
    pub min_margin_ratio: Decimal,
    pub max_single_position: Decimal,

    // Position holding rules
    pub min_holding_period_hours: u32,
    pub min_yield_advantage: Decimal,

    // Position loss detection
    pub max_unprofitable_hours: u32,
    pub min_expected_yield: Decimal,
    pub grace_period_hours: u32,
    pub max_funding_deviation: Decimal,

    // Malfunction detection
    pub max_errors_per_minute: u32,
    pub max_consecutive_failures: u32,
    pub emergency_delta_drift: Decimal,

    // Circuit breaker
    pub max_consecutive_risk_cycles: u32,
}

impl Default for RiskOrchestratorConfig {
    fn default() -> Self {
        Self {
            max_drawdown: dec!(0.05),
            min_margin_ratio: dec!(3.0),
            max_single_position: dec!(0.30),
            min_holding_period_hours: 24,
            min_yield_advantage: dec!(0.05),
            max_unprofitable_hours: 48,
            min_expected_yield: dec!(0.10),
            grace_period_hours: 8,
            max_funding_deviation: dec!(0.20),
            max_errors_per_minute: 10,
            max_consecutive_failures: 3,
            emergency_delta_drift: dec!(0.10),
            max_consecutive_risk_cycles: 3,
        }
    }
}

/// Types of risk alerts.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum RiskAlertType {
    /// Margin health warning
    MarginWarning {
        health: MarginHealth,
        action: String,
    },
    /// Liquidation risk detected
    LiquidationRisk { action: LiquidationAction },
    /// Position is unprofitable
    PositionLoss {
        symbol: String,
        reason: String,
        hours: u32,
    },
    /// Funding payment anomaly
    FundingAnomaly { symbol: String, deviation: Decimal },
    /// System malfunction
    Malfunction { malfunction_type: String },
    /// Drawdown exceeded
    DrawdownExceeded { current: Decimal, limit: Decimal },
    /// Delta drift detected
    DeltaDrift { symbol: String, drift_pct: Decimal },
}

/// A unified risk alert.
#[derive(Debug, Clone, Serialize)]
pub struct RiskAlert {
    pub alert_id: String,
    pub timestamp: DateTime<Utc>,
    pub alert_type: RiskAlertType,
    pub severity: AlertSeverity,
    pub symbol: Option<String>,
    pub message: String,
    pub metrics: HashMap<String, Decimal>,
    pub suggested_action: String,
}

impl RiskAlert {
    /// Create a new risk alert.
    pub fn new(
        alert_type: RiskAlertType,
        severity: AlertSeverity,
        symbol: Option<String>,
        message: String,
        suggested_action: String,
    ) -> Self {
        let timestamp = Utc::now();
        let alert_id = format!(
            "risk-{}-{}",
            timestamp.timestamp(),
            timestamp.timestamp_subsec_nanos()
        );

        Self {
            alert_id,
            timestamp,
            alert_type,
            severity,
            symbol,
            message,
            metrics: HashMap::new(),
            suggested_action,
        }
    }

    /// Add a metric to the alert.
    pub fn with_metric(mut self, key: &str, value: Decimal) -> Self {
        self.metrics.insert(key.to_string(), value);
        self
    }

    /// Emit as structured log for workflow parsing.
    pub fn emit(&self) {
        let json = serde_json::to_string(self).unwrap_or_default();

        match self.severity {
            AlertSeverity::Info => info!(target: "risk_alert", "RISK_ALERT: {}", json),
            AlertSeverity::Warning => warn!(target: "risk_alert", "RISK_ALERT: {}", json),
            AlertSeverity::Error => error!(target: "risk_alert", "RISK_ALERT: {}", json),
            AlertSeverity::Critical => error!(target: "risk_alert", "RISK_ALERT: {}", json),
        }
    }
}

/// Result of comprehensive risk check.
#[derive(Debug, Clone)]
pub struct RiskCheckResult {
    pub timestamp: DateTime<Utc>,
    pub should_halt: bool,
    pub should_reduce_exposure: bool,
    pub alerts: Vec<RiskAlert>,
    pub positions_to_close: Vec<String>,
    pub margin_health: MarginHealth,
    pub drawdown_pct: Decimal,
    pub malfunction_detected: bool,
}

impl Default for RiskCheckResult {
    fn default() -> Self {
        Self {
            timestamp: Utc::now(),
            should_halt: false,
            should_reduce_exposure: false,
            alerts: Vec::new(),
            positions_to_close: Vec::new(),
            margin_health: MarginHealth::Green,
            drawdown_pct: Decimal::ZERO,
            malfunction_detected: false,
        }
    }
}

/// Coordinates all risk management components.
pub struct RiskOrchestrator {
    config: RiskOrchestratorConfig,
    drawdown_tracker: DrawdownTracker,
    margin_monitor: MarginMonitor,
    liquidation_guard: LiquidationGuard,
    position_tracker: PositionTracker,
    funding_verifier: FundingVerifier,
    malfunction_detector: MalfunctionDetector,
    consecutive_risk_cycles: u32,
}

impl RiskOrchestrator {
    /// Create a new risk orchestrator.
    pub fn new(config: RiskOrchestratorConfig, initial_equity: Decimal) -> Self {
        let position_loss_config = PositionLossConfig {
            max_unprofitable_hours: config.max_unprofitable_hours,
            min_expected_yield: config.min_expected_yield,
            max_funding_deviation: config.max_funding_deviation,
            grace_period_hours: config.grace_period_hours,
        };

        let malfunction_config = MalfunctionConfig {
            max_errors_per_minute: config.max_errors_per_minute,
            max_consecutive_failures: config.max_consecutive_failures,
            emergency_delta_drift: config.emergency_delta_drift,
            ..Default::default()
        };

        // Create RiskConfig for MarginMonitor
        let risk_config = crate::config::RiskConfig {
            max_drawdown: config.max_drawdown,
            min_margin_ratio: config.min_margin_ratio,
            max_single_position: config.max_single_position,
            min_holding_period_hours: config.min_holding_period_hours,
            min_yield_advantage: config.min_yield_advantage,
            max_unprofitable_hours: config.max_unprofitable_hours,
            min_expected_yield: config.min_expected_yield,
            grace_period_hours: config.grace_period_hours,
            max_funding_deviation: config.max_funding_deviation,
            max_errors_per_minute: config.max_errors_per_minute,
            max_consecutive_failures: config.max_consecutive_failures,
            emergency_delta_drift: config.emergency_delta_drift,
            max_consecutive_risk_cycles: config.max_consecutive_risk_cycles,
        };

        let margin_monitor = MarginMonitor::new(risk_config.clone());
        let liquidation_guard = LiquidationGuard::new(MarginMonitor::new(risk_config));

        Self {
            drawdown_tracker: DrawdownTracker::new(config.max_drawdown, initial_equity),
            margin_monitor,
            liquidation_guard,
            position_tracker: PositionTracker::new(position_loss_config),
            funding_verifier: FundingVerifier::new(config.max_funding_deviation),
            malfunction_detector: MalfunctionDetector::new(malfunction_config),
            consecutive_risk_cycles: 0,
            config,
        }
    }

    /// Perform comprehensive risk check.
    ///
    /// # Arguments
    /// * `positions` - All current positions
    /// * `current_equity` - Current account equity
    /// * `total_margin` - Total margin balance
    /// * `maintenance_rates` - Map of symbol -> maintenance margin rate from API
    pub fn check_all(
        &mut self,
        positions: &[Position],
        current_equity: Decimal,
        total_margin: Decimal,
        maintenance_rates: &HashMap<String, Decimal>,
    ) -> RiskCheckResult {
        let mut result = RiskCheckResult::default();

        // 1. Check drawdown
        let drawdown_exceeded = self.drawdown_tracker.update(current_equity);
        result.drawdown_pct = self.drawdown_tracker.current_drawdown();

        if drawdown_exceeded {
            result.should_halt = true;
            result.alerts.push(
                RiskAlert::new(
                    RiskAlertType::DrawdownExceeded {
                        current: result.drawdown_pct,
                        limit: self.config.max_drawdown,
                    },
                    AlertSeverity::Critical,
                    None,
                    format!(
                        "Maximum drawdown exceeded: {:.2}%",
                        result.drawdown_pct * dec!(100)
                    ),
                    "Halt all trading immediately".to_string(),
                )
                .with_metric("drawdown_pct", result.drawdown_pct)
                .with_metric("max_drawdown", self.config.max_drawdown),
            );
        }

        // 2. Check margin health
        let (worst_health, _position_health) =
            self.margin_monitor
                .check_positions(positions, total_margin, maintenance_rates);
        result.margin_health = worst_health;

        match worst_health {
            MarginHealth::Red => {
                result.should_halt = true;
                result.should_reduce_exposure = true;
                result.alerts.push(RiskAlert::new(
                    RiskAlertType::MarginWarning {
                        health: MarginHealth::Red,
                        action: "Close all positions immediately".to_string(),
                    },
                    AlertSeverity::Critical,
                    None,
                    "Margin health CRITICAL - full position closure required".to_string(),
                    "Close all positions immediately".to_string(),
                ));
            }
            MarginHealth::Orange => {
                result.should_reduce_exposure = true;
                result.alerts.push(RiskAlert::new(
                    RiskAlertType::MarginWarning {
                        health: MarginHealth::Orange,
                        action: "Reduce positions by 50%".to_string(),
                    },
                    AlertSeverity::Error,
                    None,
                    "Margin health WARNING - emergency deleveraging".to_string(),
                    "Reduce positions by 50%".to_string(),
                ));
            }
            MarginHealth::Yellow => {
                result.alerts.push(RiskAlert::new(
                    RiskAlertType::MarginWarning {
                        health: MarginHealth::Yellow,
                        action: "Consider reducing positions by 25%".to_string(),
                    },
                    AlertSeverity::Warning,
                    None,
                    "Margin health CAUTION".to_string(),
                    "Consider reducing positions by 25%".to_string(),
                ));
            }
            MarginHealth::Green => {}
        }

        // 3. Check liquidation risk
        let liquidation_actions =
            self.liquidation_guard
                .evaluate(positions, total_margin, maintenance_rates);
        for action in liquidation_actions {
            let (symbol, severity, message) = match &action {
                LiquidationAction::ClosePosition { symbol } => (
                    symbol.clone(),
                    AlertSeverity::Critical,
                    format!("Position {} requires immediate closure", symbol),
                ),
                LiquidationAction::ReducePosition {
                    symbol,
                    reduction_pct,
                } => (
                    symbol.clone(),
                    AlertSeverity::Error,
                    format!(
                        "Position {} needs {:.0}% reduction",
                        symbol,
                        reduction_pct * dec!(100)
                    ),
                ),
                LiquidationAction::AddMargin { symbol, amount } => (
                    symbol.clone(),
                    AlertSeverity::Warning,
                    format!("Position {} needs ${:.2} additional margin", symbol, amount),
                ),
                LiquidationAction::None => continue,
            };

            result.alerts.push(RiskAlert::new(
                RiskAlertType::LiquidationRisk {
                    action: action.clone(),
                },
                severity,
                Some(symbol.clone()),
                message,
                format!("{:?}", action),
            ));

            // Add to positions to close if critical
            if matches!(action, LiquidationAction::ClosePosition { .. }) {
                result.positions_to_close.push(symbol);
            }
        }

        // 4. Check position health
        for symbol in self
            .position_tracker
            .all_positions()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            match self.position_tracker.evaluate_position(&symbol) {
                PositionAction::ForceExit { reason } => {
                    result.positions_to_close.push(symbol.clone());
                    result.alerts.push(RiskAlert::new(
                        RiskAlertType::PositionLoss {
                            symbol: symbol.clone(),
                            reason: reason.clone(),
                            hours: 48, // Would get from tracker
                        },
                        AlertSeverity::Error,
                        Some(symbol.clone()),
                        reason,
                        format!("Close position {}", symbol),
                    ));
                }
                PositionAction::ConsiderExit {
                    reason,
                    hours_unprofitable,
                } => {
                    result.alerts.push(RiskAlert::new(
                        RiskAlertType::PositionLoss {
                            symbol: symbol.clone(),
                            reason: reason.clone(),
                            hours: hours_unprofitable,
                        },
                        AlertSeverity::Warning,
                        Some(symbol.clone()),
                        reason,
                        format!("Monitor {} closely", symbol),
                    ));
                }
                PositionAction::MonitorClosely { reason } => {
                    debug!(symbol = %symbol, reason = %reason, "Position requires monitoring");
                }
                PositionAction::Hold => {}
            }
        }

        // 5. Check for malfunctions
        if self.malfunction_detector.should_halt_trading() {
            result.should_halt = true;
            result.malfunction_detected = true;
        }

        // Emit all alerts
        for alert in &result.alerts {
            alert.emit();
        }

        // Circuit breaker: track consecutive cycles with ERROR/CRITICAL alerts
        let has_critical_alerts = result.alerts.iter().any(|alert| {
            matches!(
                alert.severity,
                AlertSeverity::Error | AlertSeverity::Critical
            )
        });

        if has_critical_alerts {
            self.consecutive_risk_cycles += 1;
            debug!(
                "Risk cycle with critical alerts (consecutive: {}/{})",
                self.consecutive_risk_cycles, self.config.max_consecutive_risk_cycles
            );

            if self.consecutive_risk_cycles >= self.config.max_consecutive_risk_cycles {
                result.should_halt = true;
                error!(
                    "🚨 [CIRCUIT BREAKER] Trading halted after {} consecutive cycles with ERROR/CRITICAL alerts",
                    self.consecutive_risk_cycles
                );

                result.alerts.push(
                    RiskAlert::new(
                        RiskAlertType::Malfunction {
                            malfunction_type: "CircuitBreakerTripped".to_string(),
                        },
                        AlertSeverity::Critical,
                        None,
                        format!(
                            "Circuit breaker triggered: {} consecutive risk cycles with critical alerts",
                            self.consecutive_risk_cycles
                        ),
                        "Halt all trading immediately - manual intervention required".to_string(),
                    )
                    .with_metric("consecutive_risk_cycles", Decimal::from(self.consecutive_risk_cycles)),
                );
            }
        } else {
            if self.consecutive_risk_cycles > 0 {
                debug!(
                    "Risk cycle completed without critical alerts, resetting counter from {}",
                    self.consecutive_risk_cycles
                );
            }
            self.consecutive_risk_cycles = 0;
        }

        result
    }

    /// Check for malfunctions only (lighter check for each loop iteration).
    /// Returns true if trading should be halted due to malfunctions.
    pub fn check_malfunctions(&self) -> bool {
        self.malfunction_detector.should_halt_trading()
    }

    /// Get active alerts.
    pub fn get_active_alerts(&self) -> &[MalfunctionAlert] {
        self.malfunction_detector.get_active_alerts()
    }

    /// Record an API/execution error.
    pub fn record_error(&mut self, error: &str) -> Option<MalfunctionAlert> {
        self.malfunction_detector.record_error(error)
    }

    /// Record order failure for a symbol.
    pub fn record_order_failure(&mut self, symbol: &str) -> Option<MalfunctionAlert> {
        self.malfunction_detector.record_order_failure(symbol)
    }

    /// Record order success for a symbol.
    pub fn record_order_success(&mut self, symbol: &str) {
        self.malfunction_detector.record_order_success(symbol)
    }

    /// Check delta drift.
    pub fn check_delta_drift(
        &mut self,
        symbol: &str,
        drift_pct: Decimal,
    ) -> Option<MalfunctionAlert> {
        self.malfunction_detector
            .check_delta_drift(symbol, drift_pct)
    }

    /// Open a tracked position (entry contains symbol).
    pub fn open_position(&mut self, entry: PositionEntry) {
        let symbol = entry.symbol.clone();
        let expected_rate = entry.expected_funding_rate;
        self.position_tracker.open_position(&symbol, entry);
        self.funding_verifier
            .set_expected_rate(&symbol, expected_rate);
    }

    /// Record funding payment for a symbol.
    pub fn record_funding(&mut self, symbol: &str, amount: Decimal) {
        if let Some(pos) = self.position_tracker.get_position(symbol) {
            let expected = pos.expected_funding_rate * pos.position_value;
            self.position_tracker
                .record_funding(symbol, amount, expected);
        }
    }

    /// Verify funding payment against expected.
    pub fn verify_funding(
        &mut self,
        symbol: &str,
        actual_funding: Decimal,
    ) -> FundingVerificationResult {
        if let Some(pos) = self.position_tracker.get_position(symbol) {
            self.funding_verifier
                .verify_funding(symbol, pos.position_value, actual_funding)
        } else {
            FundingVerificationResult {
                symbol: symbol.to_string(),
                funding_received: actual_funding,
                funding_expected: Decimal::ZERO,
                deviation_pct: Decimal::ZERO,
                is_anomaly: false,
                anomaly_reason: None,
            }
        }
    }

    /// Record interest payment.
    pub fn record_interest(&mut self, symbol: &str, amount: Decimal) {
        self.position_tracker.record_interest(symbol, amount);
    }

    /// Update position PnL.
    pub fn update_position_pnl(&mut self, symbol: &str, unrealized: Decimal) {
        self.position_tracker.update_pnl(symbol, unrealized);
    }

    /// Evaluate a position.
    pub fn evaluate_position(&mut self, symbol: &str) -> PositionAction {
        self.position_tracker.evaluate_position(symbol)
    }

    /// Close a tracked position.
    pub fn close_position(&mut self, symbol: &str) -> Option<TrackedPosition> {
        self.funding_verifier.clear_expected_rate(symbol);
        self.funding_verifier.clear_stats(symbol);
        self.malfunction_detector.clear_symbol_alerts(symbol);
        self.position_tracker.close_position(symbol)
    }

    /// Get positions requiring forced closure.
    pub fn get_positions_to_close(&mut self) -> Vec<String> {
        self.position_tracker.get_positions_to_close()
    }

    /// Get tracked position.
    pub fn get_tracked_position(&self, symbol: &str) -> Option<&TrackedPosition> {
        self.position_tracker.get_position(symbol)
    }

    /// Get all tracked positions.
    pub fn get_all_tracked_positions(&self) -> Vec<&TrackedPosition> {
        self.position_tracker.all_positions().values().collect()
    }

    /// Get drawdown statistics.
    pub fn get_drawdown_stats(&self) -> super::mdd::DrawdownStats {
        self.drawdown_tracker.statistics()
    }

    /// Check if trading should halt.
    pub fn should_halt(&self) -> bool {
        self.malfunction_detector.should_halt_trading()
            || self.drawdown_tracker.current_drawdown() >= self.config.max_drawdown
    }

    /// Reset halt condition.
    pub fn reset_halt(&mut self) {
        self.malfunction_detector.reset_halt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orchestrator_creation() {
        let config = RiskOrchestratorConfig::default();
        let orchestrator = RiskOrchestrator::new(config, dec!(10000));

        assert!(!orchestrator.should_halt());
    }

    #[test]
    fn test_position_lifecycle() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };

        orchestrator.open_position(entry);
        assert!(orchestrator.get_tracked_position("BTCUSDT").is_some());

        orchestrator.close_position("BTCUSDT");
        assert!(orchestrator.get_tracked_position("BTCUSDT").is_none());
    }

    #[test]
    fn test_error_recording() {
        let config = RiskOrchestratorConfig {
            max_errors_per_minute: 3,
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Record errors below threshold
        assert!(orchestrator.record_error("test").is_none());
        assert!(orchestrator.record_error("test").is_none());

        // Third error should trigger alert
        assert!(orchestrator.record_error("test").is_some());
    }

    #[test]
    fn test_circuit_breaker_triggers_after_consecutive_risk_cycles() {
        let config = RiskOrchestratorConfig {
            max_consecutive_risk_cycles: 3,
            min_margin_ratio: dec!(3.0),
            max_drawdown: dec!(0.05),
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Create a position that triggers ERROR level alerts (ORANGE margin health)
        // but not CRITICAL (RED margin health which would halt immediately).
        // We want margin ratio between 1.5x and 2.0x to get ORANGE health (ERROR alert).
        // With margin_balance of 400 and notional of 50000 at 5x leverage:
        // margin ratio = 400 / (50000 * 0.004) = 400 / 200 = 2.0 (ORANGE - ERROR level)
        let position = crate::exchange::Position {
            symbol: "BTCUSDT".to_string(),
            position_amt: dec!(1.0),
            entry_price: dec!(50000),
            unrealized_profit: dec!(-100), // Small unrealized loss
            leverage: 5,
            notional: dec!(50000),
            isolated_margin: dec!(0),
            mark_price: dec!(50000),
            liquidation_price: dec!(0),
            position_side: crate::exchange::PositionSide::Both,
            margin_type: crate::exchange::MarginType::Cross,
        };

        // Use margin balance that gives ~2x margin ratio (ORANGE health = ERROR severity)
        let margin_balance = dec!(400);
        let equity = dec!(9900);
        let maintenance_rates = std::collections::HashMap::new();

        // First cycle with ERROR alert - should not halt
        let result1 = orchestrator.check_all(
            &[position.clone()],
            equity,
            margin_balance,
            &maintenance_rates,
        );
        assert!(!result1.should_halt);
        assert!(!result1.alerts.is_empty());

        // Second cycle with ERROR alert - should not halt
        let result2 = orchestrator.check_all(
            &[position.clone()],
            equity,
            margin_balance,
            &maintenance_rates,
        );
        assert!(!result2.should_halt);

        // Third cycle with ERROR alert - SHOULD HALT (circuit breaker triggered)
        let result3 = orchestrator.check_all(
            &[position.clone()],
            equity,
            margin_balance,
            &maintenance_rates,
        );
        assert!(result3.should_halt);

        // Verify circuit breaker alert was added
        let has_circuit_breaker_alert = result3.alerts.iter().any(|alert| {
            matches!(&alert.alert_type, RiskAlertType::Malfunction { malfunction_type }
                if malfunction_type == "CircuitBreakerTripped")
        });
        assert!(has_circuit_breaker_alert);
    }

    #[test]
    fn test_circuit_breaker_resets_when_no_critical_alerts() {
        let config = RiskOrchestratorConfig {
            max_consecutive_risk_cycles: 3,
            min_margin_ratio: dec!(3.0),
            max_drawdown: dec!(0.05),
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        let error_position = crate::exchange::Position {
            symbol: "BTCUSDT".to_string(),
            position_amt: dec!(1.0),
            entry_price: dec!(50000),
            unrealized_profit: dec!(-100),
            leverage: 5,
            notional: dec!(50000),
            isolated_margin: dec!(0),
            mark_price: dec!(50000),
            liquidation_price: dec!(0),
            position_side: crate::exchange::PositionSide::Both,
            margin_type: crate::exchange::MarginType::Cross,
        };

        let margin_balance = dec!(400);
        let equity = dec!(9900);
        let maintenance_rates = std::collections::HashMap::new();

        // Two cycles with ERROR alerts
        orchestrator.check_all(
            &[error_position.clone()],
            equity,
            margin_balance,
            &maintenance_rates,
        );
        orchestrator.check_all(
            &[error_position.clone()],
            equity,
            margin_balance,
            &maintenance_rates,
        );

        // One cycle with no positions (no critical alerts) - should reset counter
        let result_clean =
            orchestrator.check_all(&[], dec!(10000), dec!(10000), &maintenance_rates);
        assert!(!result_clean.should_halt);

        // Now even after 2 more cycles with alerts, should not halt (counter was reset)
        orchestrator.check_all(
            &[error_position.clone()],
            equity,
            margin_balance,
            &maintenance_rates,
        );
        let result = orchestrator.check_all(
            &[error_position.clone()],
            equity,
            margin_balance,
            &maintenance_rates,
        );
        assert!(!result.should_halt);
    }

    // =========================================================================
    // RiskOrchestratorConfig Tests
    // =========================================================================

    #[test]
    fn test_config_default_values() {
        let config = RiskOrchestratorConfig::default();

        assert_eq!(config.max_drawdown, dec!(0.05));
        assert_eq!(config.min_margin_ratio, dec!(3.0));
        assert_eq!(config.max_single_position, dec!(0.30));
        assert_eq!(config.max_unprofitable_hours, 48);
        assert_eq!(config.min_expected_yield, dec!(0.10));
        assert_eq!(config.grace_period_hours, 8);
        assert_eq!(config.max_funding_deviation, dec!(0.20));
        assert_eq!(config.max_errors_per_minute, 10);
        assert_eq!(config.max_consecutive_failures, 3);
        assert_eq!(config.emergency_delta_drift, dec!(0.10));
        assert_eq!(config.max_consecutive_risk_cycles, 3);
    }

    // =========================================================================
    // RiskAlert Tests
    // =========================================================================

    #[test]
    fn test_risk_alert_creation() {
        let alert = RiskAlert::new(
            RiskAlertType::DrawdownExceeded {
                current: dec!(0.06),
                limit: dec!(0.05),
            },
            AlertSeverity::Critical,
            None,
            "Drawdown exceeded".to_string(),
            "Halt trading".to_string(),
        );

        assert!(alert.alert_id.starts_with("risk-"));
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert!(alert.symbol.is_none());
        assert_eq!(alert.message, "Drawdown exceeded");
        assert_eq!(alert.suggested_action, "Halt trading");
    }

    #[test]
    fn test_risk_alert_with_metric() {
        let alert = RiskAlert::new(
            RiskAlertType::DrawdownExceeded {
                current: dec!(0.06),
                limit: dec!(0.05),
            },
            AlertSeverity::Critical,
            None,
            "Test".to_string(),
            "Test action".to_string(),
        )
        .with_metric("drawdown_pct", dec!(0.06))
        .with_metric("max_drawdown", dec!(0.05));

        assert_eq!(alert.metrics.get("drawdown_pct"), Some(&dec!(0.06)));
        assert_eq!(alert.metrics.get("max_drawdown"), Some(&dec!(0.05)));
    }

    #[test]
    fn test_risk_alert_with_symbol() {
        let alert = RiskAlert::new(
            RiskAlertType::LiquidationRisk {
                action: LiquidationAction::ClosePosition {
                    symbol: "BTCUSDT".to_string(),
                },
            },
            AlertSeverity::Critical,
            Some("BTCUSDT".to_string()),
            "Close position".to_string(),
            "Close immediately".to_string(),
        );

        assert_eq!(alert.symbol, Some("BTCUSDT".to_string()));
    }

    // =========================================================================
    // RiskCheckResult Tests
    // =========================================================================

    #[test]
    fn test_risk_check_result_default() {
        let result = RiskCheckResult::default();

        assert!(!result.should_halt);
        assert!(!result.should_reduce_exposure);
        assert!(result.alerts.is_empty());
        assert!(result.positions_to_close.is_empty());
        assert_eq!(result.margin_health, MarginHealth::Green);
        assert_eq!(result.drawdown_pct, Decimal::ZERO);
        assert!(!result.malfunction_detected);
    }

    // =========================================================================
    // RiskAlertType Tests
    // =========================================================================

    #[test]
    fn test_risk_alert_type_equality() {
        let alert1 = RiskAlertType::DrawdownExceeded {
            current: dec!(0.06),
            limit: dec!(0.05),
        };
        let alert2 = RiskAlertType::DrawdownExceeded {
            current: dec!(0.06),
            limit: dec!(0.05),
        };
        let alert3 = RiskAlertType::DrawdownExceeded {
            current: dec!(0.07),
            limit: dec!(0.05),
        };

        assert_eq!(alert1, alert2);
        assert_ne!(alert1, alert3);
    }

    #[test]
    fn test_risk_alert_type_margin_warning() {
        let alert = RiskAlertType::MarginWarning {
            health: MarginHealth::Yellow,
            action: "Reduce positions".to_string(),
        };

        match alert {
            RiskAlertType::MarginWarning { health, action } => {
                assert_eq!(health, MarginHealth::Yellow);
                assert_eq!(action, "Reduce positions");
            }
            _ => panic!("Expected MarginWarning"),
        }
    }

    #[test]
    fn test_risk_alert_type_position_loss() {
        let alert = RiskAlertType::PositionLoss {
            symbol: "BTCUSDT".to_string(),
            reason: "Unprofitable for 48h".to_string(),
            hours: 48,
        };

        match alert {
            RiskAlertType::PositionLoss {
                symbol,
                reason,
                hours,
            } => {
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(hours, 48);
                assert!(reason.contains("48h"));
            }
            _ => panic!("Expected PositionLoss"),
        }
    }

    #[test]
    fn test_risk_alert_type_funding_anomaly() {
        let alert = RiskAlertType::FundingAnomaly {
            symbol: "ETHUSDT".to_string(),
            deviation: dec!(0.25),
        };

        match alert {
            RiskAlertType::FundingAnomaly { symbol, deviation } => {
                assert_eq!(symbol, "ETHUSDT");
                assert_eq!(deviation, dec!(0.25));
            }
            _ => panic!("Expected FundingAnomaly"),
        }
    }

    #[test]
    fn test_risk_alert_type_delta_drift() {
        let alert = RiskAlertType::DeltaDrift {
            symbol: "BTCUSDT".to_string(),
            drift_pct: dec!(0.15),
        };

        match alert {
            RiskAlertType::DeltaDrift { symbol, drift_pct } => {
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(drift_pct, dec!(0.15));
            }
            _ => panic!("Expected DeltaDrift"),
        }
    }

    // =========================================================================
    // Drawdown Check Tests
    // =========================================================================

    #[test]
    fn test_check_all_drawdown_exceeded() {
        let config = RiskOrchestratorConfig {
            max_drawdown: dec!(0.05),
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Drop equity by 6% (exceeds 5% max)
        let result = orchestrator.check_all(
            &[],
            dec!(9400), // 6% drawdown
            dec!(10000),
            &HashMap::new(),
        );

        assert!(result.should_halt);
        assert!(result.drawdown_pct >= dec!(0.05));

        let has_drawdown_alert = result
            .alerts
            .iter()
            .any(|a| matches!(&a.alert_type, RiskAlertType::DrawdownExceeded { .. }));
        assert!(has_drawdown_alert);
    }

    #[test]
    fn test_check_all_drawdown_safe() {
        let config = RiskOrchestratorConfig {
            max_drawdown: dec!(0.10),
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Only 3% drawdown - safe
        let result = orchestrator.check_all(&[], dec!(9700), dec!(10000), &HashMap::new());

        assert!(!result.should_halt);
        assert_eq!(result.drawdown_pct, dec!(0.03));
    }

    // =========================================================================
    // Margin Health Check Tests
    // =========================================================================

    #[test]
    fn test_check_all_margin_red_halts() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Position with very low margin = RED health
        let position = crate::exchange::Position {
            symbol: "BTCUSDT".to_string(),
            position_amt: dec!(1.0),
            entry_price: dec!(50000),
            unrealized_profit: Decimal::ZERO,
            leverage: 10,
            notional: dec!(50000),
            isolated_margin: dec!(50), // Very low margin
            mark_price: dec!(50000),
            liquidation_price: dec!(45000),
            position_side: crate::exchange::PositionSide::Both,
            margin_type: crate::exchange::MarginType::Isolated,
        };

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let result = orchestrator.check_all(&[position], dec!(10000), dec!(100000), &rates);

        // Margin ratio = 50 / (50000 * 0.004) = 50 / 200 = 0.25 -> RED
        assert!(result.should_halt);
        assert!(result.should_reduce_exposure);
        assert_eq!(result.margin_health, MarginHealth::Red);
    }

    #[test]
    fn test_check_all_margin_orange_reduces_exposure() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Position with moderate margin = ORANGE health
        let position = crate::exchange::Position {
            symbol: "BTCUSDT".to_string(),
            position_amt: dec!(1.0),
            entry_price: dec!(50000),
            unrealized_profit: Decimal::ZERO,
            leverage: 10,
            notional: dec!(10000),
            isolated_margin: dec!(100), // ratio = 100 / 40 = 2.5 -> ORANGE
            mark_price: dec!(50000),
            liquidation_price: dec!(45000),
            position_side: crate::exchange::PositionSide::Both,
            margin_type: crate::exchange::MarginType::Isolated,
        };

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let result = orchestrator.check_all(&[position], dec!(10000), dec!(100000), &rates);

        assert!(!result.should_halt);
        assert!(result.should_reduce_exposure);
        assert_eq!(result.margin_health, MarginHealth::Orange);
    }

    // =========================================================================
    // Order Recording Tests
    // =========================================================================

    #[test]
    fn test_record_order_success_and_failure() {
        let config = RiskOrchestratorConfig {
            max_consecutive_failures: 3,
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Success should not trigger alert
        orchestrator.record_order_success("BTCUSDT");
        assert!(!orchestrator.check_malfunctions());

        // First failures should not trigger
        assert!(orchestrator.record_order_failure("BTCUSDT").is_none());
        assert!(orchestrator.record_order_failure("BTCUSDT").is_none());

        // Third failure should trigger
        assert!(orchestrator.record_order_failure("BTCUSDT").is_some());
    }

    // =========================================================================
    // Delta Drift Tests
    // =========================================================================

    #[test]
    fn test_delta_drift_check() {
        let config = RiskOrchestratorConfig {
            emergency_delta_drift: dec!(0.10), // 10%
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Small drift - no alert
        let alert1 = orchestrator.check_delta_drift("BTCUSDT", dec!(0.05));
        assert!(alert1.is_none());

        // Large drift - should alert
        let alert2 = orchestrator.check_delta_drift("BTCUSDT", dec!(0.15));
        assert!(alert2.is_some());
    }

    // =========================================================================
    // Funding Verification Tests
    // =========================================================================

    #[test]
    fn test_verify_funding_no_position() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // No position tracked - should return safe default
        let result = orchestrator.verify_funding("BTCUSDT", dec!(5));

        assert_eq!(result.symbol, "BTCUSDT");
        assert_eq!(result.funding_received, dec!(5));
        assert!(!result.is_anomaly);
    }

    #[test]
    fn test_record_funding() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Open position
        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };
        orchestrator.open_position(entry);

        // Record funding
        orchestrator.record_funding("BTCUSDT", dec!(0.5));

        // Position should be updated
        let pos = orchestrator.get_tracked_position("BTCUSDT").unwrap();
        assert_eq!(pos.total_funding_received, dec!(0.5));
    }

    // =========================================================================
    // Should Halt Tests
    // =========================================================================

    #[test]
    fn test_should_halt_from_drawdown() {
        let config = RiskOrchestratorConfig {
            max_drawdown: dec!(0.05),
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Initially should not halt
        assert!(!orchestrator.should_halt());

        // Trigger drawdown
        orchestrator.check_all(&[], dec!(9400), dec!(10000), &HashMap::new());

        // Now should halt
        assert!(orchestrator.should_halt());
    }

    // =========================================================================
    // Reset Halt Tests
    // =========================================================================

    #[test]
    fn test_reset_halt() {
        let config = RiskOrchestratorConfig {
            max_errors_per_minute: 1,
            ..Default::default()
        };
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Trigger halt via malfunction
        orchestrator.record_error("error1");
        orchestrator.record_error("error2");
        assert!(orchestrator.check_malfunctions());

        // Reset halt
        orchestrator.reset_halt();
        assert!(!orchestrator.check_malfunctions());
    }

    // =========================================================================
    // Interest Recording Tests
    // =========================================================================

    #[test]
    fn test_record_interest() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Open position
        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };
        orchestrator.open_position(entry);

        // Record interest
        orchestrator.record_interest("BTCUSDT", dec!(0.5));

        // Position should be updated
        let pos = orchestrator.get_tracked_position("BTCUSDT").unwrap();
        assert_eq!(pos.interest_paid, dec!(0.5));
    }

    // =========================================================================
    // PnL Update Tests
    // =========================================================================

    #[test]
    fn test_update_position_pnl() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Open position
        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };
        orchestrator.open_position(entry);

        // Update PnL
        orchestrator.update_position_pnl("BTCUSDT", dec!(100));

        // Position should be updated
        let pos = orchestrator.get_tracked_position("BTCUSDT").unwrap();
        assert_eq!(pos.unrealized_pnl, dec!(100));
    }

    // =========================================================================
    // Get All Tracked Positions Tests
    // =========================================================================

    #[test]
    fn test_get_all_tracked_positions() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Open multiple positions
        orchestrator.open_position(PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        });

        orchestrator.open_position(PositionEntry {
            symbol: "ETHUSDT".to_string(),
            entry_price: dec!(3000),
            quantity: dec!(1.0),
            expected_funding_rate: dec!(0.00015),
            entry_fees: dec!(1),
            position_value: dec!(3000),
        });

        let positions = orchestrator.get_all_tracked_positions();
        assert_eq!(positions.len(), 2);
    }

    // =========================================================================
    // Drawdown Stats Tests
    // =========================================================================

    #[test]
    fn test_get_drawdown_stats() {
        let config = RiskOrchestratorConfig::default();
        let mut orchestrator = RiskOrchestrator::new(config, dec!(10000));

        // Create some history
        orchestrator.check_all(&[], dec!(11000), dec!(10000), &HashMap::new());
        orchestrator.check_all(&[], dec!(10500), dec!(10000), &HashMap::new());

        let stats = orchestrator.get_drawdown_stats();

        assert_eq!(stats.peak_equity, dec!(11000));
        assert_eq!(stats.current_equity, dec!(10500));
    }
}
