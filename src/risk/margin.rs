//! Margin monitoring and health checks.

use crate::config::RiskConfig;
use crate::exchange::Position;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, warn};

/// Margin health status levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Margin Ratio = Margin Balance / Maintenance Margin
    pub fn calculate_margin_ratio(
        &self,
        margin_balance: Decimal,
        position_value: Decimal,
        _leverage: u8,
    ) -> Decimal {
        if position_value == Decimal::ZERO {
            return Decimal::MAX;
        }

        // Maintenance margin is typically ~0.4% of position value for majors
        let maintenance_margin = position_value * dec!(0.004);

        if maintenance_margin == Decimal::ZERO {
            return Decimal::MAX;
        }

        margin_balance / maintenance_margin
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
    pub fn check_positions(
        &self,
        positions: &[Position],
        total_margin: Decimal,
    ) -> (MarginHealth, Vec<(String, MarginHealth)>) {
        let mut worst_health = MarginHealth::Green;
        let mut position_health = Vec::new();

        for pos in positions {
            if pos.position_amt.abs() == Decimal::ZERO {
                continue;
            }

            let ratio = self.calculate_margin_ratio(
                total_margin,
                pos.notional.abs(),
                pos.leverage,
            );

            let health = self.get_health(ratio);

            debug!(
                symbol = %pos.symbol,
                margin_ratio = %ratio,
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
    pub fn calculate_reduction_needed(
        &self,
        current_margin: Decimal,
        position_value: Decimal,
        target_health: MarginHealth,
    ) -> Decimal {
        let target_ratio = target_health.threshold();
        let current_ratio = self.calculate_margin_ratio(
            current_margin,
            position_value,
            5, // Assume default leverage
        );

        if current_ratio >= target_ratio {
            return Decimal::ZERO;
        }

        // How much position value needs to be reduced
        // target_ratio = margin / (position * 0.004)
        // position_target = margin / (target_ratio * 0.004)
        let target_position = current_margin / (target_ratio * dec!(0.004));
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
        })
    }

    #[test]
    fn test_margin_ratio_calculation() {
        let monitor = test_monitor();

        // $10,000 margin, $50,000 position
        let ratio = monitor.calculate_margin_ratio(
            dec!(10000),
            dec!(50000),
            5,
        );

        // Maintenance margin = 50000 * 0.004 = 200
        // Ratio = 10000 / 200 = 50
        assert_eq!(ratio, dec!(50));
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
