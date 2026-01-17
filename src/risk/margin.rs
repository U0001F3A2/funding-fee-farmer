//! Margin monitoring and health checks.

use crate::config::RiskConfig;
use crate::exchange::{LeverageBracket, MarginType, Position};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Margin health status levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MarginHealth {
    /// Margin ratio > 500% - Safe
    Green,
    /// Margin ratio 300-500% - Caution
    Yellow,
    /// Margin ratio 200-300% - Warning
    Orange,
    /// Margin ratio < 200% - Critical
    Red,
}

impl MarginHealth {
    /// Get the threshold for this health level.
    pub fn threshold(&self) -> Decimal {
        match self {
            MarginHealth::Green => dec!(5.0),
            MarginHealth::Yellow => dec!(3.0),
            MarginHealth::Orange => dec!(2.0),
            MarginHealth::Red => Decimal::ZERO,
        }
    }

    /// Recommended action for this health level.
    pub fn action(&self) -> &'static str {
        match self {
            MarginHealth::Green => "Normal operation",
            MarginHealth::Yellow => "Reduce position size by 25%",
            MarginHealth::Orange => "Emergency deleveraging",
            MarginHealth::Red => "Full position closure",
        }
    }
}

/// Monitors margin levels across all positions.
pub struct MarginMonitor {
    config: RiskConfig,
}

impl MarginMonitor {
    /// Create a new margin monitor.
    pub fn new(config: RiskConfig) -> Self {
        Self { config }
    }

    /// Calculate margin ratio for a position.
    ///
    /// Margin Ratio = Position Margin / Maintenance Margin
    ///
    /// # Arguments
    /// * `position_margin` - The margin allocated to this specific position
    /// * `maintenance_margin_rate` - The actual maintenance margin rate from Binance API
    /// * `position_value` - The notional value of the position
    pub fn calculate_margin_ratio(
        &self,
        position_margin: Decimal,
        maintenance_margin_rate: Decimal,
        position_value: Decimal,
    ) -> Decimal {
        if position_value == Decimal::ZERO {
            return Decimal::MAX;
        }

        let maintenance_margin = position_value * maintenance_margin_rate;

        if maintenance_margin == Decimal::ZERO {
            return Decimal::MAX;
        }

        position_margin / maintenance_margin
    }

    /// Calculate per-position margin allocation.
    ///
    /// For isolated margin: uses the position's isolated_margin directly.
    /// For cross margin: allocates total margin proportionally to position values.
    pub fn calculate_position_margin(
        position: &Position,
        all_positions: &[Position],
        total_margin: Decimal,
    ) -> Decimal {
        match position.margin_type {
            MarginType::Isolated => position.isolated_margin,
            MarginType::Cross => {
                // Calculate total notional across all positions
                let total_notional: Decimal = all_positions
                    .iter()
                    .map(|p| p.notional.abs())
                    .sum();

                if total_notional == Decimal::ZERO {
                    return Decimal::ZERO;
                }

                // Allocate margin proportionally to this position's notional
                let position_notional = position.notional.abs();
                (position_notional / total_notional) * total_margin
            }
        }
    }

    /// Build a map of symbol -> maintenance margin rate from leverage brackets.
    ///
    /// This selects the appropriate maintenance margin rate based on the position's
    /// notional value within the tiered bracket system.
    pub fn build_maintenance_rate_map(
        brackets: &[LeverageBracket],
        positions: &[Position],
    ) -> HashMap<String, Decimal> {
        let mut rate_map = HashMap::new();

        for bracket in brackets {
            // Find the position for this symbol to get its notional value
            if let Some(position) = positions.iter().find(|p| p.symbol == bracket.symbol) {
                let notional = position.notional.abs();

                // Find the appropriate bracket tier based on notional value
                let maint_rate = bracket
                    .brackets
                    .iter()
                    .find(|b| notional >= b.notional_floor && notional <= b.notional_cap)
                    .map(|b| b.maint_margin_ratio)
                    .unwrap_or(dec!(0.004)); // Fallback to 0.4% if not found

                rate_map.insert(bracket.symbol.clone(), maint_rate);
            } else {
                // No position for this symbol, use the first bracket's rate as default
                if let Some(first_bracket) = bracket.brackets.first() {
                    rate_map.insert(bracket.symbol.clone(), first_bracket.maint_margin_ratio);
                }
            }
        }

        rate_map
    }

