//! Hedge rebalancing logic to maintain delta neutrality.

use crate::exchange::{
    BinanceClient, DeltaNeutralPosition, MarginOrder, NewOrder, OrderResponse, OrderSide,
    OrderType, SideEffectType,
};
use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

/// Configuration for hedge rebalancing.
#[derive(Debug, Clone)]
pub struct RebalanceConfig {
    /// Maximum allowed delta drift as a percentage (e.g., 0.05 = 5%)
    pub max_delta_drift: Decimal,
    /// Minimum rebalance size in USDT to avoid tiny trades
    pub min_rebalance_size: Decimal,
    /// Whether to auto-flip positions when funding direction reverses
    pub auto_flip_on_reversal: bool,
}

impl Default for RebalanceConfig {
    fn default() -> Self {
        Self {
            max_delta_drift: dec!(0.03), // 3% drift triggers rebalance
            min_rebalance_size: dec!(100), // Min $100 trade
            auto_flip_on_reversal: true,
        }
    }
}

/// Action to take for rebalancing.
#[derive(Debug, Clone)]
pub enum RebalanceAction {
    /// No rebalancing needed
    None,
    /// Adjust spot position to match futures
    AdjustSpot {
        symbol: String,
        side: OrderSide,
        quantity: Decimal,
    },
    /// Adjust futures position to match spot
    AdjustFutures {
        symbol: String,
        side: OrderSide,
        quantity: Decimal,
    },
    /// Flip the entire position (funding direction changed)
    FlipPosition {
        symbol: String,
        new_funding_direction: FundingDirection,
    },
    /// Close position entirely (funding no longer profitable)
    ClosePosition { symbol: String },
}

/// Direction of funding payments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FundingDirection {
    /// Positive funding: shorts receive, longs pay
    Positive,
    /// Negative funding: longs receive, shorts pay
    Negative,
}

/// Result of a rebalance operation.
#[derive(Debug)]
pub struct RebalanceResult {
    pub symbol: String,
    pub action_taken: RebalanceAction,
    pub order: Option<OrderResponse>,
    pub new_delta: Decimal,
    pub success: bool,
    pub error: Option<String>,
}

/// Manages hedge rebalancing to maintain delta neutrality.
pub struct HedgeRebalancer {
    config: RebalanceConfig,
}

impl HedgeRebalancer {
    /// Create a new hedge rebalancer.
    pub fn new(config: RebalanceConfig) -> Self {
        Self { config }
    }

    /// Analyze a position and determine if rebalancing is needed.
    pub fn analyze_position(
        &self,
        position: &DeltaNeutralPosition,
        current_funding_rate: Decimal,
        current_price: Decimal,
    ) -> RebalanceAction {
        let futures_qty_abs = position.futures_qty.abs();
        let spot_qty_abs = position.spot_qty.abs();

        // Calculate delta as percentage of position size (in quantity terms)
        let position_size = futures_qty_abs.max(spot_qty_abs);
        if position_size == Decimal::ZERO {
            return RebalanceAction::None;
        }

        // Delta percentage: how much the hedge has drifted as % of position
        let delta_pct = position.net_delta.abs() / position_size;

        debug!(
            symbol = %position.symbol,
            futures_qty = %position.futures_qty,
            spot_qty = %position.spot_qty,
            net_delta = %position.net_delta,
            delta_pct = %delta_pct,
            "Analyzing position delta"
        );

        // Check if funding direction has reversed
        let was_positive_funding = position.futures_qty < Decimal::ZERO; // Short futures = positive funding
        let current_funding_direction = if current_funding_rate > Decimal::ZERO {
            FundingDirection::Positive
        } else {
            FundingDirection::Negative
        };
        let expected_direction = if was_positive_funding {
            FundingDirection::Positive
        } else {
            FundingDirection::Negative
        };

        // Check if funding rate has flipped significantly
        if self.config.auto_flip_on_reversal
            && current_funding_direction != expected_direction
            && current_funding_rate.abs() > dec!(0.0001) // Only flip if new rate is meaningful
        {
            warn!(
                symbol = %position.symbol,
                old_direction = ?expected_direction,
                new_direction = ?current_funding_direction,
                funding_rate = %current_funding_rate,
                "Funding direction reversed - consider flipping position"
            );
            return RebalanceAction::FlipPosition {
                symbol: position.symbol.clone(),
                new_funding_direction: current_funding_direction,
            };
        }

        // Check if delta drift exceeds threshold
        if delta_pct <= self.config.max_delta_drift {
            return RebalanceAction::None;
        }

        // Determine which leg to adjust
        // We prefer adjusting the smaller leg to minimize transaction costs
        let delta_value = position.net_delta.abs() * current_price;
        if delta_value < self.config.min_rebalance_size {
            debug!(
                symbol = %position.symbol,
                delta_value = %delta_value,
                "Delta too small to rebalance"
            );
            return RebalanceAction::None;
        }

        // If net_delta > 0, we have more long exposure than short
        // Need to either sell spot (if long spot) or sell futures (if long futures)
        if position.net_delta > Decimal::ZERO {
            // We're net long, need to reduce
            if position.spot_qty > Decimal::ZERO {
                // Long spot, sell some
                RebalanceAction::AdjustSpot {
                    symbol: position.spot_symbol.clone(),
                    side: OrderSide::Sell,
                    quantity: position.net_delta,
                }
            } else {
                // Long futures, sell some
                RebalanceAction::AdjustFutures {
                    symbol: position.symbol.clone(),
                    side: OrderSide::Sell,
                    quantity: position.net_delta,
                }
            }
        } else {
            // We're net short, need to buy
            if position.spot_qty < Decimal::ZERO {
                // Short spot, buy some back
                RebalanceAction::AdjustSpot {
                    symbol: position.spot_symbol.clone(),
                    side: OrderSide::Buy,
                    quantity: position.net_delta.abs(),
                }
            } else {
                // Short futures, buy some back
                RebalanceAction::AdjustFutures {
                    symbol: position.symbol.clone(),
                    side: OrderSide::Buy,
                    quantity: position.net_delta.abs(),
                }
            }
        }
    }

