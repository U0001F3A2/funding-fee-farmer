//! Mock trading client for paper trading / backtesting.

use super::types::*;
use crate::persistence::{PersistedPosition, PersistedState};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Simulated position state with per-position tracking.
#[derive(Debug, Clone)]
pub struct MockPosition {
    pub symbol: String,
    pub futures_qty: Decimal,
    pub futures_entry_price: Decimal,
    pub spot_qty: Decimal,
    pub spot_entry_price: Decimal,
    pub borrowed_amount: Decimal,
    // Per-position lifecycle tracking
    /// When the position was opened
    pub opened_at: DateTime<Utc>,
    /// Total funding received for this position
    pub total_funding_received: Decimal,
    /// Total interest paid for this position (margin borrowing)
    pub total_interest_paid: Decimal,
    /// Number of funding collections for this position
    pub funding_collections: u32,
    /// Expected funding rate at position entry (for anomaly detection)
    pub expected_funding_rate: Decimal,
}

impl Default for MockPosition {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            futures_qty: Decimal::ZERO,
            futures_entry_price: Decimal::ZERO,
            spot_qty: Decimal::ZERO,
            spot_entry_price: Decimal::ZERO,
            borrowed_amount: Decimal::ZERO,
            opened_at: Utc::now(),
            total_funding_received: Decimal::ZERO,
            total_interest_paid: Decimal::ZERO,
            funding_collections: 0,
            expected_funding_rate: Decimal::ZERO,
        }
    }
}

/// Mock trading state for paper trading.
#[derive(Debug)]
pub struct MockTradingState {
    pub initial_balance: Decimal,
    pub balance: Decimal,
    pub positions: HashMap<String, MockPosition>,
    pub total_funding_received: Decimal,
    pub total_trading_fees: Decimal,
    pub total_borrow_interest: Decimal,
    pub order_count: u64,
}

impl Default for MockTradingState {
    fn default() -> Self {
        Self {
            initial_balance: dec!(10000),
            balance: dec!(10000),
            positions: HashMap::new(),
            total_funding_received: Decimal::ZERO,
            total_trading_fees: Decimal::ZERO,
            total_borrow_interest: Decimal::ZERO,
            order_count: 0,
        }
    }
}

/// Mock client that simulates Binance API responses.
pub struct MockBinanceClient {
    state: Arc<RwLock<MockTradingState>>,
    order_id_counter: AtomicU64,
    /// Simulated funding rates (fetched from real API or hardcoded)
    funding_rates: Arc<RwLock<HashMap<String, Decimal>>>,
    /// Simulated prices
    prices: Arc<RwLock<HashMap<String, Decimal>>>,
    /// Trading fee rate (0.04% taker)
    fee_rate: Decimal,
}

impl MockBinanceClient {
    /// Create a new mock client with initial balance.
    pub fn new(initial_balance: Decimal) -> Self {
        let mut state = MockTradingState::default();
        state.initial_balance = initial_balance;
        state.balance = initial_balance;

        Self {
            state: Arc::new(RwLock::new(state)),
            order_id_counter: AtomicU64::new(1),
            funding_rates: Arc::new(RwLock::new(HashMap::new())),
            prices: Arc::new(RwLock::new(HashMap::new())),
            fee_rate: dec!(0.0004), // 0.04% taker fee
        }
    }

    /// Update simulated market data (call this with real data).
    pub async fn update_market_data(
        &self,
        funding_rates: HashMap<String, Decimal>,
        prices: HashMap<String, Decimal>,
    ) {
        *self.funding_rates.write().await = funding_rates;
        *self.prices.write().await = prices;
    }

    /// Alias for update_market_data (used by backtesting engine).
    pub async fn set_market_data(
        &self,
        funding_rates: HashMap<String, Decimal>,
        prices: HashMap<String, Decimal>,
    ) {
        self.update_market_data(funding_rates, prices).await;
    }

    /// Reset all state for a new backtest run (parameter sweep).
    pub async fn reset(&self, initial_balance: Decimal) {
        let mut state = self.state.write().await;
        state.initial_balance = initial_balance;
        state.balance = initial_balance;
        state.positions.clear();
        state.total_funding_received = Decimal::ZERO;
        state.total_trading_fees = Decimal::ZERO;
        state.total_borrow_interest = Decimal::ZERO;
        state.order_count = 0;

        // Reset order ID counter
        self.order_id_counter.store(1, Ordering::SeqCst);

        // Clear market data
        self.funding_rates.write().await.clear();
        self.prices.write().await.clear();

        debug!(balance = %initial_balance, "Mock client state reset");
    }

    /// Get current mock state for logging.
    pub async fn get_state(&self) -> MockTradingState {
        let state = self.state.read().await;
        MockTradingState {
            initial_balance: state.initial_balance,
            balance: state.balance,
            positions: state.positions.clone(),
            total_funding_received: state.total_funding_received,
            total_trading_fees: state.total_trading_fees,
            total_borrow_interest: state.total_borrow_interest,
            order_count: state.order_count,
        }
    }