    /// Get overall margin health based on ratio.
    pub fn get_health(&self, margin_ratio: Decimal) -> MarginHealth {
        if margin_ratio >= dec!(5.0) {
            MarginHealth::Green
        } else if margin_ratio >= dec!(3.0) {
            MarginHealth::Yellow
        } else if margin_ratio >= dec!(2.0) {
            MarginHealth::Orange
        } else {
            MarginHealth::Red
        }
    }

    /// Check all positions and return worst health status.
    ///
    /// # Arguments
    /// * `positions` - All current positions
    /// * `total_margin` - Total margin balance (for cross-margin allocation)
    /// * `maintenance_rates` - Map of symbol -> maintenance margin rate from API
    pub fn check_positions(
        &self,
        positions: &[Position],
        total_margin: Decimal,
        maintenance_rates: &HashMap<String, Decimal>,
    ) -> (MarginHealth, Vec<(String, MarginHealth)>) {
        let mut worst_health = MarginHealth::Green;
        let mut position_health = Vec::new();

        for pos in positions {
            if pos.position_amt.abs() == Decimal::ZERO {
                continue;
            }

            // Get maintenance rate for this symbol (fallback to 0.4%)
            let maint_rate = maintenance_rates
                .get(&pos.symbol)
                .copied()
                .unwrap_or(dec!(0.004));

            // Calculate position-specific margin
            let position_margin = Self::calculate_position_margin(pos, positions, total_margin);

            let ratio = self.calculate_margin_ratio(
                position_margin,
                maint_rate,
                pos.notional.abs(),
            );

            let health = self.get_health(ratio);

            debug!(
                symbol = %pos.symbol,
                margin_ratio = %ratio,
                position_margin = %position_margin,
                maint_rate = %maint_rate,
                health = ?health,
                "Position health check"
            );

            if (health as u8) > (worst_health as u8) {
                worst_health = health;
            }

            position_health.push((pos.symbol.clone(), health));
        }

        if worst_health != MarginHealth::Green {
            warn!(
                health = ?worst_health,
                action = worst_health.action(),
                "Margin health alert"
            );
        }

        (worst_health, position_health)
    }

