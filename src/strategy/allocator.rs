//! Capital allocation logic for position sizing.

use crate::config::{CapitalConfig, RiskConfig};
use crate::exchange::QualifiedPair;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::debug;

/// Target allocation for a single position.
#[derive(Debug, Clone)]
pub struct PositionAllocation {
    /// Futures symbol (e.g., "BTCUSDT")
    pub symbol: String,
    /// Corresponding spot symbol for hedging
    pub spot_symbol: String,
    /// Base asset (e.g., "BTC")
    pub base_asset: String,
    /// Target position size in USDT
    pub target_size_usdt: Decimal,
    /// Leverage to use for futures
    pub leverage: u8,
    /// Current funding rate (positive = we receive when short)
    pub funding_rate: Decimal,
    /// Priority rank (1 = highest)
    pub priority: u8,
}

/// Manages capital allocation across multiple positions.
pub struct CapitalAllocator {
    capital_config: CapitalConfig,
    risk_config: RiskConfig,
    default_leverage: u8,
}

impl CapitalAllocator {
    /// Create a new capital allocator.
    pub fn new(
        capital_config: CapitalConfig,
        risk_config: RiskConfig,
        default_leverage: u8,
    ) -> Self {
        Self {
            capital_config,
            risk_config,
            default_leverage,
        }
    }

    /// Calculate optimal allocation for qualified pairs.
    ///
    /// # Arguments
    /// * `pairs` - Qualified pairs sorted by score (best first)
    /// * `total_equity` - Total account equity in USDT
    /// * `current_positions` - Map of symbol to current position size
    ///
    /// # Returns
    /// Vector of position allocations to achieve
    pub fn calculate_allocation(
        &self,
        pairs: &[QualifiedPair],
        total_equity: Decimal,
        current_positions: &HashMap<String, Decimal>,
    ) -> Vec<PositionAllocation> {
        let deployable_capital =
            total_equity * self.capital_config.max_utilization;
        let max_per_position =
            total_equity * self.risk_config.max_single_position;

        debug!(
            %total_equity,
            %deployable_capital,
            %max_per_position,
            "Calculating allocation"
        );

        let mut allocations = Vec::new();
        let mut allocated = Decimal::ZERO;

        for (idx, pair) in pairs.iter().enumerate() {
            // Stop if we've allocated enough capital
            if allocated >= deployable_capital {
                break;
            }

            // Calculate target size based on score and remaining capital
            let remaining = deployable_capital - allocated;
            let score_weight = self.score_to_weight(pair.score, idx);
            let target_size = (remaining * score_weight)
                .min(max_per_position)
                .max(self.capital_config.min_position_size);

            // Skip if target is below minimum
            if target_size < self.capital_config.min_position_size {
                continue;
            }

            // Check if we already have this position
            let current = current_positions
                .get(&pair.symbol)
                .copied()
                .unwrap_or(Decimal::ZERO)
                .abs();

            // Skip if position is already optimal (within 5%)
            let diff_ratio = if current > Decimal::ZERO {
                ((target_size - current) / current).abs()
            } else {
                Decimal::ONE
            };

            if diff_ratio < dec!(0.05) {
                allocated += current;
                continue;
            }

            allocations.push(PositionAllocation {
                symbol: pair.symbol.clone(),
                spot_symbol: pair.spot_symbol.clone(),
                base_asset: pair.base_asset.clone(),
                target_size_usdt: target_size,
                leverage: self.default_leverage,
                funding_rate: pair.funding_rate,
                priority: (idx + 1) as u8,
            });

            allocated += target_size;
        }

        allocations
    }

    /// Convert pair score to allocation weight.
    fn score_to_weight(&self, score: Decimal, rank: usize) -> Decimal {
        // Higher ranked pairs get larger allocations
        // Top pair: ~30%, second: ~25%, third: ~20%, etc.
        let base_weight = match rank {
            0 => dec!(0.30),
            1 => dec!(0.25),
            2 => dec!(0.20),
            3 => dec!(0.15),
            _ => dec!(0.10),
        };

        // Adjust by score (normalized around 1.0)
        let score_factor = (score / dec!(10)).min(dec!(1.5));
        base_weight * score_factor
    }

    /// Calculate the maximum safe position size given margin constraints.
    pub fn max_safe_position(
        &self,
        available_margin: Decimal,
        leverage: u8,
        margin_ratio_target: Decimal,
    ) -> Decimal {
        // Position = (Margin × Leverage) / MarginRatioTarget
        // This ensures we maintain the target margin ratio
        let leverage_dec = Decimal::from(leverage);
        (available_margin * leverage_dec) / margin_ratio_target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_allocator() -> CapitalAllocator {
        CapitalAllocator::new(
            CapitalConfig {
                max_utilization: dec!(0.85),
                reserve_buffer: dec!(0.10),
                min_position_size: dec!(1000),
            },
            RiskConfig {
                max_drawdown: dec!(0.05),
                min_margin_ratio: dec!(3),
                max_single_position: dec!(0.30),
                max_unprofitable_hours: 48,
                min_expected_yield: dec!(0.10),
                grace_period_hours: 8,
                max_funding_deviation: dec!(0.20),
                max_errors_per_minute: 10,
                max_consecutive_failures: 3,
                emergency_delta_drift: dec!(0.10),
            },
            5,
        )
    }

    fn test_pair(symbol: &str, funding_rate: Decimal, score: Decimal) -> QualifiedPair {
        let base_asset = symbol.strip_suffix("USDT").unwrap_or(symbol).to_string();
        QualifiedPair {
            symbol: symbol.to_string(),
            spot_symbol: symbol.to_string(),
            base_asset,
            funding_rate,
            volume_24h: dec!(1_000_000_000),
            spread: dec!(0.0001),
            open_interest: dec!(500_000_000),
            margin_available: true,
            borrow_rate: Some(dec!(0.0001)),
            score,
        }
    }

    #[test]
    fn test_allocation_respects_max_utilization() {
        let allocator = test_allocator();
        let pairs = vec![
            test_pair("BTCUSDT", dec!(0.001), dec!(15)),
            test_pair("ETHUSDT", dec!(0.0008), dec!(12)),
        ];

        let allocations = allocator.calculate_allocation(
            &pairs,
            dec!(100_000),
            &HashMap::new(),
        );

        let total_allocated: Decimal = allocations
            .iter()
            .map(|a| a.target_size_usdt)
            .sum();

        assert!(total_allocated <= dec!(85_000)); // 85% max utilization
    }

    #[test]
    fn test_allocation_respects_max_single_position() {
        let allocator = test_allocator();
        let pairs = vec![test_pair("BTCUSDT", dec!(0.01), dec!(100))];

        let allocations = allocator.calculate_allocation(
            &pairs,
            dec!(100_000),
            &HashMap::new(),
        );

        // Even with high score, should be capped at 30%
        assert!(allocations[0].target_size_usdt <= dec!(30_000));
    }
}
