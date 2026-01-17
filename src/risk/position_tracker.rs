//! Per-position profitability tracking and loss detection.
//!
//! Tracks each position's lifecycle including:
//! - Entry time and expected funding rate
//! - Accumulated funding payments vs costs
//! - Net PnL calculation
//! - Loss detection and exit recommendations

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Serialize;
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Configuration for position loss detection.
#[derive(Debug, Clone)]
pub struct PositionLossConfig {
    /// Maximum hours to keep an unprofitable position
    pub max_unprofitable_hours: u32,
    /// Minimum expected annualized yield (e.g., 0.10 = 10% APY)
    pub min_expected_yield: Decimal,
    /// Maximum allowed deviation from expected funding (as ratio, e.g., 0.20 = 20%)
    pub max_funding_deviation: Decimal,
    /// Hours before first profit check (allow positions to settle)
    pub grace_period_hours: u32,
}

impl Default for PositionLossConfig {
    fn default() -> Self {
        Self {
            max_unprofitable_hours: 48,
            min_expected_yield: dec!(0.10),
            max_funding_deviation: dec!(0.20),
            grace_period_hours: 8,
        }
    }
}

/// Entry information for opening a position.
#[derive(Debug, Clone)]
pub struct PositionEntry {
    pub symbol: String,
    pub entry_price: Decimal,
    pub quantity: Decimal,
    pub expected_funding_rate: Decimal,
    pub entry_fees: Decimal,
    pub position_value: Decimal,
}

/// Tracks a position's lifecycle and profitability.
#[derive(Debug, Clone, Serialize)]
pub struct TrackedPosition {
    pub symbol: String,
    pub opened_at: DateTime<Utc>,
    pub entry_price: Decimal,
    pub quantity: Decimal,
    pub position_value: Decimal,

    // Funding tracking
    pub expected_funding_rate: Decimal,
    pub funding_collections: u32,
    pub total_funding_received: Decimal,
    pub expected_total_funding: Decimal,

    // Cost tracking
    pub entry_fees: Decimal,
    pub interest_paid: Decimal,
    pub rebalance_fees: Decimal,

    // PnL tracking
    pub unrealized_pnl: Decimal,

    // Computed metrics (updated on each evaluation)
    #[serde(skip)]
    hours_open: f64,
    #[serde(skip)]
    hours_unprofitable: u32,
}

impl TrackedPosition {
    /// Create a new tracked position.
    pub fn new(symbol: String, entry: PositionEntry) -> Self {
        Self {
            symbol,
            opened_at: Utc::now(),
            entry_price: entry.entry_price,
            quantity: entry.quantity,
            position_value: entry.position_value,
            expected_funding_rate: entry.expected_funding_rate,
            funding_collections: 0,
            total_funding_received: Decimal::ZERO,
            expected_total_funding: Decimal::ZERO,
            entry_fees: entry.entry_fees,
            interest_paid: Decimal::ZERO,
            rebalance_fees: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            hours_open: 0.0,
            hours_unprofitable: 0,
        }
    }

    /// Calculate net PnL: funding received - all costs.
    pub fn net_pnl(&self) -> Decimal {
        self.total_funding_received - self.entry_fees - self.interest_paid - self.rebalance_fees
    }

    /// Calculate total costs.
    pub fn total_costs(&self) -> Decimal {
        self.entry_fees + self.interest_paid + self.rebalance_fees
    }

    /// Calculate funding efficiency (actual / expected).
    pub fn funding_efficiency(&self) -> Option<Decimal> {
        if self.expected_total_funding > Decimal::ZERO {
            Some(self.total_funding_received / self.expected_total_funding)
        } else {
            None
        }
    }

    /// Calculate annualized yield based on current performance.
    pub fn annualized_yield(&self) -> Decimal {
        if self.position_value == Decimal::ZERO || self.hours_open < 1.0 {
            return Decimal::ZERO;
        }

        let net = self.net_pnl();
        let hours_decimal = Decimal::from_f64_retain(self.hours_open).unwrap_or(dec!(1));
        let hourly_return = net / self.position_value / hours_decimal;

        // Annualize: hourly * 24 * 365
        hourly_return * dec!(8760)
    }

    /// Check if position is within grace period.
    pub fn in_grace_period(&self, grace_hours: u32) -> bool {
        let hours_open = (Utc::now() - self.opened_at).num_hours();
        hours_open < grace_hours as i64
    }

    /// Check if position is currently profitable (net PnL > 0).
    pub fn is_profitable(&self) -> bool {
        self.net_pnl() > Decimal::ZERO
    }

    /// Calculate hours open.
    pub fn hours_open(&self) -> f64 {
        let duration = Utc::now() - self.opened_at;
        duration.num_seconds() as f64 / 3600.0
    }

