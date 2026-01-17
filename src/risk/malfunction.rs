//! Trading malfunction detection.
//!
//! Detects operational issues that could indicate system malfunction:
//! - API error rate spikes
//! - Consecutive order failures
//! - Emergency delta drift (hedge breakdown)
//! - Balance/position discrepancies
//! - Rate limiting
//!
//! Provides structured alerts for the log analysis workflow.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use tracing::{debug, error, info, warn};

/// Types of malfunctions that can be detected.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum MalfunctionType {
    /// API error rate exceeded threshold
    ApiErrorSpike {
        error_count: u32,
        window_minutes: u32,
    },
    /// Consecutive order failures for a symbol
    OrderExecutionFailure {
        symbol: String,
        consecutive_failures: u32,
    },
    /// Delta drift exceeded emergency threshold
    DeltaDriftEmergency { symbol: String, drift_pct: Decimal },
    /// Balance mismatch between expected and actual
    BalanceDiscrepancy {
        expected: Decimal,
        actual: Decimal,
        difference: Decimal,
    },
    /// Position quantity mismatch
    PositionMismatch {
        symbol: String,
        expected: Decimal,
        actual: Decimal,
    },
    /// Rate limit hit on API
    RateLimitHit { endpoint: String },
    /// WebSocket connection issues
    WebSocketDisconnect { duration_secs: u64 },
}

/// Severity levels for alerts.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl AlertSeverity {
    /// Get display name.
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertSeverity::Info => "INFO",
            AlertSeverity::Warning => "WARNING",
            AlertSeverity::Error => "ERROR",
            AlertSeverity::Critical => "CRITICAL",
        }
    }
}

/// A malfunction alert.
#[derive(Debug, Clone, Serialize)]
pub struct MalfunctionAlert {
    pub alert_id: String,
    pub timestamp: DateTime<Utc>,
    pub malfunction_type: MalfunctionType,
    pub severity: AlertSeverity,
    pub message: String,
    pub should_halt: bool,
    pub suggested_action: String,
}

impl MalfunctionAlert {
    /// Create a new alert.
    fn new(
        malfunction_type: MalfunctionType,
        severity: AlertSeverity,
        message: String,
        should_halt: bool,
        suggested_action: String,
    ) -> Self {
        let timestamp = Utc::now();
        let alert_id = format!("malfunction-{}-{}", timestamp.timestamp(), rand_suffix());

        Self {
            alert_id,
            timestamp,
            malfunction_type,
            severity,
            message,
            should_halt,
            suggested_action,
        }
    }

    /// Emit alert as structured log.
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

/// Generate a random suffix for alert IDs.
fn rand_suffix() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:08x}", nanos)
}

/// Configuration for malfunction detection.
#[derive(Debug, Clone)]
pub struct MalfunctionConfig {
    /// Maximum API errors per minute before alert
    pub max_errors_per_minute: u32,
    /// Maximum consecutive order failures before alert
    pub max_consecutive_failures: u32,
    /// Delta drift percentage that triggers emergency
    pub emergency_delta_drift: Decimal,
    /// Balance discrepancy threshold (absolute)
    pub balance_discrepancy_threshold: Decimal,
    /// Error window size in minutes
    pub error_window_minutes: u32,
}

impl Default for MalfunctionConfig {
    fn default() -> Self {
        Self {
            max_errors_per_minute: 10,
            max_consecutive_failures: 3,
            emergency_delta_drift: dec!(0.10), // 10%
            balance_discrepancy_threshold: dec!(100),
            error_window_minutes: 5,
        }
    }
}

/// Detects trading malfunctions.
pub struct MalfunctionDetector {
    config: MalfunctionConfig,
    /// Recent errors with timestamps
    error_history: VecDeque<(DateTime<Utc>, String)>,
    /// Consecutive failure count per symbol
    failure_counts: HashMap<String, u32>,
    /// Active alerts (not yet resolved)
    active_alerts: Vec<MalfunctionAlert>,
    /// Last recorded balance for discrepancy detection
    last_balance: Option<Decimal>,
    /// Whether trading should be halted
    halt_trading: bool,
}

impl MalfunctionDetector {
    /// Create a new malfunction detector.
    pub fn new(config: MalfunctionConfig) -> Self {
        Self {
            config,
            error_history: VecDeque::new(),
            failure_counts: HashMap::new(),
            active_alerts: Vec::new(),
            last_balance: None,
            halt_trading: false,
        }
    }