    /// Execute a rebalancing action.
    pub async fn execute_rebalance(
        &self,
        client: &BinanceClient,
        action: &RebalanceAction,
    ) -> Result<RebalanceResult> {
        match action {
            RebalanceAction::None => Ok(RebalanceResult {
                symbol: String::new(),
                action_taken: RebalanceAction::None,
                order: None,
                new_delta: Decimal::ZERO,
                success: true,
                error: None,
            }),

            RebalanceAction::AdjustSpot { symbol, side, quantity } => {
                info!(
                    %symbol,
                    side = ?side,
                    %quantity,
                    "Executing spot rebalance"
                );

                let order = MarginOrder {
                    symbol: symbol.clone(),
                    side: *side,
                    order_type: OrderType::Market,
                    quantity: Some(*quantity),
                    price: None,
                    time_in_force: None,
                    is_isolated: Some(false),
                    side_effect_type: Some(SideEffectType::AutoBorrowRepay),
                };

                match client.place_margin_order(&order).await {
                    Ok(response) => Ok(RebalanceResult {
                        symbol: symbol.clone(),
                        action_taken: action.clone(),
                        order: Some(response),
                        new_delta: Decimal::ZERO, // Would need to refetch to confirm
                        success: true,
                        error: None,
                    }),
                    Err(e) => Ok(RebalanceResult {
                        symbol: symbol.clone(),
                        action_taken: action.clone(),
                        order: None,
                        new_delta: Decimal::ZERO,
                        success: false,
                        error: Some(e.to_string()),
                    }),
                }
            }

            RebalanceAction::AdjustFutures { symbol, side, quantity } => {
                info!(
                    %symbol,
                    side = ?side,
                    %quantity,
                    "Executing futures rebalance"
                );

                let order = NewOrder {
                    symbol: symbol.clone(),
                    side: *side,
                    position_side: None,
                    order_type: OrderType::Market,
                    quantity: Some(*quantity),
                    price: None,
                    time_in_force: None,
                    reduce_only: Some(true), // Reducing position, not adding
                    new_client_order_id: None,
                };

                match client.place_futures_order(&order).await {
                    Ok(response) => Ok(RebalanceResult {
                        symbol: symbol.clone(),
                        action_taken: action.clone(),
                        order: Some(response),
                        new_delta: Decimal::ZERO,
                        success: true,
                        error: None,
                    }),
                    Err(e) => Ok(RebalanceResult {
                        symbol: symbol.clone(),
                        action_taken: action.clone(),
                        order: None,
                        new_delta: Decimal::ZERO,
                        success: false,
                        error: Some(e.to_string()),
                    }),
                }
            }

            RebalanceAction::FlipPosition { symbol, new_funding_direction } => {
                warn!(
                    %symbol,
                    direction = ?new_funding_direction,
                    "Position flip not yet implemented - manual intervention required"
                );
                // Position flipping is complex: need to close both legs and re-enter opposite
                // This should be done carefully to minimize execution risk
                Ok(RebalanceResult {
                    symbol: symbol.clone(),
                    action_taken: action.clone(),
                    order: None,
                    new_delta: Decimal::ZERO,
                    success: false,
                    error: Some("Position flip requires manual intervention".to_string()),
                })
            }

            RebalanceAction::ClosePosition { symbol } => {
                warn!(
                    %symbol,
                    "Position close not yet implemented - manual intervention required"
                );
                Ok(RebalanceResult {
                    symbol: symbol.clone(),
                    action_taken: action.clone(),
                    order: None,
                    new_delta: Decimal::ZERO,
                    success: false,
                    error: Some("Position close requires manual intervention".to_string()),
                })
            }
        }
    }