    /// Check if position is within the minimum holding period.
    /// During this period, positions should not be exited voluntarily
    /// (to ensure funding fees cover trading costs).
    pub fn is_within_holding_period(&self, min_holding_hours: u32) -> bool {
        self.hours_open() < min_holding_hours as f64
    }

    /// Calculate estimated time to break-even based on current funding rate.
    /// Returns None if already profitable or funding rate is zero/negative.
    pub fn estimated_breakeven_hours(&self) -> Option<Decimal> {
        let net = self.net_pnl();
        if net >= Decimal::ZERO {
            return Some(Decimal::ZERO); // Already profitable
        }

        // Calculate hourly funding income
        // funding_rate is per 8 hours, so hourly = rate / 8
        let hourly_funding = (self.expected_funding_rate.abs() * self.position_value) / dec!(8);

        if hourly_funding <= Decimal::ZERO {
            return None; // Won't reach breakeven
        }

        // Hours needed = remaining loss / hourly income
        Some(net.abs() / hourly_funding)
    }
}

/// Actions the position tracker can recommend.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionAction {
    /// Position is profitable or within grace period.
    Hold,
    /// Position needs close monitoring.
    MonitorClosely { reason: String },
    /// Position should be considered for exit.
    ConsiderExit {
        reason: String,
        hours_unprofitable: u32,
    },
    /// Position must be closed immediately.
    ForceExit { reason: String },
}

impl PositionAction {
    /// Check if this action requires position closure.
    pub fn requires_close(&self) -> bool {
        matches!(self, PositionAction::ForceExit { .. })
    }
}

/// Manages position tracking and loss detection.
pub struct PositionTracker {
    config: PositionLossConfig,
    positions: HashMap<String, TrackedPosition>,
}

impl PositionTracker {
    /// Create a new position tracker.
    pub fn new(config: PositionLossConfig) -> Self {
        Self {
            config,
            positions: HashMap::new(),
        }
    }

    /// Open a new tracked position.
    pub fn open_position(&mut self, symbol: &str, entry: PositionEntry) -> &TrackedPosition {
        let position = TrackedPosition::new(symbol.to_string(), entry);

        info!(
            symbol = %symbol,
            entry_price = %position.entry_price,
            quantity = %position.quantity,
            expected_funding = %position.expected_funding_rate,
            "Opened tracked position"
        );

        self.positions.insert(symbol.to_string(), position);
        self.positions.get(symbol).unwrap()
    }

    /// Record funding payment for a position.
    pub fn record_funding(&mut self, symbol: &str, amount: Decimal, expected: Decimal) {
        if let Some(pos) = self.positions.get_mut(symbol) {
            pos.total_funding_received += amount;
            pos.expected_total_funding += expected;
            pos.funding_collections += 1;

            let deviation = if expected != Decimal::ZERO {
                ((amount - expected) / expected).abs()
            } else {
                Decimal::ZERO
            };

            if deviation > self.config.max_funding_deviation {
                warn!(
                    symbol = %symbol,
                    actual = %amount,
                    expected = %expected,
                    deviation_pct = %(deviation * dec!(100)),
                    "Funding payment deviation exceeds threshold"
                );
            }

            debug!(
                symbol = %symbol,
                amount = %amount,
                total = %pos.total_funding_received,
                collections = pos.funding_collections,
                "Recorded funding payment"
            );
        }
    }

    /// Record interest payment for a position.
    pub fn record_interest(&mut self, symbol: &str, amount: Decimal) {
        if let Some(pos) = self.positions.get_mut(symbol) {
            pos.interest_paid += amount;
            debug!(
                symbol = %symbol,
                amount = %amount,
                total = %pos.interest_paid,
                "Recorded interest payment"
            );
        }
    }

    /// Record rebalance fee for a position.
    pub fn record_rebalance_fee(&mut self, symbol: &str, amount: Decimal) {
        if let Some(pos) = self.positions.get_mut(symbol) {
            pos.rebalance_fees += amount;
            debug!(
                symbol = %symbol,
                amount = %amount,
                total = %pos.rebalance_fees,
                "Recorded rebalance fee"
            );
        }
    }

    /// Update unrealized PnL for a position.
    pub fn update_pnl(&mut self, symbol: &str, unrealized: Decimal) {
        if let Some(pos) = self.positions.get_mut(symbol) {
            pos.unrealized_pnl = unrealized;

            // Update hours open
            pos.hours_open = (Utc::now() - pos.opened_at).num_minutes() as f64 / 60.0;
        }
    }