    /// Simulate funding payment collection (call every 8 hours).
    /// Collect funding payments for all positions.
    /// Returns a map of symbol -> funding received for verification purposes.
    pub async fn collect_funding(&self) -> HashMap<String, Decimal> {
        let mut state = self.state.write().await;
        let funding_rates = self.funding_rates.read().await;
        let prices = self.prices.read().await;

        let mut total_funding = Decimal::ZERO;
        let mut per_position_funding: HashMap<String, Decimal> = HashMap::new();

        // Collect symbols first to avoid borrow conflicts
        let symbols: Vec<String> = state.positions.keys().cloned().collect();

        for symbol in symbols {
            if let Some(&rate) = funding_rates.get(&symbol) {
                if let Some(&price) = prices.get(&symbol) {
                    if let Some(position) = state.positions.get_mut(&symbol) {
                        // Funding = position_value * funding_rate
                        // Short futures with positive funding = receive
                        // Long futures with negative funding = receive
                        let futures_value = position.futures_qty * price;
                        let funding = -futures_value * rate; // Negative qty (short) * positive rate = positive funding

                        total_funding += funding;

                        // Track per-position funding
                        position.total_funding_received += funding;
                        position.funding_collections += 1;
                        per_position_funding.insert(symbol.clone(), funding);

                        debug!(
                            %symbol,
                            futures_qty = %position.futures_qty,
                            funding_rate = %rate,
                            funding_received = %funding,
                            position_total_funding = %position.total_funding_received,
                            funding_collections = position.funding_collections,
                            "Funding payment"
                        );
                    }
                }
            }
        }

        state.total_funding_received += total_funding;
        state.balance += total_funding;

        info!(
            funding_this_period = %total_funding,
            total_funding = %state.total_funding_received,
            balance = %state.balance,
            "Funding collected"
        );

        per_position_funding
    }

    /// Simulate borrow interest accrual (call periodically).
    /// Returns a map of symbol -> interest paid for tracking purposes.
    pub async fn accrue_interest(&self, hours: Decimal) -> HashMap<String, Decimal> {
        let mut state = self.state.write().await;
        let hourly_rate = dec!(0.00002); // ~0.002% per hour (typical Binance rate)

        let mut total_interest = Decimal::ZERO;
        let mut per_position_interest: HashMap<String, Decimal> = HashMap::new();

        for (symbol, position) in state.positions.iter_mut() {
            if position.borrowed_amount > Decimal::ZERO {
                let interest = position.borrowed_amount * hourly_rate * hours;
                total_interest += interest;

                // Track per-position interest
                position.total_interest_paid += interest;
                per_position_interest.insert(symbol.clone(), interest);
            }
        }

        state.total_borrow_interest += total_interest;
        state.balance -= total_interest;

        if total_interest > Decimal::ZERO {
            debug!(
                interest = %total_interest,
                total_interest = %state.total_borrow_interest,
                "Interest accrued"
            );
        }

        per_position_interest
    }