    /// Record an API or execution error.
    pub fn record_error(&mut self, error: &str) -> Option<MalfunctionAlert> {
        let now = Utc::now();

        self.error_history.push_back((now, error.to_string()));

        // Clean old errors outside window
        let window_start = now - Duration::minutes(self.config.error_window_minutes as i64);
        while let Some((timestamp, _)) = self.error_history.front() {
            if *timestamp < window_start {
                self.error_history.pop_front();
            } else {
                break;
            }
        }

        debug!(
            error = %error,
            error_count = self.error_history.len(),
            "Recorded error"
        );

        // Check if error rate exceeds threshold
        let error_count = self.error_history.len() as u32;
        if error_count >= self.config.max_errors_per_minute {
            let alert = MalfunctionAlert::new(
                MalfunctionType::ApiErrorSpike {
                    error_count,
                    window_minutes: self.config.error_window_minutes,
                },
                AlertSeverity::Error,
                format!(
                    "{} errors in {} minutes - API may be unstable",
                    error_count, self.config.error_window_minutes
                ),
                error_count >= self.config.max_errors_per_minute * 2,
                "Reduce API call frequency or investigate connectivity".to_string(),
            );

            self.add_alert(alert.clone());
            return Some(alert);
        }

        None
    }

    /// Record an order execution failure.
    pub fn record_order_failure(&mut self, symbol: &str) -> Option<MalfunctionAlert> {
        let count = self.failure_counts.entry(symbol.to_string()).or_insert(0);
        *count += 1;

        debug!(
            symbol = %symbol,
            consecutive_failures = *count,
            "Recorded order failure"
        );

        if *count >= self.config.max_consecutive_failures {
            let alert = MalfunctionAlert::new(
                MalfunctionType::OrderExecutionFailure {
                    symbol: symbol.to_string(),
                    consecutive_failures: *count,
                },
                AlertSeverity::Error,
                format!(
                    "{} consecutive order failures for {} - execution may be impaired",
                    *count, symbol
                ),
                *count >= self.config.max_consecutive_failures * 2,
                format!("Stop trading {} until issue is resolved", symbol),
            );

            self.add_alert(alert.clone());
            return Some(alert);
        }

        None
    }

    /// Record a successful order (resets failure counter).
    pub fn record_order_success(&mut self, symbol: &str) {
        if let Some(count) = self.failure_counts.get_mut(symbol) {
            if *count > 0 {
                debug!(
                    symbol = %symbol,
                    previous_failures = *count,
                    "Order success - resetting failure counter"
                );
            }
            *count = 0;
        }
    }

    /// Check delta drift and alert if emergency.
    pub fn check_delta_drift(
        &mut self,
        symbol: &str,
        drift_pct: Decimal,
    ) -> Option<MalfunctionAlert> {
        if drift_pct.abs() >= self.config.emergency_delta_drift {
            let alert = MalfunctionAlert::new(
                MalfunctionType::DeltaDriftEmergency {
                    symbol: symbol.to_string(),
                    drift_pct,
                },
                AlertSeverity::Critical,
                format!(
                    "Emergency delta drift {:.2}% on {} - hedge breakdown!",
                    drift_pct * dec!(100),
                    symbol
                ),
                true,
                format!("Immediately rebalance or close {}", symbol),
            );

            self.halt_trading = true;
            self.add_alert(alert.clone());
            return Some(alert);
        }

        None
    }

    /// Check balance discrepancy.
    pub fn check_balance(
        &mut self,
        expected: Decimal,
        actual: Decimal,
    ) -> Option<MalfunctionAlert> {
        let difference = (expected - actual).abs();

        // Update last balance
        self.last_balance = Some(actual);

        if difference >= self.config.balance_discrepancy_threshold {
            let alert = MalfunctionAlert::new(
                MalfunctionType::BalanceDiscrepancy {
                    expected,
                    actual,
                    difference,
                },
                AlertSeverity::Warning,
                format!(
                    "Balance discrepancy: expected ${:.2}, actual ${:.2} (diff: ${:.2})",
                    expected, actual, difference
                ),
                false,
                "Investigate recent trades and fees".to_string(),
            );

            self.add_alert(alert.clone());
            return Some(alert);
        }

        None
    }

    /// Check position quantity mismatch.
    pub fn check_position_mismatch(
        &mut self,
        symbol: &str,
        expected: Decimal,
        actual: Decimal,
    ) -> Option<MalfunctionAlert> {
        let diff_pct = if expected != Decimal::ZERO {
            ((expected - actual) / expected).abs()
        } else if actual != Decimal::ZERO {
            dec!(1) // 100% mismatch
        } else {
            Decimal::ZERO
        };

        // Alert if mismatch > 5%
        if diff_pct > dec!(0.05) {
            let alert = MalfunctionAlert::new(
                MalfunctionType::PositionMismatch {
                    symbol: symbol.to_string(),
                    expected,
                    actual,
                },
                AlertSeverity::Warning,
                format!(
                    "Position mismatch for {}: expected {:.6}, actual {:.6}",
                    symbol, expected, actual
                ),
                false,
                "Verify position state and reconcile".to_string(),
            );

            self.add_alert(alert.clone());
            return Some(alert);
        }

        None
    }

