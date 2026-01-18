//! Funding payment verification.
//!
//! Compares expected vs actual funding payments to detect:
//! - Funding rate changes between entry and collection
//! - Missed funding payments
//! - Execution timing issues (entered after snapshot)
//! - Exchange calculation discrepancies

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Records a funding payment for verification.
#[derive(Debug, Clone, Serialize)]
pub struct FundingRecord {
    pub symbol: String,
    pub timestamp: DateTime<Utc>,
    pub expected_rate: Decimal,
    pub actual_received: Decimal,
    pub expected_amount: Decimal,
    pub position_value: Decimal,
    pub deviation_pct: Decimal,
}

/// Result of funding verification.
#[derive(Debug, Clone, Serialize)]
pub struct FundingVerificationResult {
    pub symbol: String,
    pub funding_received: Decimal,
    pub funding_expected: Decimal,
    pub deviation_pct: Decimal,
    pub is_anomaly: bool,
    pub anomaly_reason: Option<String>,
}

/// Aggregated funding statistics per symbol.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FundingStats {
    pub symbol: String,
    pub total_received: Decimal,
    pub total_expected: Decimal,
    pub payment_count: u32,
    pub anomaly_count: u32,
    pub cumulative_deviation: Decimal,
    pub average_efficiency: Decimal,
}

/// Verifies funding payments match expectations.
pub struct FundingVerifier {
    /// Maximum allowed deviation before flagging as anomaly
    max_deviation: Decimal,
    /// Expected funding rates per symbol (set at position entry)
    expected_rates: HashMap<String, Decimal>,
    /// History of funding records
    history: Vec<FundingRecord>,
    /// Maximum history size
    max_history: usize,
    /// Per-symbol statistics
    stats: HashMap<String, FundingStats>,
}

impl FundingVerifier {
    /// Create a new funding verifier.
    pub fn new(max_deviation: Decimal) -> Self {
        Self {
            max_deviation,
            expected_rates: HashMap::new(),
            history: Vec::new(),
            max_history: 1000,
            stats: HashMap::new(),
        }
    }

    /// Set expected funding rate for a symbol (at position entry).
    pub fn set_expected_rate(&mut self, symbol: &str, rate: Decimal) {
        self.expected_rates.insert(symbol.to_string(), rate);
        debug!(
            symbol = %symbol,
            rate = %rate,
            "Set expected funding rate"
        );
    }

    /// Clear expected rate (when position is closed).
    pub fn clear_expected_rate(&mut self, symbol: &str) {
        self.expected_rates.remove(symbol);
    }

    /// Verify a funding payment.
    pub fn verify_funding(
        &mut self,
        symbol: &str,
        position_value: Decimal,
        actual_received: Decimal,
    ) -> FundingVerificationResult {
        let expected_rate = self
            .expected_rates
            .get(symbol)
            .copied()
            .unwrap_or(Decimal::ZERO);

        // Expected amount = position_value * funding_rate
        // For shorts, funding rate > 0 means we receive payment
        let expected_amount = position_value * expected_rate.abs();

        // Calculate deviation
        let deviation_pct = if expected_amount != Decimal::ZERO {
            ((actual_received - expected_amount) / expected_amount).abs()
        } else if actual_received != Decimal::ZERO {
            // Expected nothing but received something
            dec!(1)
        } else {
            Decimal::ZERO
        };

        // Determine if this is an anomaly
        let (is_anomaly, anomaly_reason) =
            self.check_anomaly(symbol, expected_amount, actual_received, deviation_pct);

        // Record the funding
        let record = FundingRecord {
            symbol: symbol.to_string(),
            timestamp: Utc::now(),
            expected_rate,
            actual_received,
            expected_amount,
            position_value,
            deviation_pct,
        };

        self.history.push(record);

        // Trim history
        while self.history.len() > self.max_history {
            self.history.remove(0);
        }

        // Update statistics
        self.update_stats(
            symbol,
            actual_received,
            expected_amount,
            deviation_pct,
            is_anomaly,
        );

        if is_anomaly {
            warn!(
                symbol = %symbol,
                actual = %actual_received,
                expected = %expected_amount,
                deviation = %deviation_pct,
                reason = ?anomaly_reason,
                "Funding anomaly detected"
            );
        } else {
            debug!(
                symbol = %symbol,
                actual = %actual_received,
                expected = %expected_amount,
                "Funding verified"
            );
        }

        FundingVerificationResult {
            symbol: symbol.to_string(),
            funding_received: actual_received,
            funding_expected: expected_amount,
            deviation_pct,
            is_anomaly,
            anomaly_reason,
        }
    }