    fn next_order_id(&self) -> u64 {
        self.order_id_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Simulate placing a futures order.
    pub async fn place_futures_order(&self, order: &NewOrder) -> Result<OrderResponse> {
        let mut state = self.state.write().await;
        let prices = self.prices.read().await;

        // IMPORTANT: Use entry price as fallback to avoid catastrophic fee errors
        // The old default of $50,000 would cause massive incorrect fees for low-priced assets
        let fallback_price = state
            .positions
            .get(&order.symbol)
            .map(|p| p.futures_entry_price)
            .filter(|p| *p > Decimal::ZERO)
            .unwrap_or(dec!(1)); // Last resort: $1 (much safer than $50,000)

        let price = prices.get(&order.symbol).copied().unwrap_or(fallback_price);
        let quantity = order.quantity.unwrap_or(Decimal::ZERO);
        let notional = quantity * price;
        let fee = notional * self.fee_rate;

        // Update position
        let position = state
            .positions
            .entry(order.symbol.clone())
            .or_insert_with(|| MockPosition {
                symbol: order.symbol.clone(),
                ..Default::default()
            });

        match order.side {
            OrderSide::Buy => {
                position.futures_qty += quantity;
                position.futures_entry_price = price;
            }
            OrderSide::Sell => {
                position.futures_qty -= quantity;
                position.futures_entry_price = price;
            }
        }

        state.balance -= fee;
        state.total_trading_fees += fee;
        state.order_count += 1;

        let order_id = self.next_order_id() as i64;

        info!(
            order_id,
            symbol = %order.symbol,
            side = ?order.side,
            quantity = %quantity,
            price = %price,
            fee = %fee,
            "Mock futures order executed"
        );

        Ok(OrderResponse {
            order_id,
            symbol: order.symbol.clone(),
            status: OrderStatus::Filled,
            client_order_id: order.new_client_order_id.clone().unwrap_or_default(),
            price,
            avg_price: price,
            orig_qty: quantity,
            executed_qty: quantity,
            time_in_force: order.time_in_force,
            order_type: order.order_type,
            side: order.side,
            update_time: chrono::Utc::now().timestamp_millis(),
        })
    }

    /// Simulate placing a margin order.
    pub async fn place_margin_order(&self, order: &MarginOrder) -> Result<OrderResponse> {
        let mut state = self.state.write().await;
        let prices = self.prices.read().await;

        // IMPORTANT: Use entry price as fallback to avoid catastrophic fee errors
        // The old default of $50,000 would cause massive incorrect fees for low-priced assets
        let fallback_price = state
            .positions
            .get(&order.symbol)
            .map(|p| p.spot_entry_price)
            .filter(|p| *p > Decimal::ZERO)
            .unwrap_or(dec!(1)); // Last resort: $1 (much safer than $50,000)

        let price = prices.get(&order.symbol).copied().unwrap_or(fallback_price);
        let quantity = order.quantity.unwrap_or(Decimal::ZERO);
        let notional = quantity * price;
        let fee = notional * self.fee_rate;

        // Update position
        let borrowed_amount = {
            let position = state
                .positions
                .entry(order.symbol.clone())
                .or_insert_with(|| MockPosition {
                    symbol: order.symbol.clone(),
                    ..Default::default()
                });

            match order.side {
                OrderSide::Buy => {
                    position.spot_qty += quantity;
                    position.spot_entry_price = price;
                }
                OrderSide::Sell => {
                    position.spot_qty -= quantity;
                    position.spot_entry_price = price;
                    // Track borrowed amount for shorting
                    if position.spot_qty < Decimal::ZERO {
                        position.borrowed_amount = position.spot_qty.abs();
                    }
                }
            }
            position.borrowed_amount
        };

        state.balance -= fee;
        state.total_trading_fees += fee;
        state.order_count += 1;

        let order_id = self.next_order_id() as i64;

        info!(
            order_id,
            symbol = %order.symbol,
            side = ?order.side,
            quantity = %quantity,
            price = %price,
            fee = %fee,
            borrowed = %borrowed_amount,
            "Mock margin order executed"
        );

        Ok(OrderResponse {
            order_id,
            symbol: order.symbol.clone(),
            status: OrderStatus::Filled,
            client_order_id: String::new(),
            price,
            avg_price: price,
            orig_qty: quantity,
            executed_qty: quantity,
            time_in_force: Some(TimeInForce::Gtc),
            order_type: order.order_type,
            side: order.side,
            update_time: chrono::Utc::now().timestamp_millis(),
        })
    }

    /// Set leverage (no-op in mock).
    pub async fn set_leverage(&self, symbol: &str, leverage: u8) -> Result<()> {
        debug!(%symbol, %leverage, "Mock set leverage");
        Ok(())
    }

    /// Set margin type (no-op in mock).
    pub async fn set_margin_type(&self, symbol: &str, margin_type: MarginType) -> Result<()> {
        debug!(%symbol, margin_type = ?margin_type, "Mock set margin type");
        Ok(())
    }

    /// Get delta-neutral positions from mock state.
    pub async fn get_delta_neutral_positions(&self) -> Vec<DeltaNeutralPosition> {
        let state = self.state.read().await;

        state
            .positions
            .iter()
            .filter(|(_, p)| p.futures_qty != Decimal::ZERO || p.spot_qty != Decimal::ZERO)
            .map(|(symbol, p)| {
                DeltaNeutralPosition {
                    symbol: symbol.clone(),
                    spot_symbol: symbol.clone(),
                    base_asset: symbol.strip_suffix("USDT").unwrap_or("BTC").to_string(),
                    futures_qty: p.futures_qty,
                    futures_entry_price: p.futures_entry_price,
                    spot_qty: p.spot_qty,
                    spot_entry_price: p.spot_entry_price,
                    net_delta: p.futures_qty + p.spot_qty,
                    borrowed_amount: p.borrowed_amount,
                    // Use per-position tracking data
                    funding_pnl: p.total_funding_received,
                    interest_paid: p.total_interest_paid,
                }
            })
            .collect()
    }

    /// Calculate current PnL.
    pub async fn calculate_pnl(&self) -> (Decimal, Decimal) {
        let state = self.state.read().await;
        let prices = self.prices.read().await;

        let mut unrealized_pnl = Decimal::ZERO;

        for (symbol, position) in &state.positions {
            if let Some(&current_price) = prices.get(symbol) {
                // Futures PnL
                let futures_pnl =
                    position.futures_qty * (current_price - position.futures_entry_price);
                // Spot PnL
                let spot_pnl = position.spot_qty * (current_price - position.spot_entry_price);
                unrealized_pnl += futures_pnl + spot_pnl;
            }
        }

        let realized_pnl =
            state.total_funding_received - state.total_trading_fees - state.total_borrow_interest;

        (realized_pnl, unrealized_pnl)
    }

    /// Set the expected funding rate for a position.
    /// Call this after position entry to record the expected rate for anomaly detection.
    pub async fn set_expected_funding_rate(&self, symbol: &str, rate: Decimal) {
        let mut state = self.state.write().await;
        if let Some(position) = state.positions.get_mut(symbol) {
            position.expected_funding_rate = rate;
            debug!(
                %symbol, %rate,
                "Set expected funding rate for position"
            );
        }
    }

    /// Export current state for persistence.
    pub async fn export_state(&self) -> PersistedState {
        let state = self.state.read().await;

        let positions = state
            .positions
            .iter()
            .map(|(symbol, pos)| {
                (
                    symbol.clone(),
                    PersistedPosition {
                        symbol: symbol.clone(),
                        futures_qty: pos.futures_qty,
                        futures_entry_price: pos.futures_entry_price,
                        spot_qty: pos.spot_qty,
                        spot_entry_price: pos.spot_entry_price,
                        borrowed_amount: pos.borrowed_amount,
                        opened_at: pos.opened_at,
                        total_funding_received: pos.total_funding_received,
                        total_interest_paid: pos.total_interest_paid,
                        funding_collections: pos.funding_collections,
                        expected_funding_rate: pos.expected_funding_rate,
                    },
                )
            })
            .collect();

        PersistedState {
            initial_balance: state.initial_balance,
            balance: state.balance,
            total_funding_received: state.total_funding_received,
            total_trading_fees: state.total_trading_fees,
            total_borrow_interest: state.total_borrow_interest,
            order_count: state.order_count,
            positions,
            last_saved: Utc::now(),
            // Note: last_funding_period is managed by main.rs and should be set by caller
            last_funding_period: None,
        }
    }

    /// Restore state from persistence.
    pub async fn restore_state(&self, persisted: PersistedState) {
        let mut state = self.state.write().await;

        state.initial_balance = persisted.initial_balance;
        state.balance = persisted.balance;
        state.total_funding_received = persisted.total_funding_received;
        state.total_trading_fees = persisted.total_trading_fees;
        state.total_borrow_interest = persisted.total_borrow_interest;
        state.order_count = persisted.order_count;

        state.positions = persisted
            .positions
            .into_iter()
            .map(|(symbol, pos)| {
                (
                    symbol,
                    MockPosition {
                        symbol: pos.symbol,
                        futures_qty: pos.futures_qty,
                        futures_entry_price: pos.futures_entry_price,
                        spot_qty: pos.spot_qty,
                        spot_entry_price: pos.spot_entry_price,
                        borrowed_amount: pos.borrowed_amount,
                        opened_at: pos.opened_at,
                        total_funding_received: pos.total_funding_received,
                        total_interest_paid: pos.total_interest_paid,
                        funding_collections: pos.funding_collections,
                        expected_funding_rate: pos.expected_funding_rate,
                    },
                )
            })
            .collect();

        // Update order counter to be higher than persisted count
        self.order_id_counter
            .store(persisted.order_count + 1, Ordering::SeqCst);

        info!(
            balance = %state.balance,
            positions = state.positions.len(),
            order_count = state.order_count,
            "Mock client state restored from persistence"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Helper functions
    // =========================================================================

    fn create_test_client() -> MockBinanceClient {
        MockBinanceClient::new(dec!(10000))
    }

    async fn setup_client_with_price(price: Decimal) -> MockBinanceClient {
        let client = create_test_client();
        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), price);
        client.update_market_data(HashMap::new(), prices).await;
        client
    }

    async fn open_short_futures_position(
        client: &MockBinanceClient,
        symbol: &str,
        quantity: Decimal,
    ) -> OrderResponse {
        let order = NewOrder {
            symbol: symbol.to_string(),
            side: OrderSide::Sell,
            position_side: None,
            order_type: OrderType::Market,
            quantity: Some(quantity),
            price: None,
            time_in_force: None,
            reduce_only: None,
            new_client_order_id: None,
        };
        client.place_futures_order(&order).await.unwrap()
    }

    async fn open_long_futures_position(
        client: &MockBinanceClient,
        symbol: &str,
        quantity: Decimal,
    ) -> OrderResponse {
        let order = NewOrder {
            symbol: symbol.to_string(),
            side: OrderSide::Buy,
            position_side: None,
            order_type: OrderType::Market,
            quantity: Some(quantity),
            price: None,
            time_in_force: None,
            reduce_only: None,
            new_client_order_id: None,
        };
        client.place_futures_order(&order).await.unwrap()
    }

    async fn open_margin_short(
        client: &MockBinanceClient,
        symbol: &str,
        quantity: Decimal,
    ) -> OrderResponse {
        let order = MarginOrder {
            symbol: symbol.to_string(),
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity: Some(quantity),
            price: None,
            time_in_force: None,
            side_effect_type: Some(SideEffectType::MarginBuy),
            is_isolated: None,
        };
        client.place_margin_order(&order).await.unwrap()
    }

    // =========================================================================
    // Basic Order Execution Tests
    // =========================================================================

    #[tokio::test]
    async fn test_mock_order_execution() {
        let client = MockBinanceClient::new(dec!(10000));

        // Set mock price
        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        client.update_market_data(HashMap::new(), prices).await;

        // Place futures order
        let order = NewOrder {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Sell,
            position_side: None,
            order_type: OrderType::Market,
            quantity: Some(dec!(0.1)),
            price: None,
            time_in_force: None,
            reduce_only: None,
            new_client_order_id: None,
        };

        let response = client.place_futures_order(&order).await.unwrap();
        assert_eq!(response.status, OrderStatus::Filled);
        assert_eq!(response.executed_qty, dec!(0.1));

        let state = client.get_state().await;
        assert_eq!(state.order_count, 1);
        assert!(state.total_trading_fees > Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_open_long_position() {
        let client = setup_client_with_price(dec!(50000)).await;

        let response = open_long_futures_position(&client, "BTCUSDT", dec!(0.5)).await;

        assert_eq!(response.status, OrderStatus::Filled);
        assert_eq!(response.executed_qty, dec!(0.5));

        let state = client.get_state().await;
        let position = state.positions.get("BTCUSDT").unwrap();
        assert_eq!(position.futures_qty, dec!(0.5)); // Positive = long
        assert_eq!(position.futures_entry_price, dec!(50000));
    }

    #[tokio::test]
    async fn test_open_short_position() {
        let client = setup_client_with_price(dec!(50000)).await;

        let response = open_short_futures_position(&client, "BTCUSDT", dec!(0.5)).await;

        assert_eq!(response.status, OrderStatus::Filled);

        let state = client.get_state().await;
        let position = state.positions.get("BTCUSDT").unwrap();
        assert_eq!(position.futures_qty, dec!(-0.5)); // Negative = short
    }

    #[tokio::test]
    async fn test_close_position_reduces_to_zero() {
        let client = setup_client_with_price(dec!(50000)).await;

        // Open short
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;
        // Close with buy
        open_long_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        let state = client.get_state().await;
        let position = state.positions.get("BTCUSDT").unwrap();
        assert_eq!(position.futures_qty, Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_reduce_position_partial_close() {
        let client = setup_client_with_price(dec!(50000)).await;

        // Open short 1.0 BTC
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;
        // Close 0.3 BTC
        open_long_futures_position(&client, "BTCUSDT", dec!(0.3)).await;

        let state = client.get_state().await;
        let position = state.positions.get("BTCUSDT").unwrap();
        assert_eq!(position.futures_qty, dec!(-0.7)); // 1.0 - 0.3 = 0.7 remaining short
    }

    // =========================================================================
    // Funding Collection Tests
    // =========================================================================

    #[tokio::test]
    async fn test_funding_positive_rate_short_receives() {
        let client = create_test_client();

        // Setup: Short position + positive funding rate
        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        rates.insert("BTCUSDT".to_string(), dec!(0.001)); // 0.1% positive rate
        client
            .update_market_data(rates.clone(), prices.clone())
            .await;

        // Open short futures position (qty = -1.0)
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        let balance_before = client.get_state().await.balance;

        // Collect funding
        let funding_map = client.collect_funding().await;

        let balance_after = client.get_state().await.balance;
        let funding_received = funding_map.get("BTCUSDT").copied().unwrap_or_default();

        // Short (-1.0) * price (50000) * rate (0.001) = -50
        // But formula is: -futures_value * rate = -(-50000) * 0.001 = +50
        assert_eq!(funding_received, dec!(50));
        assert_eq!(balance_after - balance_before, dec!(50));
    }

    #[tokio::test]
    async fn test_funding_negative_rate_long_receives() {
        let client = create_test_client();

        // Setup: Long position + negative funding rate
        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        rates.insert("BTCUSDT".to_string(), dec!(-0.001)); // -0.1% negative rate
        client.update_market_data(rates, prices).await;

        // Open long futures position (qty = +1.0)
        open_long_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        let balance_before = client.get_state().await.balance;

        // Collect funding
        let funding_map = client.collect_funding().await;

        let balance_after = client.get_state().await.balance;
        let funding_received = funding_map.get("BTCUSDT").copied().unwrap_or_default();

        // Long (+1.0) * price (50000) * rate (-0.001) = -50 (value)
        // Formula: -futures_value * rate = -(50000) * (-0.001) = +50
        assert_eq!(funding_received, dec!(50));
        assert!(balance_after > balance_before);
    }

    #[tokio::test]
    async fn test_funding_zero_rate_no_payment() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        rates.insert("BTCUSDT".to_string(), Decimal::ZERO);
        client.update_market_data(rates, prices).await;

        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        let balance_before = client.get_state().await.balance;
        let funding_map = client.collect_funding().await;
        let balance_after = client.get_state().await.balance;

        let funding_received = funding_map.get("BTCUSDT").copied().unwrap_or_default();
        assert_eq!(funding_received, Decimal::ZERO);
        assert_eq!(balance_before, balance_after);
    }

    #[tokio::test]
    async fn test_funding_extreme_rate_calculated_correctly() {
        let client = create_test_client();

        // Extreme rate like what was seen with DUSKUSDT (-1% per 8h)
        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("DUSKUSDT".to_string(), dec!(0.5)); // $0.50 price
        rates.insert("DUSKUSDT".to_string(), dec!(-0.01)); // -1% extreme rate
        client.update_market_data(rates, prices).await;

        // Long position with negative rate = receive funding
        open_long_futures_position(&client, "DUSKUSDT", dec!(10000.0)).await; // 10k DUSK

        let funding_map = client.collect_funding().await;
        let funding = funding_map.get("DUSKUSDT").copied().unwrap_or_default();

        // Position value: 10000 * 0.5 = $5000
        // Funding: -5000 * -0.01 = $50
        assert_eq!(funding, dec!(50));
    }

    #[tokio::test]
    async fn test_funding_tracks_per_position() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        prices.insert("ETHUSDT".to_string(), dec!(3000));
        rates.insert("BTCUSDT".to_string(), dec!(0.0005)); // 0.05%
        rates.insert("ETHUSDT".to_string(), dec!(0.001)); // 0.1%
        client.update_market_data(rates, prices).await;

        open_short_futures_position(&client, "BTCUSDT", dec!(0.1)).await;
        open_short_futures_position(&client, "ETHUSDT", dec!(1.0)).await;

        // Collect funding multiple times
        client.collect_funding().await;
        client.collect_funding().await;

        let state = client.get_state().await;

        let btc_pos = state.positions.get("BTCUSDT").unwrap();
        let eth_pos = state.positions.get("ETHUSDT").unwrap();

        // BTC: 0.1 * 50000 * 0.0005 = $2.50 per collection, x2 = $5
        assert_eq!(btc_pos.funding_collections, 2);
        assert_eq!(btc_pos.total_funding_received, dec!(5));

        // ETH: 1.0 * 3000 * 0.001 = $3 per collection, x2 = $6
        assert_eq!(eth_pos.funding_collections, 2);
        assert_eq!(eth_pos.total_funding_received, dec!(6));
    }

    // =========================================================================
    // Interest Accrual Tests
    // =========================================================================

    #[tokio::test]
    async fn test_interest_accrual_hourly() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        client.update_market_data(HashMap::new(), prices).await;

        // Open margin short to create borrowed amount
        open_margin_short(&client, "BTCUSDT", dec!(0.2)).await;

        let balance_before = client.get_state().await.balance;

        // Accrue 8 hours of interest
        let interest_map = client.accrue_interest(dec!(8)).await;

        let state = client.get_state().await;
        let interest_paid = interest_map.get("BTCUSDT").copied().unwrap_or_default();

        // Borrowed: 0.2 BTC
        // Interest: 0.2 * 0.00002 * 8 = 0.000032 BTC
        // But we track in USDT, so: borrowed_amount (in USDT terms) * rate * hours
        // Actually borrowed_amount is tracked as abs(spot_qty) = 0.2
        // Interest = 0.2 * 0.00002 * 8 = 0.000032
        assert!(interest_paid > Decimal::ZERO);
        assert!(state.balance < balance_before);
    }

    #[tokio::test]
    async fn test_interest_accrual_partial_hour() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        client.update_market_data(HashMap::new(), prices).await;

        open_margin_short(&client, "BTCUSDT", dec!(1.0)).await;

        // Accrue 0.5 hours
        let interest_map = client.accrue_interest(dec!(0.5)).await;

        let interest = interest_map.get("BTCUSDT").copied().unwrap_or_default();

        // Interest = 1.0 * 0.00002 * 0.5 = 0.00001
        assert_eq!(interest, dec!(0.00001));
    }

    #[tokio::test]
    async fn test_interest_no_borrow_no_accrual() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        client.update_market_data(HashMap::new(), prices).await;

        // Open futures only (no margin borrow)
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        let balance_before = client.get_state().await.balance;

        let interest_map = client.accrue_interest(dec!(24)).await;

        let balance_after = client.get_state().await.balance;

        // No interest should be charged since no borrowed amount
        assert!(interest_map.is_empty() || interest_map.values().all(|&v| v == Decimal::ZERO));
        assert_eq!(balance_before, balance_after);
    }