    /// Record rate limit hit.
    pub fn record_rate_limit(&mut self, endpoint: &str) -> MalfunctionAlert {
        let alert = MalfunctionAlert::new(
            MalfunctionType::RateLimitHit {
                endpoint: endpoint.to_string(),
            },
            AlertSeverity::Warning,
            format!("Rate limit hit on endpoint: {}", endpoint),
            false,
            "Implement backoff and reduce call frequency".to_string(),
        );

        self.add_alert(alert.clone());
        alert
    }

    /// Record WebSocket disconnect.
    pub fn record_ws_disconnect(&mut self, duration_secs: u64) -> Option<MalfunctionAlert> {
        // Only alert if disconnect > 30 seconds
        if duration_secs >= 30 {
            let severity = if duration_secs >= 300 {
                AlertSeverity::Error
            } else if duration_secs >= 60 {
                AlertSeverity::Warning
            } else {
                AlertSeverity::Info
            };

            let alert = MalfunctionAlert::new(
                MalfunctionType::WebSocketDisconnect { duration_secs },
                severity,
                format!("WebSocket disconnected for {} seconds", duration_secs),
                duration_secs >= 300,
                "Check network connectivity and reconnect".to_string(),
            );

            self.add_alert(alert.clone());
            return Some(alert);
        }

        None
    }

    /// Add alert to active list.
    fn add_alert(&mut self, alert: MalfunctionAlert) {
        // Check for halt condition
        if alert.should_halt {
            self.halt_trading = true;
        }

        alert.emit();
        self.active_alerts.push(alert);

        // Keep only recent alerts (last 100)
        while self.active_alerts.len() > 100 {
            self.active_alerts.remove(0);
        }
    }

    /// Get all active alerts.
    pub fn get_active_alerts(&self) -> &[MalfunctionAlert] {
        &self.active_alerts
    }

    /// Get alerts by severity.
    pub fn get_alerts_by_severity(&self, min_severity: AlertSeverity) -> Vec<&MalfunctionAlert> {
        self.active_alerts
            .iter()
            .filter(|a| a.severity >= min_severity)
            .collect()
    }

    /// Check if trading should be halted.
    pub fn should_halt_trading(&self) -> bool {
        self.halt_trading
    }

    /// Reset halt flag (after manual review).
    pub fn reset_halt(&mut self) {
        self.halt_trading = false;
        info!("Trading halt reset by operator");
    }

    /// Clear alerts for a symbol (e.g., when position is closed).
    pub fn clear_symbol_alerts(&mut self, symbol: &str) {
        self.failure_counts.remove(symbol);
    }

    /// Get recent error count.
    pub fn recent_error_count(&self) -> usize {
        self.error_history.len()
    }

    /// Get failure count for a symbol.
    pub fn get_failure_count(&self, symbol: &str) -> u32 {
        self.failure_counts.get(symbol).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> MalfunctionConfig {
        MalfunctionConfig {
            max_errors_per_minute: 5,
            max_consecutive_failures: 3,
            emergency_delta_drift: dec!(0.10),
            balance_discrepancy_threshold: dec!(100),
            error_window_minutes: 1,
        }
    }

    #[test]
    fn test_error_recording() {
        let mut detector = MalfunctionDetector::new(test_config());

        // Record errors below threshold
        for _ in 0..4 {
            assert!(detector.record_error("test error").is_none());
        }

        // 5th error should trigger alert
        let alert = detector.record_error("test error");
        assert!(alert.is_some());
        assert!(matches!(
            alert.unwrap().malfunction_type,
            MalfunctionType::ApiErrorSpike { .. }
        ));
    }

    #[test]
    fn test_order_failure_tracking() {
        let mut detector = MalfunctionDetector::new(test_config());

        // Two failures - no alert yet
        assert!(detector.record_order_failure("BTCUSDT").is_none());
        assert!(detector.record_order_failure("BTCUSDT").is_none());

        // Third failure triggers alert
        let alert = detector.record_order_failure("BTCUSDT");
        assert!(alert.is_some());

        // Success resets counter
        detector.record_order_success("BTCUSDT");
        assert_eq!(detector.get_failure_count("BTCUSDT"), 0);
    }

    #[test]
    fn test_delta_drift_emergency() {
        let mut detector = MalfunctionDetector::new(test_config());

        // Small drift - no alert
        assert!(detector.check_delta_drift("BTCUSDT", dec!(0.05)).is_none());

        // Emergency drift - triggers alert and halt
        let alert = detector.check_delta_drift("BTCUSDT", dec!(0.15));
        assert!(alert.is_some());
        assert!(detector.should_halt_trading());
    }

    #[test]
    fn test_balance_discrepancy() {
        let mut detector = MalfunctionDetector::new(test_config());

        // Small discrepancy - no alert
        assert!(detector.check_balance(dec!(1000), dec!(999)).is_none());

        // Large discrepancy - triggers alert
        let alert = detector.check_balance(dec!(1000), dec!(800));
        assert!(alert.is_some());
    }
}
