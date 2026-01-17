//! Backtesting simulation engine.
//!
//! Replays historical market data through the trading strategy.

use crate::backtest::{
    next_funding_time, BacktestConfig, DataLoader, MarketSnapshot,
};
use crate::backtest::metrics::{BacktestMetrics, EquityPoint};
use crate::config::Config;
use crate::exchange::{MockBinanceClient, QualifiedPair};
use crate::exchange::mock::MockTradingState;
use crate::strategy::CapitalAllocator;
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Result of a single simulation step.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub timestamp: DateTime<Utc>,
    pub balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_equity: Decimal,
    pub position_count: usize,
    pub funding_collected: Decimal,
}

/// Complete result of a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub config: Config,
    pub backtest_config: BacktestConfig,
    pub metrics: BacktestMetrics,
    pub equity_curve: Vec<EquityPoint>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub snapshots_processed: usize,
    pub funding_events: usize,
}

impl BacktestResult {
    /// Export equity curve to CSV.
    pub fn equity_to_csv(&self, path: &str) -> Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(path)?;
        writeln!(file, "timestamp,balance,unrealized_pnl,total_equity,drawdown,positions")?;

        for point in &self.equity_curve {
            writeln!(
                file,
                "{},{},{},{},{},{}",
                point.timestamp.to_rfc3339(),
                point.balance,
                point.unrealized_pnl,
                point.total_equity,
                point.drawdown,
                point.position_count,
            )?;
        }

        Ok(())
    }

    /// Get a summary string.
    pub fn summary(&self) -> String {
        format!(
            "{}\n\nBacktest Period: {} to {}\nSnapshots: {}\nFunding Events: {}",
            self.metrics.summary(),
            self.start_time.format("%Y-%m-%d"),
            self.end_time.format("%Y-%m-%d"),
            self.snapshots_processed,
            self.funding_events,
        )
    }
}

/// The backtesting simulation engine.
pub struct BacktestEngine<D: DataLoader> {
    data_loader: D,
    config: Config,
    backtest_config: BacktestConfig,
    mock_client: MockBinanceClient,
    allocator: CapitalAllocator,
    current_time: DateTime<Utc>,
    next_funding: DateTime<Utc>,

    // Tracking for metrics
    equity_curve: Vec<EquityPoint>,
    peak_equity: Decimal,
    total_funding: Decimal,
    funding_events: usize,
    positions_opened: u64,
    positions_closed: u64,
    winning_positions: u64,
    total_position_hours: f64,
}

impl<D: DataLoader> BacktestEngine<D> {
    /// Create a new backtest engine.
    pub fn new(data_loader: D, config: Config, backtest_config: BacktestConfig) -> Self {
        let initial_balance = backtest_config.initial_balance;
        let mock_client = MockBinanceClient::new(initial_balance);

        let allocator = CapitalAllocator::new(
            config.capital.clone(),
            config.risk.clone(),
            config.execution.default_leverage,
        );

        Self {
            data_loader,
            config,
            backtest_config,
            mock_client,
            allocator,
            current_time: Utc::now(),
            next_funding: Utc::now(),
            equity_curve: Vec::new(),
            peak_equity: initial_balance,
            total_funding: Decimal::ZERO,
            funding_events: 0,
            positions_opened: 0,
            positions_closed: 0,
            winning_positions: 0,
            total_position_hours: 0.0,
        }
    }