    #[tokio::test]
    async fn test_interest_tracks_per_position() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        prices.insert("ETHUSDT".to_string(), dec!(3000));
        client.update_market_data(HashMap::new(), prices).await;

        open_margin_short(&client, "BTCUSDT", dec!(0.1)).await;
        open_margin_short(&client, "ETHUSDT", dec!(2.0)).await;

        client.accrue_interest(dec!(10)).await;

        let state = client.get_state().await;
        let btc_pos = state.positions.get("BTCUSDT").unwrap();
        let eth_pos = state.positions.get("ETHUSDT").unwrap();

        // BTC: 0.1 * 0.00002 * 10 = 0.00002
        assert_eq!(btc_pos.total_interest_paid, dec!(0.00002));

        // ETH: 2.0 * 0.00002 * 10 = 0.0004
        assert_eq!(eth_pos.total_interest_paid, dec!(0.0004));
    }

    // =========================================================================
    // Fee Calculation Tests
    // =========================================================================

    #[tokio::test]
    async fn test_trading_fee_calculation() {
        let client = setup_client_with_price(dec!(50000)).await;

        let balance_before = client.get_state().await.balance;

        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        let state = client.get_state().await;

        // Fee = qty * price * 0.0004 = 1.0 * 50000 * 0.0004 = $20
        assert_eq!(state.total_trading_fees, dec!(20));
        assert_eq!(state.balance, balance_before - dec!(20));
    }

    #[tokio::test]
    async fn test_fee_accumulation_multiple_trades() {
        let client = setup_client_with_price(dec!(50000)).await;

        // Trade 1: 0.5 BTC short
        open_short_futures_position(&client, "BTCUSDT", dec!(0.5)).await;
        // Trade 2: 0.3 BTC long (partial close)
        open_long_futures_position(&client, "BTCUSDT", dec!(0.3)).await;
        // Trade 3: 0.2 BTC long (complete close)
        open_long_futures_position(&client, "BTCUSDT", dec!(0.2)).await;

        let state = client.get_state().await;

        // Total fees:
        // Trade 1: 0.5 * 50000 * 0.0004 = $10
        // Trade 2: 0.3 * 50000 * 0.0004 = $6
        // Trade 3: 0.2 * 50000 * 0.0004 = $4
        // Total: $20
        assert_eq!(state.total_trading_fees, dec!(20));
        assert_eq!(state.order_count, 3);
    }

    #[tokio::test]
    async fn test_margin_order_fee_calculation() {
        let client = setup_client_with_price(dec!(50000)).await;

        let balance_before = client.get_state().await.balance;

        open_margin_short(&client, "BTCUSDT", dec!(0.5)).await;

        let state = client.get_state().await;

        // Fee = 0.5 * 50000 * 0.0004 = $10
        assert_eq!(state.total_trading_fees, dec!(10));
        assert_eq!(state.balance, balance_before - dec!(10));
    }

    // =========================================================================
    // Margin Operations Tests
    // =========================================================================

    #[tokio::test]
    async fn test_borrow_spot_margin() {
        let client = setup_client_with_price(dec!(50000)).await;

        open_margin_short(&client, "BTCUSDT", dec!(1.0)).await;

        let state = client.get_state().await;
        let position = state.positions.get("BTCUSDT").unwrap();

        assert_eq!(position.spot_qty, dec!(-1.0));
        assert_eq!(position.borrowed_amount, dec!(1.0));
    }

    #[tokio::test]
    async fn test_margin_buy_long_no_borrow() {
        let client = setup_client_with_price(dec!(50000)).await;

        // Buy on margin (long) - no borrowing needed
        let order = MarginOrder {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Some(dec!(0.5)),
            price: None,
            time_in_force: None,
            side_effect_type: Some(SideEffectType::MarginBuy),
            is_isolated: None,
        };
        client.place_margin_order(&order).await.unwrap();

        let state = client.get_state().await;
        let position = state.positions.get("BTCUSDT").unwrap();

        assert_eq!(position.spot_qty, dec!(0.5));
        assert_eq!(position.borrowed_amount, Decimal::ZERO); // No borrow for long
    }

    // =========================================================================
    // PnL Calculation Tests
    // =========================================================================

    #[tokio::test]
    async fn test_price_update_affects_unrealized_pnl() {
        let client = create_test_client();

        // Initial price
        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        client.update_market_data(HashMap::new(), prices).await;

        // Open short at $50,000
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        // Price drops to $48,000 - short should be in profit
        let mut new_prices = HashMap::new();
        new_prices.insert("BTCUSDT".to_string(), dec!(48000));
        client.update_market_data(HashMap::new(), new_prices).await;

        let (_, unrealized_pnl) = client.calculate_pnl().await;

        // Short PnL: -1.0 * (48000 - 50000) = -1.0 * -2000 = +$2000
        assert_eq!(unrealized_pnl, dec!(2000));
    }

    #[tokio::test]
    async fn test_unrealized_pnl_loss_scenario() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        client.update_market_data(HashMap::new(), prices).await;

        // Open short at $50,000
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        // Price rises to $52,000 - short should be in loss
        let mut new_prices = HashMap::new();
        new_prices.insert("BTCUSDT".to_string(), dec!(52000));
        client.update_market_data(HashMap::new(), new_prices).await;

        let (_, unrealized_pnl) = client.calculate_pnl().await;

        // Short PnL: -1.0 * (52000 - 50000) = -$2000
        assert_eq!(unrealized_pnl, dec!(-2000));
    }

    #[tokio::test]
    async fn test_realized_pnl_includes_all_components() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        rates.insert("BTCUSDT".to_string(), dec!(0.001));
        client.update_market_data(rates, prices).await;

        // Open position (creates fees)
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;
        open_margin_short(&client, "BTCUSDT", dec!(1.0)).await;

        // Collect funding
        client.collect_funding().await;

        // Accrue interest
        client.accrue_interest(dec!(8)).await;

        let (realized_pnl, _) = client.calculate_pnl().await;
        let state = client.get_state().await;

        // Realized PnL = funding - fees - interest
        let expected =
            state.total_funding_received - state.total_trading_fees - state.total_borrow_interest;
        assert_eq!(realized_pnl, expected);
    }

    // =========================================================================
    // State Reset Tests
    // =========================================================================

    #[tokio::test]
    async fn test_reset_clears_all_state() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        rates.insert("BTCUSDT".to_string(), dec!(0.001));
        client.update_market_data(rates, prices).await;

        // Create some state
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;
        open_margin_short(&client, "BTCUSDT", dec!(1.0)).await;
        client.collect_funding().await;
        client.accrue_interest(dec!(8)).await;

        // Verify state exists
        let state_before = client.get_state().await;
        assert!(!state_before.positions.is_empty());
        assert!(state_before.total_funding_received > Decimal::ZERO);

        // Reset
        client.reset(dec!(5000)).await;

        // Verify all state cleared
        let state_after = client.get_state().await;
        assert_eq!(state_after.initial_balance, dec!(5000));
        assert_eq!(state_after.balance, dec!(5000));
        assert!(state_after.positions.is_empty());
        assert_eq!(state_after.total_funding_received, Decimal::ZERO);
        assert_eq!(state_after.total_trading_fees, Decimal::ZERO);
        assert_eq!(state_after.total_borrow_interest, Decimal::ZERO);
        assert_eq!(state_after.order_count, 0);
    }

    // =========================================================================
    // Delta Neutral Position Tests
    // =========================================================================

    #[tokio::test]
    async fn test_get_delta_neutral_positions() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        client.update_market_data(HashMap::new(), prices).await;

        // Create delta-neutral position: short futures + long spot
        open_short_futures_position(&client, "BTCUSDT", dec!(1.0)).await;

        // Buy on margin
        let order = MarginOrder {
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Some(dec!(1.0)),
            price: None,
            time_in_force: None,
            side_effect_type: Some(SideEffectType::MarginBuy),
            is_isolated: None,
        };
        client.place_margin_order(&order).await.unwrap();

        let positions = client.get_delta_neutral_positions().await;

        assert_eq!(positions.len(), 1);
        let pos = &positions[0];
        assert_eq!(pos.symbol, "BTCUSDT");
        assert_eq!(pos.futures_qty, dec!(-1.0));
        assert_eq!(pos.spot_qty, dec!(1.0));
        assert_eq!(pos.net_delta, Decimal::ZERO); // Delta neutral!
    }

    // =========================================================================
    // State Persistence Tests
    // =========================================================================

    #[tokio::test]
    async fn test_export_and_restore_state() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        rates.insert("BTCUSDT".to_string(), dec!(0.001));
        client.update_market_data(rates, prices).await;

        // Create state
        open_short_futures_position(&client, "BTCUSDT", dec!(0.5)).await;
        client.collect_funding().await;

        // Export
        let exported = client.export_state().await;

        // Create new client and restore
        let client2 = create_test_client();
        client2.restore_state(exported).await;

        // Verify state matches
        let state1 = client.get_state().await;
        let state2 = client2.get_state().await;

        assert_eq!(state1.balance, state2.balance);
        assert_eq!(state1.total_funding_received, state2.total_funding_received);
        assert_eq!(state1.positions.len(), state2.positions.len());
    }

    // =========================================================================
    // Multiple Positions Tests
    // =========================================================================

    #[tokio::test]
    async fn test_multiple_positions_independent() {
        let client = create_test_client();

        let mut prices = HashMap::new();
        let mut rates = HashMap::new();
        prices.insert("BTCUSDT".to_string(), dec!(50000));
        prices.insert("ETHUSDT".to_string(), dec!(3000));
        prices.insert("SOLUSDT".to_string(), dec!(100));
        rates.insert("BTCUSDT".to_string(), dec!(0.001));
        rates.insert("ETHUSDT".to_string(), dec!(0.0005));
        rates.insert("SOLUSDT".to_string(), dec!(-0.0002));
        client.update_market_data(rates, prices).await;

        // Open multiple positions
        open_short_futures_position(&client, "BTCUSDT", dec!(0.1)).await;
        open_short_futures_position(&client, "ETHUSDT", dec!(1.0)).await;
        open_long_futures_position(&client, "SOLUSDT", dec!(10.0)).await;

        client.collect_funding().await;

        let state = client.get_state().await;

        assert_eq!(state.positions.len(), 3);

        // BTC: short 0.1 @ 50000 w/ 0.1% rate = $5 funding
        let btc = state.positions.get("BTCUSDT").unwrap();
        assert_eq!(btc.total_funding_received, dec!(5));

        // ETH: short 1.0 @ 3000 w/ 0.05% rate = $1.5 funding
        let eth = state.positions.get("ETHUSDT").unwrap();
        assert_eq!(eth.total_funding_received, dec!(1.5));

        // SOL: long 10.0 @ 100 w/ -0.02% rate = $0.2 funding
        let sol = state.positions.get("SOLUSDT").unwrap();
        assert_eq!(sol.total_funding_received, dec!(0.2));
    }
}