    /// Calculate how much position reduction is needed to reach target health.
    ///
    /// # Arguments
    /// * `position_margin` - The margin allocated to this specific position
    /// * `maintenance_margin_rate` - The actual maintenance margin rate for this symbol
    /// * `position_value` - Current position notional value
    /// * `target_health` - The desired health level to achieve
    pub fn calculate_reduction_needed(
        &self,
        position_margin: Decimal,
        maintenance_margin_rate: Decimal,
        position_value: Decimal,
        target_health: MarginHealth,
    ) -> Decimal {
        let target_ratio = target_health.threshold();
        let current_ratio = self.calculate_margin_ratio(
            position_margin,
            maintenance_margin_rate,
            position_value,
        );

        if current_ratio >= target_ratio {
            return Decimal::ZERO;
        }

        // How much position value needs to be reduced
        // target_ratio = margin / (position * maint_rate)
        // position_target = margin / (target_ratio * maint_rate)
        let target_position = position_margin / (target_ratio * maintenance_margin_rate);
        (position_value - target_position).max(Decimal::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_monitor() -> MarginMonitor {
        MarginMonitor::new(RiskConfig {
            max_drawdown: dec!(0.05),
            min_margin_ratio: dec!(3.0),
            max_single_position: dec!(0.30),
            max_unprofitable_hours: 48,
            min_expected_yield: dec!(0.10),
            grace_period_hours: 8,
            max_funding_deviation: dec!(0.20),
            max_errors_per_minute: 10,
            max_consecutive_failures: 3,
            emergency_delta_drift: dec!(0.10),
            max_consecutive_risk_cycles: 3,
        })
    }

    #[test]
    fn test_margin_ratio_calculation() {
        let monitor = test_monitor();

        // $10,000 position margin, 0.4% maint rate, $50,000 position
        let ratio = monitor.calculate_margin_ratio(
            dec!(10000),     // position_margin
            dec!(0.004),     // maintenance_margin_rate
            dec!(50000),     // position_value
        );

        // Maintenance margin = 50000 * 0.004 = 200
        // Ratio = 10000 / 200 = 50
        assert_eq!(ratio, dec!(50));
    }

    #[test]
    fn test_margin_ratio_multi_position() {
        use crate::exchange::{MarginType, PositionSide};

        let monitor = test_monitor();

        // Create two positions sharing $10,000 margin in cross mode
        let pos1 = Position {
            symbol: "BTCUSDT".to_string(),
            position_amt: dec!(1.0),
            entry_price: dec!(50000),
            mark_price: dec!(50000),
            unrealized_profit: Decimal::ZERO,
            liquidation_price: dec!(45000),
            leverage: 5,
            position_side: PositionSide::Both,
            notional: dec!(50000),  // $50k notional
            isolated_margin: Decimal::ZERO,
            margin_type: MarginType::Cross,
        };

        let pos2 = Position {
            symbol: "ETHUSDT".to_string(),
            position_amt: dec!(10.0),
            entry_price: dec!(3000),
            mark_price: dec!(3000),
            unrealized_profit: Decimal::ZERO,
            liquidation_price: dec!(2700),
            leverage: 5,
            position_side: PositionSide::Both,
            notional: dec!(30000),  // $30k notional
            isolated_margin: Decimal::ZERO,
            margin_type: MarginType::Cross,
        };

        let all_positions = vec![pos1.clone(), pos2.clone()];
        let total_margin = dec!(10000);

        // Total notional = 50k + 30k = 80k
        // BTC gets: (50k / 80k) * 10k = $6,250 margin
        // ETH gets: (30k / 80k) * 10k = $3,750 margin

        let btc_margin = MarginMonitor::calculate_position_margin(&pos1, &all_positions, total_margin);
        let eth_margin = MarginMonitor::calculate_position_margin(&pos2, &all_positions, total_margin);

        assert_eq!(btc_margin, dec!(6250));
        assert_eq!(eth_margin, dec!(3750));

        // BTC ratio = 6250 / (50000 * 0.004) = 6250 / 200 = 31.25
        let btc_ratio = monitor.calculate_margin_ratio(btc_margin, dec!(0.004), pos1.notional);
        assert_eq!(btc_ratio, dec!(31.25));

        // ETH ratio = 3750 / (30000 * 0.004) = 3750 / 120 = 31.25
        let eth_ratio = monitor.calculate_margin_ratio(eth_margin, dec!(0.004), pos2.notional);
        assert_eq!(eth_ratio, dec!(31.25));
    }

    #[test]
    fn test_margin_ratio_isolated_vs_cross() {
        use crate::exchange::{MarginType, PositionSide};

        let monitor = test_monitor();

        // Isolated position with dedicated margin
        let isolated_pos = Position {
            symbol: "BTCUSDT".to_string(),
            position_amt: dec!(1.0),
            entry_price: dec!(50000),
            mark_price: dec!(50000),
            unrealized_profit: Decimal::ZERO,
            liquidation_price: dec!(45000),
            leverage: 5,
            position_side: PositionSide::Both,
            notional: dec!(50000),
            isolated_margin: dec!(12000),  // Dedicated $12k margin
            margin_type: MarginType::Isolated,
        };

        let all_positions = vec![isolated_pos.clone()];
        let total_margin = dec!(100000);  // Total margin doesn't matter for isolated

        let margin = MarginMonitor::calculate_position_margin(&isolated_pos, &all_positions, total_margin);

        // Should use isolated_margin, not share total_margin
        assert_eq!(margin, dec!(12000));

        // Ratio = 12000 / (50000 * 0.004) = 12000 / 200 = 60
        let ratio = monitor.calculate_margin_ratio(margin, dec!(0.004), isolated_pos.notional);
        assert_eq!(ratio, dec!(60));
    }

    #[test]
    fn test_health_levels() {
        let monitor = test_monitor();

        assert_eq!(monitor.get_health(dec!(10.0)), MarginHealth::Green);
        assert_eq!(monitor.get_health(dec!(4.0)), MarginHealth::Yellow);
        assert_eq!(monitor.get_health(dec!(2.5)), MarginHealth::Orange);
        assert_eq!(monitor.get_health(dec!(1.5)), MarginHealth::Red);
    }
}