    /// Evaluate a position and recommend action.
    pub fn evaluate_position(&mut self, symbol: &str) -> PositionAction {
        let pos = match self.positions.get_mut(symbol) {
            Some(p) => p,
            None => return PositionAction::Hold,
        };

        // Update hours
        pos.hours_open = (Utc::now() - pos.opened_at).num_minutes() as f64 / 60.0;

        // Check grace period
        if pos.in_grace_period(self.config.grace_period_hours) {
            return PositionAction::Hold;
        }

        let net_pnl = pos.net_pnl();
        let total_costs = pos.total_costs();
        let is_profitable = pos.is_profitable();
        let breakeven_hours = pos.estimated_breakeven_hours();

        // Log net profitability metrics
        debug!(
            %symbol,
            net_pnl = %net_pnl,
            funding_received = %pos.total_funding_received,
            interest_paid = %pos.interest_paid,
            total_costs = %total_costs,
            is_profitable = is_profitable,
            hours_open = pos.hours_open,
            breakeven_hours = ?breakeven_hours,
            "Position profitability check"
        );

        // Alert for long-term unprofitable positions (>24 hours and still losing money)
        if !is_profitable && pos.hours_open > 24.0 {
            warn!(
                %symbol,
                hours_open = pos.hours_open,
                %net_pnl,
                %total_costs,
                funding_received = %pos.total_funding_received,
                "⚠️  Position unprofitable after 24+ hours - review required"
            );
        }
        let annualized = pos.annualized_yield();

        // Check if position is unprofitable
        if net_pnl < Decimal::ZERO {
            pos.hours_unprofitable =
                (pos.hours_open - self.config.grace_period_hours as f64).max(0.0) as u32;

            // Force exit if unprofitable for too long
            if pos.hours_unprofitable >= self.config.max_unprofitable_hours {
                return PositionAction::ForceExit {
                    reason: format!(
                        "Position unprofitable for {}h (net PnL: ${:.2})",
                        pos.hours_unprofitable, net_pnl
                    ),
                };
            }

            // Consider exit if yield is significantly below expectations
            if annualized < -self.config.min_expected_yield {
                return PositionAction::ConsiderExit {
                    reason: format!(
                        "Negative yield {:.2}% APY (net PnL: ${:.2})",
                        annualized * dec!(100),
                        net_pnl
                    ),
                    hours_unprofitable: pos.hours_unprofitable,
                };
            }

            return PositionAction::MonitorClosely {
                reason: format!(
                    "Unprofitable for {}h (net PnL: ${:.2})",
                    pos.hours_unprofitable, net_pnl
                ),
            };
        }

        // Reset unprofitable counter if back in profit
        pos.hours_unprofitable = 0;

        // Check funding efficiency
        if let Some(efficiency) = pos.funding_efficiency() {
            if efficiency < dec!(1) - self.config.max_funding_deviation {
                return PositionAction::MonitorClosely {
                    reason: format!("Funding efficiency low: {:.1}%", efficiency * dec!(100)),
                };
            }
        }

        PositionAction::Hold
    }

    /// Close a position and return its final state.
    pub fn close_position(&mut self, symbol: &str) -> Option<TrackedPosition> {
        let position = self.positions.remove(symbol);

        if let Some(ref pos) = position {
            info!(
                symbol = %symbol,
                hours_open = pos.hours_open,
                net_pnl = %pos.net_pnl(),
                funding_received = %pos.total_funding_received,
                total_costs = %pos.total_costs(),
                "Closed tracked position"
            );
        }

        position
    }

    /// Get all unprofitable positions.
    pub fn get_unprofitable_positions(&self) -> Vec<(&str, &TrackedPosition)> {
        self.positions
            .iter()
            .filter(|(_, pos)| {
                !pos.in_grace_period(self.config.grace_period_hours)
                    && pos.net_pnl() < Decimal::ZERO
            })
            .map(|(s, p)| (s.as_str(), p))
            .collect()
    }

    /// Get all positions requiring forced exit.
    pub fn get_positions_to_close(&mut self) -> Vec<String> {
        let symbols: Vec<String> = self.positions.keys().cloned().collect();

        symbols
            .into_iter()
            .filter(|symbol| {
                matches!(
                    self.evaluate_position(symbol),
                    PositionAction::ForceExit { .. }
                )
            })
            .collect()
    }

    /// Get a position by symbol.
    pub fn get_position(&self, symbol: &str) -> Option<&TrackedPosition> {
        self.positions.get(symbol)
    }

    /// Get all tracked positions.
    pub fn all_positions(&self) -> &HashMap<String, TrackedPosition> {
        &self.positions
    }

