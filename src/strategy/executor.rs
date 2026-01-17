//! Order execution and position management.

use crate::config::ExecutionConfig;
use crate::exchange::{
    BinanceClient, MarginOrder, MarginType, NewOrder, OrderResponse, OrderSide, OrderStatus,
    OrderType, SideEffectType, TimeInForce,
};
use crate::strategy::allocator::{PositionAllocation, PositionReduction};
use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Duration;
use tracing::{error, info, warn};

use std::collections::HashMap;

/// Handles order execution for funding fee farming positions.
pub struct OrderExecutor {
    config: ExecutionConfig,
    precisions: HashMap<String, u8>,
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
        Self {
            config,
            precisions: HashMap::new(),
        }
    }

    /// Update symbol precisions.
    pub fn set_precisions(&mut self, precisions: HashMap<String, u8>) {
        self.precisions = precisions;
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
        let actual_futures_qty = futures_order
            .as_ref()
            .map(|o| o.executed_qty)
            .unwrap_or(quantity);

        let spot_result = self
            .place_spot_margin_order(
                client,
                spot_symbol,
                spot_side,
                actual_futures_qty,
                is_positive_funding,
            )
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
                        .place_futures_order_with_retry(
                            client,
                            symbol,
                            unwind_side,
                            f_order.executed_qty,
                            3,
                        )
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
        let futures_qty = futures_order
            .as_ref()
            .map(|o| o.executed_qty)
            .unwrap_or(dec!(0));
        let spot_qty = spot_order
            .as_ref()
            .map(|o| o.executed_qty)
            .unwrap_or(dec!(0));
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
        self.place_order_with_retry(
            client,
            symbol,
            side,
            OrderType::Market,
            quantity,
            None,
            max_retries,
        )
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

        self.place_order_with_retry(client, symbol, side, OrderType::Market, quantity, None, 3)
            .await
    }

    /// Reduce an oversized position to maintain optimal allocation.
    ///
    /// This reduces both the futures and spot positions proportionally to maintain
    /// delta neutrality while freeing up capital for better opportunities.
    pub async fn reduce_position(
        &self,
        client: &BinanceClient,
        reduction: &PositionReduction,
        current_price: Decimal,
        futures_position: Decimal, // Current futures position (positive=long, negative=short)
    ) -> Result<EntryResult> {
        let symbol = &reduction.symbol;
        let spot_symbol = &reduction.spot_symbol;

        // Calculate reduction quantity
        let reduction_quantity = reduction.reduction_usdt / current_price;
        let reduction_quantity = self.round_quantity(reduction_quantity, symbol);

        if reduction_quantity <= Decimal::ZERO {
            return Ok(EntryResult {
                symbol: symbol.clone(),
                spot_order: None,
                futures_order: None,
                success: true,
                error: Some("Reduction quantity too small".to_string()),
            });
        }

        info!(
            %symbol,
            current_size = %reduction.current_size_usdt,
            target_size = %reduction.target_size_usdt,
            reduction = %reduction.reduction_usdt,
            %reduction_quantity,
            "Reducing oversized position"
        );

        // Determine the direction of reduction based on current futures position
        // If futures is short (negative), we close part of short (buy) and sell spot
        // If futures is long (positive), we close part of long (sell) and buy back spot/repay
        let is_short_futures = futures_position < Decimal::ZERO;

        // Step 1: Reduce futures position
        let futures_side = if is_short_futures {
            OrderSide::Buy // Close short
        } else {
            OrderSide::Sell // Close long
        };

        let futures_result = self
            .place_futures_order_with_retry(client, symbol, futures_side, reduction_quantity, 3)
            .await;

        let futures_order = match futures_result {
            Ok(order) => Some(order),
            Err(e) => {
                error!(%symbol, error = %e, "Failed to reduce futures position");
                return Ok(EntryResult {
                    symbol: symbol.clone(),
                    spot_order: None,
                    futures_order: None,
                    success: false,
                    error: Some(format!("Futures reduction failed: {}", e)),
                });
            }
        };

        // Step 2: Reduce spot position (opposite side of futures)
        let spot_side = if is_short_futures {
            // Was long spot to hedge short futures, sell spot
            OrderSide::Sell
        } else {
            // Was short spot (margin) to hedge long futures, buy to repay
            OrderSide::Buy
        };

        let side_effect = if is_short_futures {
            // Selling spot normally
            SideEffectType::NoSideEffect
        } else {
            // Buying to repay margin borrow
            SideEffectType::AutoRepay
        };

        let spot_order = MarginOrder {
            symbol: spot_symbol.clone(),
            side: spot_side,
            order_type: OrderType::Market,
            quantity: Some(reduction_quantity),
            price: None,
            time_in_force: None,
            is_isolated: Some(false),
            side_effect_type: Some(side_effect),
        };

        let spot_result = client.place_margin_order(&spot_order).await;

        let spot_order_response = match spot_result {
            Ok(order) => Some(order),
            Err(e) => {
                // Log warning but don't fail - futures already reduced
                warn!(
                    %symbol,
                    error = %e,
                    "Spot reduction failed - position may have delta drift"
                );
                None
            }
        };

        let success = futures_order.is_some();

        info!(
            %symbol,
            futures_success = futures_order.is_some(),
            spot_success = spot_order_response.is_some(),
            %reduction_quantity,
            "Position reduction complete"
        );

        Ok(EntryResult {
            symbol: symbol.clone(),
            spot_order: spot_order_response,
            futures_order,
            success,
            error: None,
        })
    }

    /// Prepare futures symbol (set leverage and margin type).
    async fn prepare_futures_symbol(
        &self,
        client: &BinanceClient,
        symbol: &str,
        leverage: u8,
    ) -> Result<()> {
        // Set cross margin (more capital efficient)
        client.set_margin_type(symbol, MarginType::Cross).await.ok(); // Ignore error if already set

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
    fn round_quantity(&self, quantity: Decimal, symbol: &str) -> Decimal {
        let precision = self.precisions.get(symbol).copied().unwrap_or(3);
        quantity.round_dp(precision as u32)
    }

    /// Check if position entry should proceed based on slippage.
    pub fn check_slippage(&self, expected_price: Decimal, actual_price: Decimal) -> bool {
        let slippage = ((actual_price - expected_price) / expected_price).abs();
        slippage <= self.config.slippage_tolerance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Test Helpers
    // =========================================================================

    fn test_executor() -> OrderExecutor {
        OrderExecutor::new(ExecutionConfig {
            default_leverage: 5,
            max_leverage: 10,
            slippage_tolerance: dec!(0.0005),
            order_timeout_secs: 30,
        })
    }

    fn test_allocation(symbol: &str, funding_rate: Decimal, size: Decimal) -> PositionAllocation {
        PositionAllocation {
            symbol: symbol.to_string(),
            spot_symbol: symbol.to_string(),
            base_asset: symbol.strip_suffix("USDT").unwrap_or(symbol).to_string(),
            target_size_usdt: size,
            leverage: 5,
            funding_rate,
            priority: 1,
        }
    }

    // =========================================================================
    // Slippage Tests
    // =========================================================================

    #[test]
    fn test_slippage_check_passes_within_tolerance() {
        let executor = test_executor();

        // 0.03% slippage - should pass (tolerance is 0.05%)
        assert!(executor.check_slippage(dec!(50000), dec!(50015)));
    }

    #[test]
    fn test_slippage_check_fails_above_tolerance() {
        let executor = test_executor();

        // 0.1% slippage - should fail
        assert!(!executor.check_slippage(dec!(50000), dec!(50050)));
    }

    #[test]
    fn test_slippage_check_negative_slippage() {
        let executor = test_executor();

        // Negative slippage (price dropped) - still checks absolute value
        // 0.02% below expected - should pass
        assert!(executor.check_slippage(dec!(50000), dec!(49990)));
    }

    #[test]
    fn test_slippage_check_exact_tolerance() {
        let executor = test_executor();

        // Exactly at tolerance (0.05%)
        let tolerance_price = dec!(50000) * (dec!(1) + dec!(0.0005));
        assert!(executor.check_slippage(dec!(50000), tolerance_price));
    }

    #[test]
    fn test_slippage_check_zero_slippage() {
        let executor = test_executor();

        // No slippage
        assert!(executor.check_slippage(dec!(50000), dec!(50000)));
    }

    // =========================================================================
    // Quantity Rounding Tests
    // =========================================================================

    #[test]
    fn test_round_quantity_default_precision() {
        let executor = test_executor();

        // No precision set, defaults to 3
        let rounded = executor.round_quantity(dec!(1.23456789), "BTCUSDT");
        assert_eq!(rounded, dec!(1.235));
    }

    #[test]
    fn test_round_quantity_custom_precision() {
        let mut executor = test_executor();
        let mut precisions = HashMap::new();
        precisions.insert("BTCUSDT".to_string(), 5);
        executor.set_precisions(precisions);

        let rounded = executor.round_quantity(dec!(1.23456789), "BTCUSDT");
        assert_eq!(rounded, dec!(1.23457));
    }

    #[test]
    fn test_round_quantity_zero_precision() {
        let mut executor = test_executor();
        let mut precisions = HashMap::new();
        precisions.insert("BTCUSDT".to_string(), 0);
        executor.set_precisions(precisions);

        let rounded = executor.round_quantity(dec!(1.9), "BTCUSDT");
        assert_eq!(rounded, dec!(2));
    }

    #[test]
    fn test_round_quantity_different_symbols() {
        let mut executor = test_executor();
        let mut precisions = HashMap::new();
        precisions.insert("BTCUSDT".to_string(), 5);
        precisions.insert("ETHUSDT".to_string(), 4);
        precisions.insert("SOLUSDT".to_string(), 2);
        executor.set_precisions(precisions);

        assert_eq!(
            executor.round_quantity(dec!(0.123456), "BTCUSDT"),
            dec!(0.12346)
        );
        assert_eq!(
            executor.round_quantity(dec!(0.123456), "ETHUSDT"),
            dec!(0.1235)
        );
        assert_eq!(
            executor.round_quantity(dec!(0.123456), "SOLUSDT"),
            dec!(0.12)
        );
    }

    // =========================================================================
    // Entry Result Tests
    // =========================================================================

    #[test]
    fn test_entry_result_success_fields() {
        let result = EntryResult {
            symbol: "BTCUSDT".to_string(),
            spot_order: None,
            futures_order: None,
            success: true,
            error: None,
        };

        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_entry_result_failure_with_error() {
        let result = EntryResult {
            symbol: "BTCUSDT".to_string(),
            spot_order: None,
            futures_order: None,
            success: false,
            error: Some("Test error".to_string()),
        };

        assert!(!result.success);
        assert!(result.error.is_some());
        assert_eq!(result.error.unwrap(), "Test error");
    }

    // =========================================================================
    // Allocation Tests (Position Side Logic)
    // =========================================================================

    #[test]
    fn test_positive_funding_determines_short_futures() {
        let alloc = test_allocation("BTCUSDT", dec!(0.001), dec!(10000));

        // Positive funding: short futures earns funding
        assert!(alloc.funding_rate > Decimal::ZERO);

        // In the executor, positive funding means:
        // spot_side = Buy, futures_side = Sell
        let is_positive = alloc.funding_rate > Decimal::ZERO;
        assert!(is_positive);
    }

    #[test]
    fn test_negative_funding_determines_long_futures() {
        let alloc = test_allocation("BTCUSDT", dec!(-0.001), dec!(10000));

        // Negative funding: long futures earns funding
        assert!(alloc.funding_rate < Decimal::ZERO);

        // In the executor, negative funding means:
        // spot_side = Sell, futures_side = Buy
        let is_positive = alloc.funding_rate > Decimal::ZERO;
        assert!(!is_positive);
    }

    // =========================================================================
    // Delta Calculation Tests
    // =========================================================================

    #[test]
    fn test_delta_mismatch_calculation() {
        // Simulate delta check from enter_position
        let futures_qty = dec!(1.0);
        let spot_qty = dec!(0.95); // 5% difference

        let delta_diff = (futures_qty - spot_qty).abs();
        let delta_pct = if futures_qty > dec!(0) {
            delta_diff / futures_qty * dec!(100)
        } else {
            dec!(0)
        };

        assert_eq!(delta_pct, dec!(5));

        // 5% is at the threshold - should still succeed
        let success = delta_pct <= dec!(5);
        assert!(success);
    }

    #[test]
    fn test_delta_mismatch_above_threshold() {
        let futures_qty = dec!(1.0);
        let spot_qty = dec!(0.90); // 10% difference

        let delta_diff = (futures_qty - spot_qty).abs();
        let delta_pct = delta_diff / futures_qty * dec!(100);

        assert_eq!(delta_pct, dec!(10));

        // 10% exceeds 5% threshold - should fail
        let success = delta_pct <= dec!(5);
        assert!(!success);
    }

    #[test]
    fn test_delta_perfect_match() {
        let futures_qty = dec!(1.0);
        let spot_qty = dec!(1.0);

        let delta_diff = (futures_qty - spot_qty).abs();
        let delta_pct = delta_diff / futures_qty * dec!(100);

        assert_eq!(delta_pct, Decimal::ZERO);
        assert!(delta_pct <= dec!(5));
    }

    #[test]
    fn test_delta_warning_threshold() {
        // The code warns at > 1% but allows up to 5%
        let futures_qty = dec!(1.0);
        let spot_qty = dec!(0.98); // 2% difference

        let delta_diff = (futures_qty - spot_qty).abs();
        let delta_pct = delta_diff / futures_qty * dec!(100);

        assert_eq!(delta_pct, dec!(2));

        // Exceeds warning threshold (1%) but within success threshold (5%)
        let warn = delta_pct > dec!(1);
        let success = delta_pct <= dec!(5);

        assert!(warn);
        assert!(success);
    }

    // =========================================================================
    // Executor Config Tests
    // =========================================================================

    #[test]
    fn test_executor_creation() {
        let config = ExecutionConfig {
            default_leverage: 5,
            max_leverage: 10,
            slippage_tolerance: dec!(0.001),
            order_timeout_secs: 60,
        };

        let executor = OrderExecutor::new(config);
        // Executor created successfully
        assert!(executor.precisions.is_empty());
    }

    #[test]
    fn test_set_precisions() {
        let mut executor = test_executor();

        let mut precisions = HashMap::new();
        precisions.insert("BTCUSDT".to_string(), 5);
        precisions.insert("ETHUSDT".to_string(), 4);

        executor.set_precisions(precisions.clone());

        assert_eq!(executor.precisions.len(), 2);
        assert_eq!(executor.precisions.get("BTCUSDT"), Some(&5u8));
        assert_eq!(executor.precisions.get("ETHUSDT"), Some(&4u8));
    }
}
