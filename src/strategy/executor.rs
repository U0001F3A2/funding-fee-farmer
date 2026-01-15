//! Order execution and position management.

use crate::config::ExecutionConfig;
use crate::exchange::{
    BinanceClient, MarginType, NewOrder, OrderResponse, OrderSide, OrderStatus,
    OrderType, TimeInForce,
};
use crate::strategy::allocator::PositionAllocation;
use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use std::time::Duration;
use tracing::{error, info, warn};

/// Handles order execution for funding fee farming positions.
pub struct OrderExecutor {
    config: ExecutionConfig,
}

/// Result of a position entry attempt.
#[derive(Debug)]
pub struct EntryResult {
    pub symbol: String,
    pub spot_order: Option<OrderResponse>,
    pub futures_order: Option<OrderResponse>,
    pub success: bool,
    pub error: Option<String>,
}

impl OrderExecutor {
    /// Create a new order executor.
    pub fn new(config: ExecutionConfig) -> Self {
        Self { config }
    }

    /// Execute a delta-neutral entry (spot + futures hedge).
    ///
    /// For positive funding: Long spot + Short futures
    /// For negative funding: Short spot (margin) + Long futures
    pub async fn enter_position(
        &self,
        client: &BinanceClient,
        allocation: &PositionAllocation,
        current_price: Decimal,
    ) -> Result<EntryResult> {
        let symbol = &allocation.symbol;
        let is_positive_funding = allocation.funding_rate > Decimal::ZERO;

        info!(
            %symbol,
            target_size = %allocation.target_size_usdt,
            funding_rate = %allocation.funding_rate,
            positive = is_positive_funding,
            "Entering position"
        );

        // Set up futures account for this symbol
        self.prepare_futures_symbol(client, symbol, allocation.leverage)
            .await?;

        // Calculate quantity based on price
        let quantity = allocation.target_size_usdt / current_price;
        let quantity = self.round_quantity(quantity, symbol);

        // Determine order sides based on funding direction
        let (_spot_side, futures_side) = if is_positive_funding {
            (OrderSide::Buy, OrderSide::Sell) // Long spot, short futures
        } else {
            (OrderSide::Sell, OrderSide::Buy) // Short spot (margin), long futures
        };

        // Execute futures order first (more critical for funding capture)
        let futures_order = self
            .place_order_with_retry(
                client,
                symbol,
                futures_side,
                OrderType::Market,
                quantity,
                None,
                3,
            )
            .await;

        match futures_order {
            Ok(order) if order.status == OrderStatus::Filled => {
                info!(
                    %symbol,
                    order_id = order.order_id,
                    filled_qty = %order.executed_qty,
                    avg_price = %order.avg_price,
                    "Futures order filled"
                );

                // TODO: Execute spot hedge
                // For now, we only implement futures side
                // Spot hedging requires margin account setup

                Ok(EntryResult {
                    symbol: symbol.clone(),
                    spot_order: None, // TODO: Implement spot leg
                    futures_order: Some(order),
                    success: true,
                    error: None,
                })
            }
            Ok(order) => {
                let status = order.status;
                warn!(
                    %symbol,
                    status = ?status,
                    "Futures order not fully filled"
                );
                Ok(EntryResult {
                    symbol: symbol.clone(),
                    spot_order: None,
                    futures_order: Some(order),
                    success: false,
                    error: Some(format!("Order status: {:?}", status)),
                })
            }
            Err(e) => {
                error!(%symbol, error = %e, "Failed to place futures order");
                Ok(EntryResult {
                    symbol: symbol.clone(),
                    spot_order: None,
                    futures_order: None,
                    success: false,
                    error: Some(e.to_string()),
                })
            }
        }
    }

    /// Exit an existing position.
    pub async fn exit_position(
        &self,
        client: &BinanceClient,
        symbol: &str,
        current_position: Decimal,
    ) -> Result<OrderResponse> {
        let side = if current_position > Decimal::ZERO {
            OrderSide::Sell // Close long
        } else {
            OrderSide::Buy // Close short
        };

        let quantity = current_position.abs();

        info!(
            %symbol,
            %quantity,
            side = ?side,
            "Exiting position"
        );

        self.place_order_with_retry(
            client,
            symbol,
            side,
            OrderType::Market,
            quantity,
            None,
            3,
        )
        .await
    }

    /// Prepare futures symbol (set leverage and margin type).
    async fn prepare_futures_symbol(
        &self,
        client: &BinanceClient,
        symbol: &str,
        leverage: u8,
    ) -> Result<()> {
        // Set cross margin (more capital efficient)
        client
            .set_margin_type(symbol, MarginType::Cross)
            .await
            .ok(); // Ignore error if already set

        // Set leverage
        client.set_leverage(symbol, leverage).await?;

        Ok(())
    }

    /// Place an order with retry logic.
    async fn place_order_with_retry(
        &self,
        client: &BinanceClient,
        symbol: &str,
        side: OrderSide,
        order_type: OrderType,
        quantity: Decimal,
        price: Option<Decimal>,
        max_retries: u8,
    ) -> Result<OrderResponse> {
        let mut last_error = None;

        for attempt in 1..=max_retries {
            let order = NewOrder {
                symbol: symbol.to_string(),
                side,
                position_side: None,
                order_type,
                quantity: Some(quantity),
                price,
                time_in_force: if order_type == OrderType::Limit {
                    Some(TimeInForce::Gtx) // Post-only
                } else {
                    None
                },
                reduce_only: None,
                new_client_order_id: None,
            };

            match client.place_futures_order(&order).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    warn!(
                        %symbol,
                        attempt,
                        max_retries,
                        error = %e,
                        "Order failed, retrying"
                    );
                    last_error = Some(e);

                    if attempt < max_retries {
                        tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("Unknown error")))
    }

    /// Round quantity to valid precision for the symbol.
    fn round_quantity(&self, quantity: Decimal, _symbol: &str) -> Decimal {
        // TODO: Get precision from exchange info
        // For now, use reasonable defaults
        quantity.round_dp(3)
    }

    /// Check if position entry should proceed based on slippage.
    pub fn check_slippage(
        &self,
        expected_price: Decimal,
        actual_price: Decimal,
    ) -> bool {
        let slippage = ((actual_price - expected_price) / expected_price).abs();
        slippage <= self.config.slippage_tolerance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_executor() -> OrderExecutor {
        OrderExecutor::new(ExecutionConfig {
            default_leverage: 5,
            max_leverage: 10,
            slippage_tolerance: dec!(0.0005),
            order_timeout_secs: 30,
        })
    }

    #[test]
    fn test_slippage_check() {
        let executor = test_executor();

        // 0.03% slippage - should pass
        assert!(executor.check_slippage(dec!(50000), dec!(50015)));

        // 0.1% slippage - should fail
        assert!(!executor.check_slippage(dec!(50000), dec!(50050)));
    }
}