    /// Get position count.
    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    /// Get aggregate metrics across all positions for monitoring.
    pub fn get_aggregate_metrics(&self) -> AggregateMetrics {
        let mut total_funding_received = Decimal::ZERO;
        let mut total_interest_paid = Decimal::ZERO;
        let mut total_fees = Decimal::ZERO;
        let mut total_net_pnl = Decimal::ZERO;
        let mut total_position_value = Decimal::ZERO;
        let mut profitable_count = 0usize;
        let mut unprofitable_count = 0usize;

        for pos in self.positions.values() {
            total_funding_received += pos.total_funding_received;
            total_interest_paid += pos.interest_paid;
            total_fees += pos.entry_fees + pos.rebalance_fees;
            total_net_pnl += pos.net_pnl();
            total_position_value += pos.position_value;

            if pos.is_profitable() {
                profitable_count += 1;
            } else if !pos.in_grace_period(self.config.grace_period_hours) {
                unprofitable_count += 1;
            }
        }

        AggregateMetrics {
            position_count: self.positions.len(),
            profitable_count,
            unprofitable_count,
            total_position_value,
            total_funding_received,
            total_interest_paid,
            total_fees,
            total_net_pnl,
            net_yield_pct: if total_position_value > Decimal::ZERO {
                (total_net_pnl / total_position_value) * dec!(100)
            } else {
                Decimal::ZERO
            },
        }
    }

    /// Log aggregate profitability summary (call periodically for monitoring).
    pub fn log_profitability_summary(&self) {
        let metrics = self.get_aggregate_metrics();

        if metrics.position_count == 0 {
            return;
        }

        info!(
            position_count = metrics.position_count,
            profitable = metrics.profitable_count,
            unprofitable = metrics.unprofitable_count,
            total_value = %metrics.total_position_value,
            funding_received = %metrics.total_funding_received,
            interest_paid = %metrics.total_interest_paid,
            total_fees = %metrics.total_fees,
            net_pnl = %metrics.total_net_pnl,
            net_yield_pct = %metrics.net_yield_pct,
            "📊 Portfolio profitability summary"
        );

        // Warn if overall portfolio is unprofitable
        if metrics.total_net_pnl < Decimal::ZERO {
            warn!(
                net_pnl = %metrics.total_net_pnl,
                "⚠️  Overall portfolio is currently unprofitable"
            );
        }
    }
}

/// Aggregate metrics across all tracked positions.
#[derive(Debug, Clone, Serialize)]
pub struct AggregateMetrics {
    pub position_count: usize,
    pub profitable_count: usize,
    pub unprofitable_count: usize,
    pub total_position_value: Decimal,
    pub total_funding_received: Decimal,
    pub total_interest_paid: Decimal,
    pub total_fees: Decimal,
    pub total_net_pnl: Decimal,
    pub net_yield_pct: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PositionLossConfig {
        PositionLossConfig {
            max_unprofitable_hours: 48,
            min_expected_yield: dec!(0.10),
            max_funding_deviation: dec!(0.20),
            grace_period_hours: 8,
        }
    }

    #[test]
    fn test_open_and_track_position() {
        let mut tracker = PositionTracker::new(test_config());

        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };

        tracker.open_position("BTCUSDT", entry);

        assert!(tracker.get_position("BTCUSDT").is_some());
        assert_eq!(tracker.position_count(), 1);
    }

    #[test]
    fn test_funding_recording() {
        let mut tracker = PositionTracker::new(test_config());

        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };

        tracker.open_position("BTCUSDT", entry);
        tracker.record_funding("BTCUSDT", dec!(5), dec!(5));

        let pos = tracker.get_position("BTCUSDT").unwrap();
        assert_eq!(pos.total_funding_received, dec!(5));
        assert_eq!(pos.funding_collections, 1);
    }

    #[test]
    fn test_net_pnl_calculation() {
        let mut tracker = PositionTracker::new(test_config());

        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };

        tracker.open_position("BTCUSDT", entry);
        tracker.record_funding("BTCUSDT", dec!(10), dec!(10));
        tracker.record_interest("BTCUSDT", dec!(1));
        tracker.record_rebalance_fee("BTCUSDT", dec!(0.5));

        let pos = tracker.get_position("BTCUSDT").unwrap();

        // Net = 10 - 2 (entry) - 1 (interest) - 0.5 (rebalance) = 6.5
        assert_eq!(pos.net_pnl(), dec!(6.5));
    }

    #[test]
    fn test_close_position() {
        let mut tracker = PositionTracker::new(test_config());

        let entry = PositionEntry {
            symbol: "BTCUSDT".to_string(),
            entry_price: dec!(50000),
            quantity: dec!(0.1),
            expected_funding_rate: dec!(0.0001),
            entry_fees: dec!(2),
            position_value: dec!(5000),
        };

        tracker.open_position("BTCUSDT", entry);
        let closed = tracker.close_position("BTCUSDT");

        assert!(closed.is_some());
        assert!(tracker.get_position("BTCUSDT").is_none());
    }
}
