//! Liquidation prevention and emergency exit logic.

use crate::exchange::Position;
use crate::risk::margin::{MarginHealth, MarginMonitor};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};
use tracing::{error, info, warn};

/// Action to take for a position at risk of liquidation.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum LiquidationAction {
    /// No action needed
    None,
    /// Reduce position by specified percentage
    ReducePosition { symbol: String, reduction_pct: Decimal },
    /// Close position entirely
    ClosePosition { symbol: String },
    /// Add margin if possible
    AddMargin { symbol: String, amount: Decimal },
}

/// Guards against liquidation by monitoring and taking preventive action.
pub struct LiquidationGuard {
    margin_monitor: MarginMonitor,
    /// Symbols currently being processed (to prevent duplicate actions)
    processing: HashSet<String>,
}

impl LiquidationGuard {
    /// Create a new liquidation guard.
    pub fn new(margin_monitor: MarginMonitor) -> Self {
        Self {
            margin_monitor,
            processing: HashSet::new(),
        }
    }

    /// Evaluate positions and determine required actions.
    ///
    /// # Arguments
    /// * `positions` - All current positions
    /// * `total_margin` - Total margin balance (for cross-margin allocation)
    /// * `maintenance_rates` - Map of symbol -> maintenance margin rate from API
    pub fn evaluate(
        &self,
        positions: &[Position],
        total_margin: Decimal,
        maintenance_rates: &HashMap<String, Decimal>,
    ) -> Vec<LiquidationAction> {
        let mut actions = Vec::new();

        for pos in positions {
            if pos.position_amt.abs() == Decimal::ZERO {
                continue;
            }

            // Skip if already processing this symbol
            if self.processing.contains(&pos.symbol) {
                continue;
            }

            // Get maintenance rate for this symbol (fallback to 0.4%)
            let maint_rate = maintenance_rates
                .get(&pos.symbol)
                .copied()
                .unwrap_or(dec!(0.004));

            // Calculate position-specific margin
            let position_margin = MarginMonitor::calculate_position_margin(pos, positions, total_margin);

            let ratio = self.margin_monitor.calculate_margin_ratio(
                position_margin,
                maint_rate,
                pos.notional.abs(),
            );

            let health = self.margin_monitor.get_health(ratio);

            let action = match health {
                MarginHealth::Green => LiquidationAction::None,

                MarginHealth::Yellow => {
                    info!(
                        symbol = %pos.symbol,
                        margin_ratio = %ratio,
                        "Yellow zone - reducing position by 25%"
                    );
                    LiquidationAction::ReducePosition {
                        symbol: pos.symbol.clone(),
                        reduction_pct: dec!(0.25),
                    }
                }

                MarginHealth::Orange => {
                    warn!(
                        symbol = %pos.symbol,
                        margin_ratio = %ratio,
                        "Orange zone - reducing position by 50%"
                    );
                    LiquidationAction::ReducePosition {
                        symbol: pos.symbol.clone(),
                        reduction_pct: dec!(0.50),
                    }
                }

                MarginHealth::Red => {
                    error!(
                        symbol = %pos.symbol,
                        margin_ratio = %ratio,
                        liquidation_price = %pos.liquidation_price,
                        "RED ZONE - closing position immediately"
                    );
                    LiquidationAction::ClosePosition {
                        symbol: pos.symbol.clone(),
                    }
                }
            };

            if !matches!(action, LiquidationAction::None) {
                actions.push(action);
            }
        }

        actions
    }

    /// Calculate distance to liquidation in percentage terms.
    pub fn liquidation_distance(position: &Position) -> Option<Decimal> {
        if position.mark_price == Decimal::ZERO {
            return None;
        }

        let liq_price = position.liquidation_price;
        if liq_price == Decimal::ZERO {
            return None;
        }

        let distance = ((position.mark_price - liq_price) / position.mark_price).abs();
        Some(distance * dec!(100)) // Return as percentage
    }

    /// Check if any position is dangerously close to liquidation.
    pub fn any_critical(&self, positions: &[Position]) -> bool {
        positions.iter().any(|pos| {
            if let Some(distance) = Self::liquidation_distance(pos) {
                distance < dec!(5.0) // Less than 5% from liquidation
            } else {
                false
            }
        })
    }

    /// Mark a symbol as being processed (to prevent duplicate actions).
    pub fn mark_processing(&mut self, symbol: &str) {
        self.processing.insert(symbol.to_string());
    }

    /// Clear processing flag for a symbol.
    pub fn clear_processing(&mut self, symbol: &str) {
        self.processing.remove(symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RiskConfig;
    use crate::exchange::{MarginType, PositionSide};

    fn test_position(symbol: &str, notional: Decimal, margin_balance: Decimal) -> Position {
        Position {
            symbol: symbol.to_string(),
            position_amt: dec!(1.0),
            entry_price: dec!(50000),
            mark_price: dec!(50000),
            unrealized_profit: Decimal::ZERO,
            liquidation_price: dec!(45000),
            leverage: 5,
            position_side: PositionSide::Both,
            notional,
            isolated_margin: Decimal::ZERO,
            margin_type: MarginType::Cross,
        }
    }

    #[test]
    fn test_liquidation_distance() {
        let pos = test_position("BTCUSDT", dec!(50000), dec!(10000));
        let distance = LiquidationGuard::liquidation_distance(&pos);

        // Distance = |50000 - 45000| / 50000 = 10%
        assert_eq!(distance, Some(dec!(10.0)));
    }
}
