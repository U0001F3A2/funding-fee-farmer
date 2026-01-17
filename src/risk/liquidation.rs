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
    ReducePosition {
        symbol: String,
        reduction_pct: Decimal,
    },
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
            let position_margin =
                MarginMonitor::calculate_position_margin(pos, positions, total_margin);

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

    // =========================================================================
    // Test Helpers
    // =========================================================================

    fn test_risk_config() -> RiskConfig {
        RiskConfig {
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

    fn test_guard() -> LiquidationGuard {
        LiquidationGuard::new(MarginMonitor::new(test_risk_config()))
    }

    fn test_position(symbol: &str, notional: Decimal, isolated_margin: Decimal) -> Position {
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
            isolated_margin,
            margin_type: MarginType::Isolated,
        }
    }

    fn test_cross_position(symbol: &str, notional: Decimal) -> Position {
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

    // =========================================================================
    // Liquidation Distance Tests
    // =========================================================================

    #[test]
    fn test_liquidation_distance() {
        let pos = test_position("BTCUSDT", dec!(50000), dec!(10000));
        let distance = LiquidationGuard::liquidation_distance(&pos);

        // Distance = |50000 - 45000| / 50000 = 10%
        assert_eq!(distance, Some(dec!(10.0)));
    }

    #[test]
    fn test_liquidation_distance_close_to_liq() {
        let mut pos = test_position("BTCUSDT", dec!(50000), dec!(10000));
        pos.mark_price = dec!(46000);
        pos.liquidation_price = dec!(45000);

        let distance = LiquidationGuard::liquidation_distance(&pos);

        // Distance = |46000 - 45000| / 46000 = 1000/46000 ≈ 2.17%
        let expected = (dec!(1000) / dec!(46000)) * dec!(100);
        assert!((distance.unwrap() - expected).abs() < dec!(0.01));
    }

    #[test]
    fn test_liquidation_distance_zero_mark_price() {
        let mut pos = test_position("BTCUSDT", dec!(50000), dec!(10000));
        pos.mark_price = Decimal::ZERO;

        let distance = LiquidationGuard::liquidation_distance(&pos);

        assert_eq!(distance, None);
    }

    #[test]
    fn test_liquidation_distance_zero_liq_price() {
        let mut pos = test_position("BTCUSDT", dec!(50000), dec!(10000));
        pos.liquidation_price = Decimal::ZERO;

        let distance = LiquidationGuard::liquidation_distance(&pos);

        assert_eq!(distance, None);
    }

    #[test]
    fn test_liquidation_distance_short_position() {
        let mut pos = test_position("BTCUSDT", dec!(-50000), dec!(10000));
        // For shorts, liquidation price is above mark price
        pos.mark_price = dec!(50000);
        pos.liquidation_price = dec!(55000);
        pos.position_amt = dec!(-1.0);

        let distance = LiquidationGuard::liquidation_distance(&pos);

        // Distance = |50000 - 55000| / 50000 = 10%
        assert_eq!(distance, Some(dec!(10.0)));
    }

    // =========================================================================
    // Any Critical Tests
    // =========================================================================

    #[test]
    fn test_any_critical_no_positions() {
        let guard = test_guard();
        let positions: Vec<Position> = vec![];

        assert!(!guard.any_critical(&positions));
    }

    #[test]
    fn test_any_critical_safe_position() {
        let guard = test_guard();
        let positions = vec![test_position("BTCUSDT", dec!(50000), dec!(10000))];

        // Position has 10% distance to liquidation - safe
        assert!(!guard.any_critical(&positions));
    }

    #[test]
    fn test_any_critical_dangerous_position() {
        let guard = test_guard();
        let mut pos = test_position("BTCUSDT", dec!(50000), dec!(10000));
        // Mark price very close to liquidation (3%)
        pos.mark_price = dec!(46391);
        pos.liquidation_price = dec!(45000);
        let positions = vec![pos];

        // Distance = |46391 - 45000| / 46391 ≈ 3% < 5%
        assert!(guard.any_critical(&positions));
    }

    #[test]
    fn test_any_critical_one_of_many() {
        let guard = test_guard();

        let safe_pos = test_position("BTCUSDT", dec!(50000), dec!(10000));

        let mut danger_pos = test_position("ETHUSDT", dec!(30000), dec!(6000));
        danger_pos.mark_price = dec!(3100);
        danger_pos.liquidation_price = dec!(3000);
        // Distance = |3100 - 3000| / 3100 ≈ 3.2% < 5%

        let positions = vec![safe_pos, danger_pos];

        // Should return true if ANY position is critical
        assert!(guard.any_critical(&positions));
    }

    // =========================================================================
    // Evaluate Tests - Margin Health Actions
    // =========================================================================

    #[test]
    fn test_evaluate_green_no_action() {
        let guard = test_guard();

        // Position with very high margin = Green
        let positions = vec![test_position("BTCUSDT", dec!(1000), dec!(50000))];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        // Margin ratio = 50000 / (1000 * 0.004) = 50000 / 4 = 12500 -> Green
        assert!(actions.is_empty());
    }

    #[test]
    fn test_evaluate_yellow_reduce_25() {
        let guard = test_guard();

        // Position in Yellow zone (margin ratio 3-5)
        // Need: margin / (notional * 0.004) = ~4
        // If notional = 10000, maint = 40, margin = 160 gives ratio = 4
        let positions = vec![test_position("BTCUSDT", dec!(10000), dec!(160))];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            LiquidationAction::ReducePosition {
                symbol,
                reduction_pct,
            } => {
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(*reduction_pct, dec!(0.25));
            }
            _ => panic!("Expected ReducePosition action"),
        }
    }

    #[test]
    fn test_evaluate_orange_reduce_50() {
        let guard = test_guard();

        // Position in Orange zone (margin ratio 2-3)
        // If notional = 10000, maint = 40, margin = 100 gives ratio = 2.5
        let positions = vec![test_position("BTCUSDT", dec!(10000), dec!(100))];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            LiquidationAction::ReducePosition {
                symbol,
                reduction_pct,
            } => {
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(*reduction_pct, dec!(0.50));
            }
            _ => panic!("Expected ReducePosition action with 50%"),
        }
    }

    #[test]
    fn test_evaluate_red_close_position() {
        let guard = test_guard();

        // Position in Red zone (margin ratio < 2)
        // If notional = 10000, maint = 40, margin = 50 gives ratio = 1.25
        let positions = vec![test_position("BTCUSDT", dec!(10000), dec!(50))];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            LiquidationAction::ClosePosition { symbol } => {
                assert_eq!(symbol, "BTCUSDT");
            }
            _ => panic!("Expected ClosePosition action"),
        }
    }

    #[test]
    fn test_evaluate_uses_fallback_rate() {
        let guard = test_guard();

        // Position with no rate in map - should use fallback 0.4%
        let positions = vec![test_position("NEWUSDT", dec!(10000), dec!(200))];

        // Empty rates map
        let rates = HashMap::new();

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        // Should still evaluate using fallback rate
        // Ratio = 200 / (10000 * 0.004) = 200 / 40 = 5 -> Green
        assert!(actions.is_empty());
    }

    #[test]
    fn test_evaluate_skips_zero_positions() {
        let guard = test_guard();

        let mut pos = test_position("BTCUSDT", dec!(10000), dec!(50));
        pos.position_amt = Decimal::ZERO;

        let positions = vec![pos];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        // Zero position should be skipped
        assert!(actions.is_empty());
    }

    #[test]
    fn test_evaluate_multiple_positions() {
        let guard = test_guard();

        let positions = vec![
            test_position("BTCUSDT", dec!(10000), dec!(160)), // Yellow
            test_position("ETHUSDT", dec!(10000), dec!(50)),  // Red
        ];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));
        rates.insert("ETHUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        assert_eq!(actions.len(), 2);

        // Find the BTC action (should be ReducePosition 25%)
        let btc_action = actions.iter().find(|a| match a {
            LiquidationAction::ReducePosition { symbol, .. } => symbol == "BTCUSDT",
            _ => false,
        });
        assert!(btc_action.is_some());

        // Find the ETH action (should be ClosePosition)
        let eth_action = actions.iter().find(|a| match a {
            LiquidationAction::ClosePosition { symbol } => symbol == "ETHUSDT",
            _ => false,
        });
        assert!(eth_action.is_some());
    }

    // =========================================================================
    // Processing Flag Tests
    // =========================================================================

    #[test]
    fn test_mark_processing() {
        let mut guard = test_guard();

        guard.mark_processing("BTCUSDT");

        // Now evaluation should skip this symbol
        let positions = vec![test_position("BTCUSDT", dec!(10000), dec!(50))]; // Would be Red

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        // Should be empty because BTCUSDT is marked as processing
        assert!(actions.is_empty());
    }

    #[test]
    fn test_clear_processing() {
        let mut guard = test_guard();

        guard.mark_processing("BTCUSDT");
        guard.clear_processing("BTCUSDT");

        // Now evaluation should NOT skip this symbol
        let positions = vec![test_position("BTCUSDT", dec!(10000), dec!(50))]; // Red

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        // Should have action now
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn test_processing_multiple_symbols() {
        let mut guard = test_guard();

        guard.mark_processing("BTCUSDT");

        let positions = vec![
            test_position("BTCUSDT", dec!(10000), dec!(50)), // Red but processing
            test_position("ETHUSDT", dec!(10000), dec!(50)), // Red and not processing
        ];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));
        rates.insert("ETHUSDT".to_string(), dec!(0.004));

        let actions = guard.evaluate(&positions, dec!(100000), &rates);

        // Only ETH action should be returned
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            LiquidationAction::ClosePosition { symbol } => {
                assert_eq!(symbol, "ETHUSDT");
            }
            _ => panic!("Expected ClosePosition for ETHUSDT"),
        }
    }

    // =========================================================================
    // Cross Margin Tests
    // =========================================================================

    #[test]
    fn test_evaluate_cross_margin_allocation() {
        let guard = test_guard();

        // Two cross-margin positions sharing $1000 margin
        let positions = vec![
            test_cross_position("BTCUSDT", dec!(5000)), // 50% of total notional
            test_cross_position("ETHUSDT", dec!(5000)), // 50% of total notional
        ];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));
        rates.insert("ETHUSDT".to_string(), dec!(0.004));

        // Total margin = $1000
        // Each position gets $500 margin
        // Each position's maint = 5000 * 0.004 = 20
        // Each ratio = 500 / 20 = 25 -> Green
        let actions = guard.evaluate(&positions, dec!(1000), &rates);

        assert!(actions.is_empty()); // Both Green
    }

    #[test]
    fn test_evaluate_cross_margin_unbalanced() {
        let guard = test_guard();

        // Unbalanced cross-margin positions
        let positions = vec![
            test_cross_position("BTCUSDT", dec!(9000)), // 90% of total notional
            test_cross_position("ETHUSDT", dec!(1000)), // 10% of total notional
        ];

        let mut rates = HashMap::new();
        rates.insert("BTCUSDT".to_string(), dec!(0.004));
        rates.insert("ETHUSDT".to_string(), dec!(0.004));

        // Total margin = $100 (very low)
        // BTC gets: (9000/10000) * 100 = $90
        // BTC maint = 9000 * 0.004 = 36
        // BTC ratio = 90 / 36 = 2.5 -> Orange
        // ETH gets: (1000/10000) * 100 = $10
        // ETH maint = 1000 * 0.004 = 4
        // ETH ratio = 10 / 4 = 2.5 -> Orange
        let actions = guard.evaluate(&positions, dec!(100), &rates);

        // Both should have Orange actions (50% reduction)
        assert_eq!(actions.len(), 2);
        for action in &actions {
            match action {
                LiquidationAction::ReducePosition { reduction_pct, .. } => {
                    assert_eq!(*reduction_pct, dec!(0.50));
                }
                _ => panic!("Expected ReducePosition with 50%"),
            }
        }
    }

    // =========================================================================
    // LiquidationAction Tests
    // =========================================================================

    #[test]
    fn test_liquidation_action_equality() {
        let action1 = LiquidationAction::ClosePosition {
            symbol: "BTCUSDT".to_string(),
        };
        let action2 = LiquidationAction::ClosePosition {
            symbol: "BTCUSDT".to_string(),
        };
        let action3 = LiquidationAction::ClosePosition {
            symbol: "ETHUSDT".to_string(),
        };

        assert_eq!(action1, action2);
        assert_ne!(action1, action3);
    }

    #[test]
    fn test_liquidation_action_reduce_equality() {
        let action1 = LiquidationAction::ReducePosition {
            symbol: "BTCUSDT".to_string(),
            reduction_pct: dec!(0.25),
        };
        let action2 = LiquidationAction::ReducePosition {
            symbol: "BTCUSDT".to_string(),
            reduction_pct: dec!(0.25),
        };
        let action3 = LiquidationAction::ReducePosition {
            symbol: "BTCUSDT".to_string(),
            reduction_pct: dec!(0.50),
        };

        assert_eq!(action1, action2);
        assert_ne!(action1, action3);
    }

    #[test]
    fn test_liquidation_action_add_margin() {
        let action = LiquidationAction::AddMargin {
            symbol: "BTCUSDT".to_string(),
            amount: dec!(1000),
        };

        match action {
            LiquidationAction::AddMargin { symbol, amount } => {
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(amount, dec!(1000));
            }
            _ => panic!("Expected AddMargin action"),
        }
    }
}