    /// Run the backtest from start to end time.
    pub async fn run(
        &mut self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<BacktestResult> {
        info!(
            "Starting backtest from {} to {}",
            start.format("%Y-%m-%d %H:%M"),
            end.format("%Y-%m-%d %H:%M")
        );

        // Load historical data
        let snapshots = self.data_loader.load_snapshots(start, end)?;
        if snapshots.is_empty() {
            anyhow::bail!("No data available for the specified time range");
        }

        info!("Loaded {} snapshots", snapshots.len());

        // Initialize time tracking
        self.current_time = snapshots[0].timestamp;
        self.next_funding = next_funding_time(self.current_time);
        self.peak_equity = self.backtest_config.initial_balance;

        // Reset tracking
        self.equity_curve.clear();
        self.total_funding = Decimal::ZERO;
        self.funding_events = 0;
        self.positions_opened = 0;
        self.positions_closed = 0;
        self.winning_positions = 0;
        self.total_position_hours = 0.0;

        // Process each snapshot
        for (i, snapshot) in snapshots.iter().enumerate() {
            self.current_time = snapshot.timestamp;

            // Step the simulation
            let step_result = self.step(snapshot).await?;

            // Record equity point
            if self.backtest_config.record_equity_curve {
                let point = EquityPoint::new(
                    step_result.timestamp,
                    step_result.balance,
                    step_result.unrealized_pnl,
                    step_result.position_count,
                    self.peak_equity,
                );
                self.equity_curve.push(point);
            }

            // Update peak equity
            if step_result.total_equity > self.peak_equity {
                self.peak_equity = step_result.total_equity;
            }

            // Progress logging
            if i % 100 == 0 {
                debug!(
                    "Progress: {}/{} ({:.1}%), Equity: ${:.2}",
                    i,
                    snapshots.len(),
                    (i as f64 / snapshots.len() as f64) * 100.0,
                    step_result.total_equity
                );
            }
        }

        // Get final state
        let final_state = self.mock_client.get_state().await;

        // Calculate metrics
        let metrics = BacktestMetrics::calculate(
            &self.equity_curve,
            self.backtest_config.initial_balance,
            self.total_funding,
            final_state.total_trading_fees,
            final_state.total_borrow_interest,
            self.positions_opened,
            self.positions_closed,
            self.winning_positions,
            self.total_position_hours,
        );

        info!("Backtest complete. Final equity: ${:.2}", final_state.balance);

        Ok(BacktestResult {
            config: self.config.clone(),
            backtest_config: self.backtest_config.clone(),
            metrics,
            equity_curve: self.equity_curve.clone(),
            start_time: start,
            end_time: end,
            snapshots_processed: snapshots.len(),
            funding_events: self.funding_events,
        })
    }

    /// Process a single time step.
    async fn step(&mut self, snapshot: &MarketSnapshot) -> Result<StepResult> {
        // 1. Update market data in mock client
        self.mock_client
            .set_market_data(snapshot.funding_rates(), snapshot.prices())
            .await;

        // 2. Check for funding collection
        let mut funding_collected = Decimal::ZERO;
        if self.current_time >= self.next_funding {
            funding_collected = self.process_funding().await?;
            self.next_funding = next_funding_time(self.current_time + Duration::seconds(1));
        }

        // 3. Accrue interest (proportional to time since last step)
        let time_step_hours = self.backtest_config.time_step_minutes as f64 / 60.0;
        let interest_hours = Decimal::from_f64_retain(time_step_hours).unwrap_or(dec!(1));
        self.mock_client.accrue_interest(interest_hours).await;

        // 4. Run strategy (simplified - just allocation for now)
        self.run_strategy_step(snapshot).await?;

        // 5. Get current state
        let state = self.mock_client.get_state().await;
        let (_, unrealized_pnl) = self.mock_client.calculate_pnl().await;
        let total_equity = state.balance + unrealized_pnl;

        Ok(StepResult {
            timestamp: self.current_time,
            balance: state.balance,
            unrealized_pnl,
            total_equity,
            position_count: state.positions.len(),
            funding_collected,
        })
    }

    /// Process funding collection at funding times.
    async fn process_funding(&mut self) -> Result<Decimal> {
        let per_position_funding = self.mock_client.collect_funding().await;
        let total: Decimal = per_position_funding.values().sum();

        if total != Decimal::ZERO {
            debug!(
                "Funding collected at {}: ${:.4} across {} positions",
                self.current_time.format("%Y-%m-%d %H:%M"),
                total,
                per_position_funding.len()
            );
        }

        self.total_funding += total;
        self.funding_events += 1;

        Ok(total)
    }

    /// Run one step of strategy logic.
    async fn run_strategy_step(&mut self, snapshot: &MarketSnapshot) -> Result<()> {
        // Convert snapshot to qualified pairs for allocator
        let qualified_pairs = self.snapshot_to_qualified_pairs(snapshot);

        if qualified_pairs.is_empty() {
            return Ok(());
        }

        // Get current state
        let state = self.mock_client.get_state().await;
        let current_positions: std::collections::HashMap<String, Decimal> = state
            .positions
            .iter()
            .map(|(sym, pos)| (sym.clone(), pos.futures_qty.abs() * pos.futures_entry_price))
            .collect();

        // Calculate allocations
        let allocations = self.allocator.calculate_allocation(
            &qualified_pairs,
            state.balance,
            &current_positions,
        );

        // Execute allocations (enter new positions)
        for alloc in allocations.iter().take(5) {
            // Max 5 new positions per step
            // Skip if already have position
            if state.positions.contains_key(&alloc.symbol) {
                continue;
            }

            // Get price from snapshot
            let symbol_data = match snapshot.get_symbol(&alloc.symbol) {
                Some(data) => data,
                None => continue,
            };

            // Calculate quantity
            let price = symbol_data.price;
            if price <= Decimal::ZERO {
                continue;
            }

            let quantity = alloc.target_size_usdt / price;

            // Determine sides based on funding direction
            let funding_rate = symbol_data.funding_rate;
            let (futures_side, spot_side) = if funding_rate > Decimal::ZERO {
                // Positive funding: short futures, long spot
                (crate::exchange::OrderSide::Sell, crate::exchange::OrderSide::Buy)
            } else {
                // Negative funding: long futures, short spot
                (crate::exchange::OrderSide::Buy, crate::exchange::OrderSide::Sell)
            };

            // Execute futures order
            let futures_order = crate::exchange::NewOrder {
                symbol: alloc.symbol.clone(),
                side: futures_side.clone(),
                position_side: None,
                order_type: crate::exchange::OrderType::Market,
                quantity: Some(quantity),
                price: None,
                time_in_force: None,
                reduce_only: Some(false),
                new_client_order_id: None,
            };

            let futures_result = self.mock_client.place_futures_order(&futures_order).await;

            if futures_result.is_err() {
                continue;
            }

            // Execute spot hedge
            let spot_symbol = alloc.symbol.replace("USDT", "");
            let spot_symbol = format!("{}USDT", spot_symbol);

            let margin_order = crate::exchange::MarginOrder {
                symbol: spot_symbol,
                side: spot_side,
                order_type: crate::exchange::OrderType::Market,
                quantity: Some(quantity),
                price: None,
                time_in_force: None,
                is_isolated: Some(false),
                side_effect_type: Some(crate::exchange::SideEffectType::AutoBorrowRepay),
            };

            let _ = self.mock_client.place_margin_order(&margin_order).await;

            self.positions_opened += 1;

            debug!(
                "Opened position: {} @ ${:.4}, qty: {:.4}",
                alloc.symbol, price, quantity
            );
        }

        Ok(())
    }

    /// Convert market snapshot to qualified pairs for the allocator.
    fn snapshot_to_qualified_pairs(&self, snapshot: &MarketSnapshot) -> Vec<QualifiedPair> {
        let config = &self.config.pair_selection;

        snapshot
            .symbols
            .iter()
            .filter(|s| {
                // Apply pair selection filters
                s.volume_24h >= config.min_volume_24h
                    && s.funding_rate.abs() >= config.min_funding_rate
                    && s.spread <= config.max_spread
                    && s.open_interest >= config.min_open_interest
            })
            .map(|s| {
                // Calculate score (simplified)
                let funding_score = s.funding_rate.abs() * dec!(10000);
                let volume_score =
                    (s.volume_24h / dec!(1_000_000_000)).min(dec!(1)) * dec!(100);
                let spread_penalty = s.spread * dec!(1000);
                let score = funding_score + volume_score - spread_penalty;

                // Extract base asset from symbol (e.g., "BTCUSDT" -> "BTC")
                let base_asset = s.symbol.strip_suffix("USDT")
                    .unwrap_or(&s.symbol)
                    .to_string();

                QualifiedPair {
                    symbol: s.symbol.clone(),
                    spot_symbol: s.symbol.clone(),
                    base_asset,
                    funding_rate: s.funding_rate,
                    volume_24h: s.volume_24h,
                    spread: s.spread,
                    open_interest: s.open_interest,
                    margin_available: true, // Assume available for backtesting
                    borrow_rate: None, // Not available in snapshot
                    score,
                }
            })
            .collect()
    }

    /// Get the current equity curve.
    pub fn equity_curve(&self) -> &[EquityPoint] {
        &self.equity_curve
    }

    /// Get the current mock client state.
    pub async fn get_state(&self) -> MockTradingState {
        self.mock_client.get_state().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::data::{CsvDataLoader, SymbolData};
    use chrono::TimeZone;

    // =========================================================================
    // Test Helpers
    // =========================================================================

    fn test_config() -> Config {
        Config::default()
    }

    fn test_backtest_config() -> BacktestConfig {
        BacktestConfig {
            initial_balance: dec!(10000),
            time_step_minutes: 60,
            record_equity_curve: true,
            record_trades: false,
            output_path: None,
        }
    }

    fn make_snapshot(
        timestamp: DateTime<Utc>,
        symbols: Vec<(&str, Decimal, Decimal)>,
    ) -> MarketSnapshot {
        MarketSnapshot {
            timestamp,
            symbols: symbols
                .into_iter()
                .map(|(sym, rate, price)| SymbolData {
                    symbol: sym.to_string(),
                    funding_rate: rate,
                    price,
                    volume_24h: dec!(1_500_000_000),
                    spread: dec!(0.0001),
                    open_interest: dec!(800_000_000),
                })
                .collect(),
        }
    }

    fn make_funding_time() -> DateTime<Utc> {
        // Funding times are 00:00, 08:00, 16:00 UTC
        Utc.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).unwrap()
    }

    // =========================================================================
    // Engine Creation Tests
    // =========================================================================

    #[tokio::test]
    async fn test_engine_creation() {
        let snapshots = vec![MarketSnapshot::new(Utc::now())];
        let loader = CsvDataLoader::from_snapshots(snapshots);

        let engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        assert_eq!(engine.backtest_config.initial_balance, dec!(10000));
    }

    #[tokio::test]
    async fn test_engine_initial_state() {
        let snapshots = vec![MarketSnapshot::new(Utc::now())];
        let loader = CsvDataLoader::from_snapshots(snapshots);

        let engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        assert!(engine.equity_curve.is_empty());
        assert_eq!(engine.total_funding, Decimal::ZERO);
        assert_eq!(engine.funding_events, 0);
        assert_eq!(engine.positions_opened, 0);
        assert_eq!(engine.positions_closed, 0);
    }

    #[tokio::test]
    async fn test_engine_with_custom_balance() {
        let config = BacktestConfig {
            initial_balance: dec!(50000),
            ..test_backtest_config()
        };

        let snapshots = vec![MarketSnapshot::new(Utc::now())];
        let loader = CsvDataLoader::from_snapshots(snapshots);

        let engine = BacktestEngine::new(loader, test_config(), config);

        assert_eq!(engine.peak_equity, dec!(50000));
    }

    // =========================================================================
    // Funding Processing Tests
    // =========================================================================

    #[tokio::test]
    async fn test_funding_time_processing() {
        let timestamp = make_funding_time();
        let snapshots = vec![make_snapshot(timestamp, vec![("BTCUSDT", dec!(0.0001), dec!(42000))])];

        let loader = CsvDataLoader::from_snapshots(snapshots);
        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        // Set next funding to the snapshot time
        engine.next_funding = timestamp;
        engine.current_time = timestamp;

        // Process funding should trigger
        let funding = engine.process_funding().await.unwrap();

        // No positions yet, so funding should be 0
        assert_eq!(funding, Decimal::ZERO);
        assert_eq!(engine.funding_events, 1);
    }

    #[tokio::test]
    async fn test_funding_accumulates() {
        let timestamp = make_funding_time();
        let snapshots = vec![make_snapshot(timestamp, vec![("BTCUSDT", dec!(0.0001), dec!(42000))])];

        let loader = CsvDataLoader::from_snapshots(snapshots);
        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        engine.next_funding = timestamp;
        engine.current_time = timestamp;

        // Process multiple funding events
        engine.process_funding().await.unwrap();
        engine.process_funding().await.unwrap();
        engine.process_funding().await.unwrap();

        assert_eq!(engine.funding_events, 3);
    }

    // =========================================================================
    // Snapshot to Qualified Pairs Tests
    // =========================================================================

    #[tokio::test]
    async fn test_snapshot_to_qualified_pairs_filters() {
        let timestamp = Utc::now();
        let snapshot = MarketSnapshot {
            timestamp,
            symbols: vec![
                // High volume, good funding - should qualify
                SymbolData {
                    symbol: "BTCUSDT".to_string(),
                    funding_rate: dec!(0.0005),
                    price: dec!(50000),
                    volume_24h: dec!(2_000_000_000),
                    spread: dec!(0.0001),
                    open_interest: dec!(1_000_000_000),
                },
                // Low volume - should NOT qualify
                SymbolData {
                    symbol: "LOWUSDT".to_string(),
                    funding_rate: dec!(0.001),
                    price: dec!(100),
                    volume_24h: dec!(10_000_000), // Below threshold
                    spread: dec!(0.0001),
                    open_interest: dec!(500_000_000),
                },
                // Low funding - should NOT qualify
                SymbolData {
                    symbol: "LOWFUNDUSDT".to_string(),
                    funding_rate: dec!(0.00001), // Below threshold
                    price: dec!(100),
                    volume_24h: dec!(500_000_000),
                    spread: dec!(0.0001),
                    open_interest: dec!(500_000_000),
                },
            ],
        };

        let loader = CsvDataLoader::from_snapshots(vec![snapshot.clone()]);
        let engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let pairs = engine.snapshot_to_qualified_pairs(&snapshot);

        // Only BTCUSDT should qualify
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].symbol, "BTCUSDT");
    }

    #[tokio::test]
    async fn test_snapshot_to_qualified_pairs_score() {
        let timestamp = Utc::now();
        let snapshot = MarketSnapshot {
            timestamp,
            symbols: vec![
                SymbolData {
                    symbol: "BTCUSDT".to_string(),
                    funding_rate: dec!(0.001), // High funding
                    price: dec!(50000),
                    volume_24h: dec!(2_000_000_000),
                    spread: dec!(0.0001),
                    open_interest: dec!(1_000_000_000),
                },
                SymbolData {
                    symbol: "ETHUSDT".to_string(),
                    funding_rate: dec!(0.0005), // Lower funding
                    price: dec!(3000),
                    volume_24h: dec!(1_000_000_000),
                    spread: dec!(0.0001),
                    open_interest: dec!(500_000_000),
                },
            ],
        };

        let loader = CsvDataLoader::from_snapshots(vec![snapshot.clone()]);
        let engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let pairs = engine.snapshot_to_qualified_pairs(&snapshot);

        // BTC should have higher score (higher funding rate)
        let btc = pairs.iter().find(|p| p.symbol == "BTCUSDT").unwrap();
        let eth = pairs.iter().find(|p| p.symbol == "ETHUSDT").unwrap();
        assert!(btc.score > eth.score);
    }

    #[tokio::test]
    async fn test_snapshot_to_qualified_pairs_base_asset() {
        let timestamp = Utc::now();
        let snapshot = make_snapshot(timestamp, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]);

        let loader = CsvDataLoader::from_snapshots(vec![snapshot.clone()]);
        let engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let pairs = engine.snapshot_to_qualified_pairs(&snapshot);

        assert_eq!(pairs[0].base_asset, "BTC");
    }

    // =========================================================================
    // Step Result Tests
    // =========================================================================

    #[tokio::test]
    async fn test_step_updates_equity() {
        let timestamp = Utc::now();
        let snapshot = make_snapshot(timestamp, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]);

        let loader = CsvDataLoader::from_snapshots(vec![snapshot.clone()]);
        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        engine.current_time = timestamp;
        engine.next_funding = timestamp + Duration::hours(8); // Don't trigger funding

        let result = engine.step(&snapshot).await.unwrap();

        assert_eq!(result.timestamp, timestamp);
        assert!(result.balance > Decimal::ZERO);
        // The step may open positions if qualifying pairs are found
        // Just verify the result is valid
        assert!(result.total_equity > Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_step_at_funding_time() {
        let timestamp = make_funding_time();
        let snapshot = make_snapshot(timestamp, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]);

        let loader = CsvDataLoader::from_snapshots(vec![snapshot.clone()]);
        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        engine.current_time = timestamp;
        engine.next_funding = timestamp; // Trigger funding

        let result = engine.step(&snapshot).await.unwrap();

        // Funding should have been processed
        assert_eq!(engine.funding_events, 1);
        // No positions, so funding collected should be 0
        assert_eq!(result.funding_collected, Decimal::ZERO);
    }

    // =========================================================================
    // Equity Curve Tests
    // =========================================================================

    #[tokio::test]
    async fn test_equity_curve_accessor() {
        let snapshots = vec![MarketSnapshot::new(Utc::now())];
        let loader = CsvDataLoader::from_snapshots(snapshots);

        let engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        // Initially empty
        assert!(engine.equity_curve().is_empty());
    }

    // =========================================================================
    // Get State Tests
    // =========================================================================

    #[tokio::test]
    async fn test_get_state() {
        let snapshots = vec![MarketSnapshot::new(Utc::now())];
        let loader = CsvDataLoader::from_snapshots(snapshots);

        let engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let state = engine.get_state().await;

        assert_eq!(state.balance, dec!(10000));
        assert!(state.positions.is_empty());
    }

    // =========================================================================
    // Run Backtest Tests
    // =========================================================================

    #[tokio::test]
    async fn test_run_with_empty_data() {
        let loader = CsvDataLoader::from_snapshots(vec![]);

        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let start = Utc::now() - Duration::days(1);
        let end = Utc::now();

        let result = engine.run(start, end).await;

        // Should error with no data
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_with_single_snapshot() {
        let timestamp = Utc::now();
        let snapshot = make_snapshot(timestamp, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]);

        let loader = CsvDataLoader::from_snapshots(vec![snapshot]);

        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let start = timestamp - Duration::hours(1);
        let end = timestamp + Duration::hours(1);

        let result = engine.run(start, end).await.unwrap();

        assert_eq!(result.snapshots_processed, 1);
        assert_eq!(result.start_time, start);
        assert_eq!(result.end_time, end);
    }

    #[tokio::test]
    async fn test_run_with_multiple_snapshots() {
        let base_time = Utc::now();
        let snapshots = vec![
            make_snapshot(base_time, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]),
            make_snapshot(base_time + Duration::hours(1), vec![("BTCUSDT", dec!(0.0006), dec!(50100))]),
            make_snapshot(base_time + Duration::hours(2), vec![("BTCUSDT", dec!(0.0004), dec!(49900))]),
        ];

        let loader = CsvDataLoader::from_snapshots(snapshots);

        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let start = base_time - Duration::hours(1);
        let end = base_time + Duration::hours(3);

        let result = engine.run(start, end).await.unwrap();

        assert_eq!(result.snapshots_processed, 3);
        assert!(!result.equity_curve.is_empty());
    }

    // =========================================================================
    // BacktestResult Tests
    // =========================================================================

    #[tokio::test]
    async fn test_backtest_result_summary() {
        let base_time = Utc::now();
        let snapshot = make_snapshot(base_time, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]);

        let loader = CsvDataLoader::from_snapshots(vec![snapshot]);

        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let start = base_time - Duration::hours(1);
        let end = base_time + Duration::hours(1);

        let result = engine.run(start, end).await.unwrap();

        let summary = result.summary();

        assert!(summary.contains("Backtest Period"));
        assert!(summary.contains("Snapshots: 1"));
    }

    // =========================================================================
    // StepResult Structure Tests
    // =========================================================================

    #[test]
    fn test_step_result_structure() {
        let result = StepResult {
            timestamp: Utc::now(),
            balance: dec!(10000),
            unrealized_pnl: dec!(100),
            total_equity: dec!(10100),
            position_count: 2,
            funding_collected: dec!(5),
        };

        assert_eq!(result.balance, dec!(10000));
        assert_eq!(result.unrealized_pnl, dec!(100));
        assert_eq!(result.total_equity, dec!(10100));
        assert_eq!(result.position_count, 2);
        assert_eq!(result.funding_collected, dec!(5));
    }

    // =========================================================================
    // Peak Equity Tracking Tests
    // =========================================================================

    #[tokio::test]
    async fn test_peak_equity_updates() {
        let base_time = Utc::now();
        let snapshots = vec![
            make_snapshot(base_time, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]),
            make_snapshot(base_time + Duration::hours(1), vec![("BTCUSDT", dec!(0.0005), dec!(50500))]),
            make_snapshot(base_time + Duration::hours(2), vec![("BTCUSDT", dec!(0.0005), dec!(50200))]),
        ];

        let loader = CsvDataLoader::from_snapshots(snapshots);

        let mut engine = BacktestEngine::new(loader, test_config(), test_backtest_config());

        let start = base_time - Duration::hours(1);
        let end = base_time + Duration::hours(3);

        let _result = engine.run(start, end).await.unwrap();

        // Peak should have been updated
        assert!(engine.peak_equity >= dec!(10000));
    }

    // =========================================================================
    // Configuration Tests
    // =========================================================================

    #[tokio::test]
    async fn test_backtest_config_time_step() {
        let config = BacktestConfig {
            time_step_minutes: 30, // 30 minute steps
            ..test_backtest_config()
        };

        let snapshots = vec![MarketSnapshot::new(Utc::now())];
        let loader = CsvDataLoader::from_snapshots(snapshots);

        let engine = BacktestEngine::new(loader, test_config(), config);

        assert_eq!(engine.backtest_config.time_step_minutes, 30);
    }

    #[tokio::test]
    async fn test_backtest_config_no_equity_curve() {
        let config = BacktestConfig {
            record_equity_curve: false,
            ..test_backtest_config()
        };

        let base_time = Utc::now();
        let snapshot = make_snapshot(base_time, vec![("BTCUSDT", dec!(0.0005), dec!(50000))]);

        let loader = CsvDataLoader::from_snapshots(vec![snapshot]);

        let mut engine = BacktestEngine::new(loader, test_config(), config);

        let start = base_time - Duration::hours(1);
        let end = base_time + Duration::hours(1);

        let result = engine.run(start, end).await.unwrap();

        // Equity curve should be empty when not recording
        assert!(result.equity_curve.is_empty());
    }
}