    /// Check if a funding payment is anomalous.
    fn check_anomaly(
        &self,
        _symbol: &str,
        expected: Decimal,
        actual: Decimal,
        deviation: Decimal,
    ) -> (bool, Option<String>) {
        // Case 1: Large deviation from expected
        if deviation > self.max_deviation {
            let reason = format!(
                "Deviation {:.1}% exceeds threshold {:.1}%",
                deviation * dec!(100),
                self.max_deviation * dec!(100)
            );
            return (true, Some(reason));
        }

        // Case 2: Expected positive funding but received negative
        if expected > Decimal::ZERO && actual < Decimal::ZERO {
            return (
                true,
                Some("Expected positive funding but received negative".to_string()),
            );
        }

        // Case 3: Expected funding but received nothing
        if expected > dec!(0.01) && actual.abs() < dec!(0.001) {
            return (
                true,
                Some(format!("Expected ${:.4} but received nothing", expected)),
            );
        }

        // Case 4: Received unexpected large amount
        if expected.abs() < dec!(0.001) && actual.abs() > dec!(1) {
            return (true, Some(format!("Unexpected funding of ${:.4}", actual)));
        }

        (false, None)
    }

    /// Update per-symbol statistics.
    fn update_stats(
        &mut self,
        symbol: &str,
        actual: Decimal,
        expected: Decimal,
        deviation: Decimal,
        is_anomaly: bool,
    ) {
        let stats = self
            .stats
            .entry(symbol.to_string())
            .or_insert(FundingStats {
                symbol: symbol.to_string(),
                ..Default::default()
            });

        stats.total_received += actual;
        stats.total_expected += expected;
        stats.payment_count += 1;
        stats.cumulative_deviation += deviation;

        if is_anomaly {
            stats.anomaly_count += 1;
        }

        // Calculate average efficiency
        if stats.total_expected > Decimal::ZERO {
            stats.average_efficiency = stats.total_received / stats.total_expected;
        }
    }

    /// Get cumulative deviation for a symbol.
    pub fn get_cumulative_deviation(&self, symbol: &str) -> Decimal {
        self.stats
            .get(symbol)
            .map(|s| s.cumulative_deviation)
            .unwrap_or(Decimal::ZERO)
    }

    /// Get funding statistics for a symbol.
    pub fn get_stats(&self, symbol: &str) -> Option<&FundingStats> {
        self.stats.get(symbol)
    }

    /// Get all funding statistics.
    pub fn all_stats(&self) -> &HashMap<String, FundingStats> {
        &self.stats
    }

    /// Get recent funding records.
    pub fn recent_records(&self, count: usize) -> &[FundingRecord] {
        let start = self.history.len().saturating_sub(count);
        &self.history[start..]
    }

    /// Get symbols with poor funding efficiency (< 80%).
    pub fn get_underperforming_symbols(&self) -> Vec<(&str, Decimal)> {
        self.stats
            .iter()
            .filter(|(_, s)| s.average_efficiency < dec!(0.8) && s.payment_count >= 3)
            .map(|(sym, s)| (sym.as_str(), s.average_efficiency))
            .collect()
    }

    /// Clear statistics for a symbol (when position is closed).
    pub fn clear_stats(&mut self, symbol: &str) {
        self.stats.remove(symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_funding_verification() {
        let mut verifier = FundingVerifier::new(dec!(0.20));

        verifier.set_expected_rate("BTCUSDT", dec!(0.0001));

        // Position value $10,000, expected funding = $10,000 * 0.0001 = $1
        let result = verifier.verify_funding("BTCUSDT", dec!(10000), dec!(0.95));

        assert!(!result.is_anomaly); // 5% deviation is within 20% threshold
        assert_eq!(result.funding_expected, dec!(1));
    }

    #[test]
    fn test_anomaly_detection() {
        let mut verifier = FundingVerifier::new(dec!(0.20));

        verifier.set_expected_rate("BTCUSDT", dec!(0.0001));

        // 50% deviation - should be anomaly
        let result = verifier.verify_funding("BTCUSDT", dec!(10000), dec!(0.5));

        assert!(result.is_anomaly);
        assert!(result.anomaly_reason.is_some());
    }

    #[test]
    fn test_stats_accumulation() {
        let mut verifier = FundingVerifier::new(dec!(0.20));

        verifier.set_expected_rate("BTCUSDT", dec!(0.0001));

        // Three payments
        verifier.verify_funding("BTCUSDT", dec!(10000), dec!(1));
        verifier.verify_funding("BTCUSDT", dec!(10000), dec!(1));
        verifier.verify_funding("BTCUSDT", dec!(10000), dec!(1));

        let stats = verifier.get_stats("BTCUSDT").unwrap();
        assert_eq!(stats.payment_count, 3);
        assert_eq!(stats.total_received, dec!(3));
    }

    #[test]
    fn test_zero_expected_funding() {
        let mut verifier = FundingVerifier::new(dec!(0.20));

        // No expected rate set, but received funding
        let result = verifier.verify_funding("ETHUSDT", dec!(5000), dec!(5));

        // Should flag as anomaly since we didn't expect anything
        assert!(result.is_anomaly);
    }
}