    /// Check all positions and rebalance as needed.
    pub async fn check_and_rebalance(
        &self,
        client: &BinanceClient,
        positions: &[DeltaNeutralPosition],
        funding_rates: &std::collections::HashMap<String, Decimal>,
        prices: &std::collections::HashMap<String, Decimal>,
    ) -> Vec<RebalanceResult> {
        let mut results = Vec::new();

        for position in positions {
            let funding_rate = funding_rates
                .get(&position.symbol)
                .copied()
                .unwrap_or(Decimal::ZERO);
            let price = prices
                .get(&position.symbol)
                .copied()
                .unwrap_or(Decimal::ZERO);

            if price == Decimal::ZERO {
                continue;
            }

            let action = self.analyze_position(position, funding_rate, price);

            if !matches!(action, RebalanceAction::None) {
                match self.execute_rebalance(client, &action).await {
                    Ok(result) => results.push(result),
                    Err(e) => {
                        results.push(RebalanceResult {
                            symbol: position.symbol.clone(),
                            action_taken: action,
                            order: None,
                            new_delta: position.net_delta,
                            success: false,
                            error: Some(e.to_string()),
                        });
                    }
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_position(
        symbol: &str,
        futures_qty: Decimal,
        spot_qty: Decimal,
    ) -> DeltaNeutralPosition {
        DeltaNeutralPosition {
            symbol: symbol.to_string(),
            spot_symbol: symbol.to_string(),
            base_asset: symbol.strip_suffix("USDT").unwrap_or("BTC").to_string(),
            futures_qty,
            futures_entry_price: dec!(50000),
            spot_qty,
            spot_entry_price: dec!(50000),
            net_delta: futures_qty + spot_qty, // Simplified: positive = long exposure
            borrowed_amount: if spot_qty < Decimal::ZERO { spot_qty.abs() } else { Decimal::ZERO },
            funding_pnl: Decimal::ZERO,
            interest_paid: Decimal::ZERO,
        }
    }

    #[test]
    fn test_no_rebalance_when_delta_neutral() {
        let rebalancer = HedgeRebalancer::new(RebalanceConfig::default());

        // Perfect hedge: short 1 futures, long 1 spot
        let position = test_position("BTCUSDT", dec!(-1), dec!(1));

        let action = rebalancer.analyze_position(&position, dec!(0.0005), dec!(50000));
        assert!(matches!(action, RebalanceAction::None));
    }

    #[test]
    fn test_rebalance_when_drift_exceeds_threshold() {
        let rebalancer = HedgeRebalancer::new(RebalanceConfig {
            max_delta_drift: dec!(0.03),
            min_rebalance_size: dec!(100),
            auto_flip_on_reversal: true,
        });

        // 5% drift: short 1 futures, long 1.05 spot
        let position = test_position("BTCUSDT", dec!(-1), dec!(1.05));

        let action = rebalancer.analyze_position(&position, dec!(0.0005), dec!(50000));

        // Should suggest selling spot to reduce long exposure
        match action {
            RebalanceAction::AdjustSpot { side, .. } => {
                assert_eq!(side, OrderSide::Sell);
            }
            _ => panic!("Expected AdjustSpot action"),
        }
    }
}
