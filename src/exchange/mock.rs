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

        let price = prices.get(&order.symbol).copied().unwrap_or(dec!(50000));
        let quantity = order.quantity.unwrap_or(Decimal::ZERO);
        let notional = quantity * price;
        let fee = notional * self.fee_rate;

        // Update position
        let position = state.positions.entry(order.symbol.clone()).or_insert_with(|| MockPosition {
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

        let price = prices.get(&order.symbol).copied().unwrap_or(dec!(50000));
        let quantity = order.quantity.unwrap_or(Decimal::ZERO);
        let notional = quantity * price;
        let fee = notional * self.fee_rate;

        // Update position
        let borrowed_amount = {
            let position = state.positions.entry(order.symbol.clone()).or_insert_with(|| MockPosition {
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
        let prices = self.prices.read().await;

        state
            .positions
            .iter()
            .filter(|(_, p)| p.futures_qty != Decimal::ZERO || p.spot_qty != Decimal::ZERO)
            .map(|(symbol, p)| {
                let _price = prices.get(symbol).copied().unwrap_or(dec!(50000));
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
                let futures_pnl = position.futures_qty * (current_price - position.futures_entry_price);
                // Spot PnL
                let spot_pnl = position.spot_qty * (current_price - position.spot_entry_price);
                unrealized_pnl += futures_pnl + spot_pnl;
            }
        }

        let realized_pnl = state.total_funding_received - state.total_trading_fees - state.total_borrow_interest;

        (realized_pnl, unrealized_pnl)
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
}
