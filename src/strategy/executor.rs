//! Order execution and position management.

use crate::config::ExecutionConfig;
use crate::exchange::{
    BinanceClient, MarginOrder, MarginType, NewOrder, OrderResponse, OrderSide, OrderStatus,
    OrderType, SideEffectType, TimeInForce,
};
use crate::strategy::allocator::PositionAllocation;
use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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
    /// For positive funding: Long spot + Short futures (we receive funding)
    /// For negative funding: Short spot (margin borrow) + Long futures (we receive funding)
    pub async fn enter_position(
        &self,
        client: &BinanceClient,
        allocation: &PositionAllocation,
        current_price: Decimal,
    ) -> Result<EntryResult> {
        let symbol = &allocation.symbol;
        let spot_symbol = &allocation.spot_symbol;
        let is_positive_funding = allocation.funding_rate > Decimal::ZERO;

        info!(
            %symbol,
            %spot_symbol,
            target_size = %allocation.target_size_usdt,
            funding_rate = %allocation.funding_rate,
            positive = is_positive_funding,
            "Entering delta-neutral position"
        );

        // Set up futures account for this symbol
        self.prepare_futures_symbol(client, symbol, allocation.leverage)
            .await?;

        // Calculate quantity based on price
        let quantity = allocation.target_size_usdt / current_price;
        let quantity = self.round_quantity(quantity, symbol);

        // Determine order sides based on funding direction
        let (spot_side, futures_side) = if is_positive_funding {
            // Positive funding: Short futures earns funding, long spot as hedge
            (OrderSide::Buy, OrderSide::Sell)
        } else {
            // Negative funding: Long futures earns funding, short spot as hedge (needs borrow)
            (OrderSide::Sell, OrderSide::Buy)
        };

        // Execute futures order first (more critical for funding capture)
        let futures_result = self
            .place_futures_order_with_retry(client, symbol, futures_side, quantity, 3)
            .await;

        let futures_order = match futures_result {
            Ok(order) if order.status == OrderStatus::Filled => {
                info!(
                    %symbol,
                    order_id = order.order_id,
                    filled_qty = %order.executed_qty,
                    avg_price = %order.avg_price,
                    "Futures order filled"
                );
                Some(order)
            }
            Ok(order) => {
                let status = order.status;
                warn!(%symbol, status = ?status, "Futures order not fully filled");
                return Ok(EntryResult {
                    symbol: symbol.clone(),
                    spot_order: None,
                    futures_order: Some(order),
                    success: false,
                    error: Some(format!("Futures order status: {:?}", status)),
                });
            }
            Err(e) => {
                error!(%symbol, error = %e, "Failed to place futures order");
                return Ok(EntryResult {
                    symbol: symbol.clone(),
                    spot_order: None,
                    futures_order: None,
                    success: false,
                    error: Some(e.to_string()),
                });
            }
        };

        // Now execute spot hedge
        let actual_futures_qty = futures_order.as_ref().map(|o| o.executed_qty).unwrap_or(quantity);

        let spot_result = self
            .place_spot_margin_order(client, spot_symbol, spot_side, actual_futures_qty, is_positive_funding)
            .await;

        let spot_order = match spot_result {
            Ok(order) if order.status == OrderStatus::Filled => {
                info!(
                    %spot_symbol,
                    order_id = order.order_id,
                    filled_qty = %order.executed_qty,
                    avg_price = %order.avg_price,
                    "Spot margin order filled - delta neutral achieved"
                );
                Some(order)
            }
            Ok(order) => {
                let status = order.status;
                warn!(%spot_symbol, status = ?status, "Spot order not fully filled - position may be unhedged!");
                Some(order)
            }
            Err(e) => {
                error!(%spot_symbol, error = %e, "Failed to place spot hedge order - UNWINDING FUTURES");
                // Critical: Spot leg failed, need to unwind futures to avoid naked exposure
                if let Some(ref f_order) = futures_order {
                    let unwind_side = if futures_side == OrderSide::Buy {
                        OrderSide::Sell
                    } else {
                        OrderSide::Buy
                    };
                    if let Err(unwind_err) = self
                        .place_futures_order_with_retry(client, symbol, unwind_side, f_order.executed_qty, 3)
                        .await
                    {
                        error!(%symbol, error = %unwind_err, "CRITICAL: Failed to unwind futures position!");
                    }
                }
                return Ok(EntryResult {
                    symbol: symbol.clone(),
                    spot_order: None,
                    futures_order,
                    success: false,
                    error: Some(format!("Spot hedge failed: {}", e)),
                });
            }
        };

        // Verify delta neutrality
        let futures_qty = futures_order.as_ref().map(|o| o.executed_qty).unwrap_or(dec!(0));
        let spot_qty = spot_order.as_ref().map(|o| o.executed_qty).unwrap_or(dec!(0));
        let delta_diff = (futures_qty - spot_qty).abs();
        let delta_pct = if futures_qty > dec!(0) {
            delta_diff / futures_qty * dec!(100)
        } else {
            dec!(0)
        };

        if delta_pct > dec!(1) {
            warn!(
                %symbol,
                futures_qty = %futures_qty,
                spot_qty = %spot_qty,
                delta_diff_pct = %delta_pct,
                "Delta mismatch > 1% - position partially hedged"
            );
        }

        Ok(EntryResult {
            symbol: symbol.clone(),
            spot_order,
            futures_order,
            success: delta_pct <= dec!(5), // Allow up to 5% mismatch
            error: if delta_pct > dec!(5) {
                Some(format!("Delta mismatch: {:.2}%", delta_pct))
            } else {
                None
            },
        })
    }

    /// Place a spot margin order for hedging.
    async fn place_spot_margin_order(
        &self,
        client: &BinanceClient,
        symbol: &str,
        side: OrderSide,
        quantity: Decimal,
        is_positive_funding: bool,
    ) -> Result<OrderResponse> {
        // For positive funding (buying spot): NO_SIDE_EFFECT (normal buy)
        // For negative funding (selling spot): MARGIN_BUY to auto-borrow the asset
        let side_effect = if is_positive_funding {
            SideEffectType::NoSideEffect
        } else {
            // Shorting spot requires borrowing the base asset first
            SideEffectType::MarginBuy
        };

        let order = MarginOrder {
            symbol: symbol.to_string(),
            side,
            order_type: OrderType::Market,
            quantity: Some(quantity),
            price: None,
            time_in_force: None,
            is_isolated: Some(false), // Cross margin for capital efficiency
            side_effect_type: Some(side_effect),
        };

        client.place_margin_order(&order).await
    }

    /// Place a futures order with retry logic.
    async fn place_futures_order_with_retry(
        &self,
        client: &BinanceClient,
        symbol: &str,
        side: OrderSide,
        quantity: Decimal,
        max_retries: u8,
    ) -> Result<OrderResponse> {
        self.place_order_with_retry(client, symbol, side, OrderType::Market, quantity, None, max_retries)
            .await
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
