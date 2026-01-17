//! Funding Fee Farmer - Main Entry Point
//!
//! MVP version with mock trading support for paper trading and testing.

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use clap::{Parser, Subcommand};
use funding_fee_farmer::backtest::{
    BacktestConfig, BacktestEngine, CsvDataLoader, DataLoader, ParameterSpace, SweepRunner,
};
use funding_fee_farmer::config::Config;
use funding_fee_farmer::exchange::{BinanceClient, MockBinanceClient};
use funding_fee_farmer::persistence::PersistenceManager;
use funding_fee_farmer::risk::{
    LiquidationAction, MarginHealth, MarginMonitor, PositionEntry, RiskAlertType, RiskOrchestrator,
    RiskOrchestratorConfig,
};
use funding_fee_farmer::strategy::{
    CapitalAllocator, HedgeRebalancer, MarketScanner, OrderExecutor, RebalanceConfig,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::EnvFilter;

/// Funding Fee Farmer CLI
#[derive(Parser)]
#[command(name = "funding-fee-farmer")]
#[command(version, about = "Delta-neutral funding fee farming on Binance")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a backtest simulation on historical data
    Backtest {
        /// Path to CSV data file
        #[arg(short, long)]
        data: String,

        /// Start date (YYYY-MM-DD)
        #[arg(short, long)]
        start: String,

        /// End date (YYYY-MM-DD)
        #[arg(short, long)]
        end: String,

        /// Initial balance for simulation
        #[arg(short = 'b', long, default_value = "10000")]
        initial_balance: f64,

        /// Output directory for results
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Run a parameter sweep optimization
    Sweep {
        /// Path to CSV data file
        #[arg(short, long)]
        data: String,

        /// Start date (YYYY-MM-DD)
        #[arg(short, long)]
        start: String,

        /// End date (YYYY-MM-DD)
        #[arg(short, long)]
        end: String,

        /// Initial balance for simulation
        #[arg(short = 'b', long, default_value = "10000")]
        initial_balance: f64,

        /// Number of parallel backtests
        #[arg(short, long, default_value = "4")]
        parallelism: usize,

        /// Output directory for results
        #[arg(short, long)]
        output: Option<String>,

        /// Use minimal parameter space (faster, for testing)
        #[arg(long)]
        minimal: bool,
    },

    /// Show current mock farmer status from persisted state
    Status {
        /// Path to SQLite database (default: data/mock_state.db)
        #[arg(short, long, default_value = "data/mock_state.db")]
        db: String,

        /// Show detailed position information
        #[arg(short, long)]
        verbose: bool,
    },
}

/// Trading mode: Live (real money) or Mock (paper trading).
#[derive(Debug, Clone, Copy, PartialEq)]
enum TradingMode {
    Live,
    Mock,
}

/// Application state for logging and monitoring.
#[derive(Debug)]
struct AppMetrics {
    start_time: DateTime<Utc>,
    scan_count: u64,
    opportunities_found: u64,
    positions_entered: u64,
    positions_exited: u64,
    rebalances_triggered: u64,
    funding_collections: u64,
    errors_count: u64,
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self {
            start_time: Utc::now(),
            scan_count: 0,
            opportunities_found: 0,
            positions_entered: 0,
            positions_exited: 0,
            rebalances_triggered: 0,
            funding_collections: 0,
            errors_count: 0,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI arguments
    let cli = Cli::parse();

    // Initialize comprehensive logging
    init_logging()?;

    // Handle subcommands
    match cli.command {
        Some(Commands::Backtest {
            data,
            start,
            end,
            initial_balance,
            output,
        }) => {
            return run_backtest(&data, &start, &end, initial_balance, output.as_deref()).await;
        }
        Some(Commands::Sweep {
            data,
            start,
            end,
            initial_balance,
            parallelism,
            output,
            minimal,
        }) => {
            return run_sweep(
                &data,
                &start,
                &end,
                initial_balance,
                parallelism,
                output.as_deref(),
                minimal,
            )
            .await;
        }
        Some(Commands::Status { db, verbose }) => {
            return show_status(&db, verbose);
        }
        None => {
            // Default: run trading mode
        }
    }

    info!("╔════════════════════════════════════════════════════════════╗");
    info!(
        "║       Funding Fee Farmer v{} - MVP Paper Trading        ║",
        env!("CARGO_PKG_VERSION")
    );
    info!("╚════════════════════════════════════════════════════════════╝");

    // Determine trading mode from environment
    let trading_mode = if std::env::var("LIVE_TRADING").unwrap_or_default() == "true" {
        warn!("⚠️  LIVE TRADING MODE - Real money at risk!");
        TradingMode::Live
    } else {
        info!("📝 MOCK TRADING MODE - Paper trading enabled");
        TradingMode::Mock
    };

    // Load configuration
    let config = Config::load()?;
    log_config(&config);

    // Initialize components
    let scanner = MarketScanner::new(config.pair_selection.clone());
    let allocator = CapitalAllocator::new(
        config.capital.clone(),
        config.risk.clone(),
        config.execution.default_leverage,
    );
    let mut executor = OrderExecutor::new(config.execution.clone());
    let rebalancer = HedgeRebalancer::new(RebalanceConfig::default());

    // Initialize clients
    // For MVP mock trading, we create a real client only if credentials are available
    let binance_config = funding_fee_farmer::config::BinanceConfig {
        api_key: std::env::var("BINANCE_API_KEY").unwrap_or_default(),
        secret_key: std::env::var("BINANCE_SECRET_KEY").unwrap_or_default(),
        testnet: false,
    };

    let real_client = match BinanceClient::new(&binance_config) {
        Ok(client) => {
            if binance_config.api_key.is_empty() {
                info!("⚠️  No API keys provided. Running in Read-Only/Mock mode.");
            }
            client
        }
        Err(e) => {
            error!("Failed to create Binance client: {}", e);
            return Err(e);
        }
    };

    let mock_client = MockBinanceClient::new(dec!(10000)); // $10k paper trading default

    // Initialize SQLite persistence for mock state
    let persistence = PersistenceManager::new("data/mock_state.db")
        .expect("Failed to initialize persistence database");

    // Try to restore previous state
    let initial_balance = if let Ok(Some(persisted_state)) = persistence.load_state() {
        info!("📂 [PERSISTENCE] Restoring state from database");
        info!(
            "   Balance: ${:.2}, Positions: {}, Total Funding: ${:.4}",
            persisted_state.balance,
            persisted_state.positions.len(),
            persisted_state.total_funding_received
        );
        let balance = persisted_state.balance;
        mock_client.restore_state(persisted_state).await;
        balance
    } else {
        info!("📂 [PERSISTENCE] No previous state found, starting fresh with $10,000");
        dec!(10000)
    };

    // Initialize RiskOrchestrator with comprehensive risk monitoring
    let risk_config = RiskOrchestratorConfig {
        max_drawdown: config.risk.max_drawdown,
        min_margin_ratio: config.risk.min_margin_ratio,
        max_single_position: config.risk.max_single_position,
        min_holding_period_hours: config.risk.min_holding_period_hours,
        min_yield_advantage: config.risk.min_yield_advantage,
        max_unprofitable_hours: config.risk.max_unprofitable_hours,
        min_expected_yield: config.risk.min_expected_yield,
        grace_period_hours: config.risk.grace_period_hours,
        max_funding_deviation: config.risk.max_funding_deviation,
        max_errors_per_minute: config.risk.max_errors_per_minute,
        max_consecutive_failures: config.risk.max_consecutive_failures,
        emergency_delta_drift: config.risk.emergency_delta_drift,
        max_consecutive_risk_cycles: config.risk.max_consecutive_risk_cycles,
    };
    let mut risk_orchestrator = RiskOrchestrator::new(risk_config, initial_balance);

    // Initialize precisions
    match real_client.get_futures_exchange_info().await {
        Ok(info) => {
            let precisions = info
                .symbols
                .into_iter()
                .map(|s| (s.symbol, s.quantity_precision))
                .collect();
            executor.set_precisions(precisions);
            info!("✅ [INIT] Futures exchange info loaded");
        }
        Err(e) => {
            warn!("⚠️  [INIT] Failed to load exchange info: {}", e);
            if trading_mode == TradingMode::Live {
                // In Live mode, we might want to panic, but for now we warn
                error!(
                    "❌ LIVE Mode warning: Exchange info failed. Precision defaults will be used."
                );
            }
        }
    }

    // Metrics tracking
    let mut metrics = AppMetrics::default();

    // Shutdown signal
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("🛑 Shutdown signal received");
        shutdown_clone.store(true, Ordering::SeqCst);
    });

    info!("🚀 Starting main trading loop...");
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Track last funding collection time and state saves
    let mut last_funding_hour: Option<u32> = None;
    let mut last_status_log = Utc::now();
    let mut last_state_save = Utc::now();

    // Main trading loop
    while !shutdown.load(Ordering::SeqCst) {
        let loop_start = Utc::now();

        // ═══════════════════════════════════════════════════════════════
        // PHASE 1: Market Scanning
        // ═══════════════════════════════════════════════════════════════
        info!("📡 [SCAN] Starting market scan #{}", metrics.scan_count + 1);

        let scan_result = scanner.scan(&real_client).await;
        metrics.scan_count += 1;

        let qualified_pairs = match scan_result {
            Ok(pairs) => {
                info!("📊 [SCAN] Found {} qualified pairs", pairs.len());
                for (i, pair) in pairs.iter().take(5).enumerate() {
                    info!(
                        "   #{}: {} | Funding: {:.4}% | Volume: ${:.0}M | Score: {:.2}",
                        i + 1,
                        pair.symbol,
                        pair.funding_rate * dec!(100),
                        pair.volume_24h / dec!(1_000_000),
                        pair.score
                    );
                }
                metrics.opportunities_found += pairs.len() as u64;
                pairs
            }
            Err(e) => {
                error!("❌ [SCAN] Failed: {}", e);
                metrics.errors_count += 1;
                Vec::new()
            }
        };

        // ═══════════════════════════════════════════════════════════════
        // PHASE 2: Malfunction Check
        // ═══════════════════════════════════════════════════════════════
        if risk_orchestrator.check_malfunctions() {
            error!("🚨 [RISK] Trading halted due to detected malfunction!");
            // Log active alerts
            for alert in risk_orchestrator.get_active_alerts() {
                error!(
                    "   Alert: {} - {:?}",
                    alert.message,
                    alert.malfunction_type
                );
            }
            // Wait longer before retrying
            tokio::time::sleep(Duration::from_secs(300)).await;
            continue;
        }

        // ═══════════════════════════════════════════════════════════════
        // PHASE 3: Capital Allocation
        // ═══════════════════════════════════════════════════════════════
        if !qualified_pairs.is_empty() {
            // Get prices first so we can convert position quantities to USDT values
            let prices = fetch_prices(&real_client, &qualified_pairs).await;

            // Convert position quantities to USDT values for the allocator
            // The allocator compares target_size (USDT) with current position (must also be USDT)
            let current_positions: HashMap<String, Decimal> = if trading_mode == TradingMode::Mock {
                mock_client
                    .get_delta_neutral_positions()
                    .await
                    .into_iter()
                    .map(|p| {
                        let price = prices.get(&p.symbol).copied().unwrap_or(Decimal::ONE);
                        let position_value_usdt = p.futures_qty.abs() * price;
                        (p.symbol, position_value_usdt)
                    })
                    .collect()
            } else {
                match fetch_real_positions(&real_client).await {
                    Ok(pos) => pos,
                    Err(_) => HashMap::new(),
                }
            };

            let mock_state = mock_client.get_state().await;

            // DEBUG: Log current positions with values (elevated to INFO for visibility)
            info!(
                "📊 [POSITIONS] current_positions ({} entries): {:?}",
                current_positions.len(),
                current_positions.iter().map(|(k, v)| format!("{}=${:.2}", k, v)).collect::<Vec<_>>()
            );

            let allocations = allocator.calculate_allocation(
                &qualified_pairs,
                mock_state.balance,
                &current_positions,
            );

            if !allocations.is_empty() {
                info!("💰 [ALLOCATE] {} new positions to enter", allocations.len());
                for alloc in &allocations {
                    info!(
                        "   {} | Size: ${:.2} | Leverage: {}x | Funding: {:.4}%",
                        alloc.symbol,
                        alloc.target_size_usdt,
                        alloc.leverage,
                        alloc.funding_rate * dec!(100)
                    );
                }

                // ═══════════════════════════════════════════════════════════════
                // PHASE 4: Order Execution (Mock)
                // ═══════════════════════════════════════════════════════════════
                if trading_mode == TradingMode::Mock {
                    // Update mock client with real prices (prices already fetched above)
                    let funding_rates: HashMap<String, Decimal> = qualified_pairs
                        .iter()
                        .map(|p| (p.symbol.clone(), p.funding_rate))
                        .collect();
                    mock_client
                        .update_market_data(funding_rates, prices.clone())
                        .await;

                    for alloc in allocations.iter().take(2) {
                        // Limit to top 2 for MVP
                        let price = prices.get(&alloc.symbol).copied().unwrap_or(dec!(50000));

                        // Get current position size for this symbol
                        let current_position_qty = current_positions
                            .get(&alloc.symbol)
                            .copied()
                            .unwrap_or(Decimal::ZERO) / price;

                        // Calculate target quantity
                        let target_qty = (alloc.target_size_usdt / price).round_dp(4);

                        // Calculate delta - only ADD to position, never reduce here
                        // (Reductions are handled by rebalancer)
                        let delta_qty = target_qty - current_position_qty.abs();

                        // DEBUG: Log what we're looking up (elevated to INFO)
                        info!(
                            "🔍 [LOOKUP] {} - has_key: {}, usdt_value: {:?}, qty: {:.4}",
                            alloc.symbol,
                            current_positions.contains_key(&alloc.symbol),
                            current_positions.get(&alloc.symbol),
                            current_position_qty
                        );

                        // Skip if position already exists or delta is too small
                        if current_position_qty.abs() > Decimal::ZERO {
                            info!(
                                "⏩ [SKIP] {} already has position: {:.4} qty (target: {:.4})",
                                alloc.symbol, current_position_qty, target_qty
                            );
                            continue;
                        }

                        if delta_qty <= Decimal::ZERO {
                            info!(
                                "⏩ [SKIP] {} delta is zero or negative: {:.4}",
                                alloc.symbol, delta_qty
                            );
                            continue;
                        }

                        // Pre-flight margin health check - ensure new position won't degrade margin to Orange/Red
                        let current_total_positions: Decimal = current_positions.values().sum();
                        let projected_health = MarginMonitor::simulate_position_entry(
                            current_total_positions,
                            mock_state.balance,
                            alloc.target_size_usdt,
                            alloc.leverage,
                            None, // Use default 0.5% maintenance rate
                        );

                        match projected_health {
                            MarginHealth::Orange | MarginHealth::Red => {
                                warn!(
                                    "⏩ [SKIP] {} - pre-flight check: projected margin health {:?} too risky",
                                    alloc.symbol, projected_health
                                );
                                continue;
                            }
                            _ => {
                                debug!(
                                    "✓ [PRE-FLIGHT] {} - projected health {:?} acceptable",
                                    alloc.symbol, projected_health
                                );
                            }
                        }

                        info!("📈 [EXECUTE] Entering NEW position: {} (qty: {:.4})", alloc.symbol, target_qty);

                        // Calculate quantity - only enter new positions, not adjustments
                        let quantity = target_qty;

                        // Determine sides based on funding direction
                        let (futures_side, spot_side) = if alloc.funding_rate > Decimal::ZERO {
                            (
                                funding_fee_farmer::exchange::OrderSide::Sell,
                                funding_fee_farmer::exchange::OrderSide::Buy,
                            )
                        } else {
                            (
                                funding_fee_farmer::exchange::OrderSide::Buy,
                                funding_fee_farmer::exchange::OrderSide::Sell,
                            )
                        };

                        // Execute futures order
                        let futures_order = funding_fee_farmer::exchange::NewOrder {
                            symbol: alloc.symbol.clone(),
                            side: futures_side,
                            position_side: None,
                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                            quantity: Some(quantity),
                            price: None,
                            time_in_force: None,
                            reduce_only: None,
                            new_client_order_id: None,
                        };

                        if let Err(e) = mock_client.place_futures_order(&futures_order).await {
                            error!("❌ [EXECUTE] Futures order failed: {}", e);
                            metrics.errors_count += 1;
                            risk_orchestrator.record_error(&format!("Futures order failed: {}", e));
                            risk_orchestrator.record_order_failure(&alloc.symbol);
                            continue;
                        }
                        risk_orchestrator.record_order_success(&alloc.symbol);

                        // Execute spot hedge
                        let spot_order = funding_fee_farmer::exchange::MarginOrder {
                            symbol: alloc.spot_symbol.clone(),
                            side: spot_side,
                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                            quantity: Some(quantity),
                            price: None,
                            time_in_force: None,
                            is_isolated: Some(false),
                            side_effect_type: Some(
                                funding_fee_farmer::exchange::SideEffectType::AutoBorrowRepay,
                            ),
                        };

                        if let Err(e) = mock_client.place_margin_order(&spot_order).await {
                            error!("❌ [EXECUTE] Spot hedge failed: {}", e);
                            metrics.errors_count += 1;
                            risk_orchestrator.record_error(&format!("Spot hedge failed: {}", e));
                            risk_orchestrator.record_order_failure(&alloc.spot_symbol);

                            // Unwind the futures position to avoid directional exposure
                            let unwind_side = match futures_side {
                                funding_fee_farmer::exchange::OrderSide::Buy => {
                                    funding_fee_farmer::exchange::OrderSide::Sell
                                }
                                funding_fee_farmer::exchange::OrderSide::Sell => {
                                    funding_fee_farmer::exchange::OrderSide::Buy
                                }
                            };

                            let unwind_order = funding_fee_farmer::exchange::NewOrder {
                                symbol: alloc.symbol.clone(),
                                side: unwind_side,
                                position_side: None,
                                order_type: funding_fee_farmer::exchange::OrderType::Market,
                                quantity: Some(quantity),
                                price: None,
                                time_in_force: None,
                                reduce_only: Some(true),
                                new_client_order_id: None,
                            };

                            if let Err(unwind_err) =
                                mock_client.place_futures_order(&unwind_order).await
                            {
                                error!(
                                    "❌ [EXECUTE] CRITICAL: Failed to unwind futures position: {}",
                                    unwind_err
                                );
                            } else {
                                warn!(
                                    "⚠️  [EXECUTE] Unwound futures for {} due to spot hedge failure",
                                    alloc.symbol
                                );
                            }
                            continue;
                        }

                        info!(
                            "✅ [EXECUTE] Position entered: {} | Qty: {} | Price: ${}",
                            alloc.symbol, quantity, price
                        );
                        metrics.positions_entered += 1;

                        // Track position for risk monitoring
                        let entry = PositionEntry {
                            symbol: alloc.symbol.clone(),
                            entry_price: price,
                            quantity,
                            position_value: alloc.target_size_usdt,
                            expected_funding_rate: alloc.funding_rate,
                            entry_fees: alloc.target_size_usdt * dec!(0.0004), // ~0.04% taker fee
                        };
                        risk_orchestrator.open_position(entry);
                    }
                } else {
                    // LIVE TRADING EXECUTION
                    let prices = fetch_prices(&real_client, &qualified_pairs).await;

                    for alloc in &allocations {
                        let price = prices.get(&alloc.symbol).copied().unwrap_or(dec!(0));
                        if price == Decimal::ZERO {
                            warn!("Skipping {} due to missing price", alloc.symbol);
                            continue;
                        }

                        match executor.enter_position(&real_client, alloc, price).await {
                            Ok(result) => {
                                if result.success {
                                    info!("✅ [EXECUTE] Entered position for {}", result.symbol);
                                    metrics.positions_entered += 1;
                                } else {
                                    error!(
                                        "❌ [EXECUTE] Failed to enter {}: {:?}",
                                        result.symbol, result.error
                                    );
                                    metrics.errors_count += 1;
                                }
                            }
                            Err(e) => {
                                error!("❌ [EXECUTE] Error executing {}: {}", alloc.symbol, e);
                                metrics.errors_count += 1;
                            }
                        }
                    }
                }
            }

            // ═══════════════════════════════════════════════════════════════
            // PHASE 4.5: Position Size Rebalancing
            // Reduce oversized positions to free capital for better opportunities
            // ═══════════════════════════════════════════════════════════════
            let candidate_reductions = allocator.calculate_reductions(
                &qualified_pairs,
                mock_state.balance, // Use mock_state balance for consistency with allocation
                &current_positions,
            );

            // Filter reductions based on minimum holding period and yield advantage
            let reductions: Vec<_> = candidate_reductions
                .into_iter()
                .filter(|reduction| {
                    // Check if position is within minimum holding period
                    if let Some(tracked) = risk_orchestrator.get_tracked_position(&reduction.symbol) {
                        let within_holding = tracked.is_within_holding_period(
                            config.risk.min_holding_period_hours,
                        );

                        if within_holding {
                            // Position is protected by holding period
                            // Only allow reduction if there's a significant yield advantage elsewhere

                            // Find the best alternative opportunity
                            let best_alternative_rate = qualified_pairs
                                .iter()
                                .filter(|p| p.symbol != reduction.symbol)
                                .map(|p| p.funding_rate.abs())
                                .max()
                                .unwrap_or(Decimal::ZERO);

                            let current_rate = tracked.expected_funding_rate.abs();
                            let yield_advantage = best_alternative_rate - current_rate;

                            if yield_advantage < config.risk.min_yield_advantage {
                                info!(
                                    "🛡️  [PROTECT] {} within {}h holding period (opened {:.1}h ago). \
                                     Yield advantage {:.4}% < required {:.2}%",
                                    reduction.symbol,
                                    config.risk.min_holding_period_hours,
                                    tracked.hours_open(),
                                    yield_advantage * dec!(100),
                                    config.risk.min_yield_advantage * dec!(100)
                                );
                                return false; // Skip this reduction
                            } else {
                                info!(
                                    "📊 [YIELD] {} has significant yield advantage ({:.4}% > {:.2}%) - allowing early reduction",
                                    reduction.symbol,
                                    yield_advantage * dec!(100),
                                    config.risk.min_yield_advantage * dec!(100)
                                );
                            }
                        }
                    }
                    true // Allow reduction
                })
                .collect();

            if !reductions.is_empty() {
                info!("📉 [REDUCE] {} positions need reduction", reductions.len());
                for reduction in &reductions {
                    info!(
                        "   {} | Current: ${:.2} | Target: ${:.2} | Reduce: ${:.2}",
                        reduction.symbol,
                        reduction.current_size_usdt,
                        reduction.target_size_usdt,
                        reduction.reduction_usdt
                    );
                }

                if trading_mode == TradingMode::Mock {
                    let prices = fetch_prices(&real_client, &qualified_pairs).await;

                    for reduction in &reductions {
                        let price = prices.get(&reduction.symbol).copied().unwrap_or(dec!(50000));
                        let reduction_qty = (reduction.reduction_usdt / price).round_dp(4);

                        if reduction_qty <= Decimal::ZERO {
                            continue;
                        }

                        // Get current position to determine direction
                        let positions = mock_client.get_delta_neutral_positions().await;
                        let futures_position = positions
                            .iter()
                            .find(|p| p.symbol == reduction.symbol)
                            .map(|p| p.futures_qty)
                            .unwrap_or(Decimal::ZERO);

                        let is_short = futures_position < Decimal::ZERO;

                        info!(
                            "📉 [REDUCE] Reducing {} by {:.4} qty (is_short: {})",
                            reduction.symbol, reduction_qty, is_short
                        );

                        // Close part of futures position
                        let futures_close_side = if is_short {
                            funding_fee_farmer::exchange::OrderSide::Buy
                        } else {
                            funding_fee_farmer::exchange::OrderSide::Sell
                        };

                        let futures_order = funding_fee_farmer::exchange::NewOrder {
                            symbol: reduction.symbol.clone(),
                            side: futures_close_side,
                            position_side: None,
                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                            quantity: Some(reduction_qty),
                            price: None,
                            time_in_force: None,
                            reduce_only: Some(true),
                            new_client_order_id: None,
                        };

                        match mock_client.place_futures_order(&futures_order).await {
                            Ok(_) => {
                                info!("✅ [REDUCE] Reduced futures position for {}", reduction.symbol);
                            }
                            Err(e) => {
                                error!("❌ [REDUCE] Failed to reduce futures for {}: {}", reduction.symbol, e);
                                metrics.errors_count += 1;
                                continue;
                            }
                        }

                        // Close matching spot position
                        let spot_close_side = if is_short {
                            funding_fee_farmer::exchange::OrderSide::Sell // Sell spot hedge
                        } else {
                            funding_fee_farmer::exchange::OrderSide::Buy // Buy back shorted spot
                        };

                        let side_effect = if is_short {
                            funding_fee_farmer::exchange::SideEffectType::NoSideEffect
                        } else {
                            funding_fee_farmer::exchange::SideEffectType::AutoRepay
                        };

                        let spot_order = funding_fee_farmer::exchange::MarginOrder {
                            symbol: reduction.spot_symbol.clone(),
                            side: spot_close_side,
                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                            quantity: Some(reduction_qty),
                            price: None,
                            time_in_force: None,
                            is_isolated: Some(false),
                            side_effect_type: Some(side_effect),
                        };

                        match mock_client.place_margin_order(&spot_order).await {
                            Ok(_) => {
                                info!("✅ [REDUCE] Reduced spot position for {}", reduction.spot_symbol);
                                metrics.rebalances_triggered += 1;
                            }
                            Err(e) => {
                                warn!("⚠️  [REDUCE] Spot reduction failed for {}: {} (delta drift may occur)",
                                    reduction.spot_symbol, e);
                            }
                        }
                    }
                } else {
                    // LIVE TRADING: Execute reductions
                    let prices = fetch_prices(&real_client, &qualified_pairs).await;
                    let positions = real_client.get_positions().await.unwrap_or_default();

                    for reduction in &reductions {
                        let price = prices.get(&reduction.symbol).copied().unwrap_or(Decimal::ZERO);
                        if price == Decimal::ZERO {
                            warn!("Skipping reduction for {} due to missing price", reduction.symbol);
                            continue;
                        }

                        let futures_position = positions
                            .iter()
                            .find(|p| p.symbol == reduction.symbol)
                            .map(|p| p.position_amt)
                            .unwrap_or(Decimal::ZERO);

                        match executor.reduce_position(&real_client, reduction, price, futures_position).await {
                            Ok(result) => {
                                if result.success {
                                    info!("✅ [REDUCE] Reduced position for {}", result.symbol);
                                    metrics.rebalances_triggered += 1;
                                } else {
                                    error!("❌ [REDUCE] Failed to reduce {}: {:?}", result.symbol, result.error);
                                    metrics.errors_count += 1;
                                }
                            }
                            Err(e) => {
                                error!("❌ [REDUCE] Error reducing {}: {}", reduction.symbol, e);
                                metrics.errors_count += 1;
                            }
                        }
                    }
                }
            }
        }

        // ═══════════════════════════════════════════════════════════════
        // PHASE 5: Hedge Rebalancing
        // ═══════════════════════════════════════════════════════════════
        if trading_mode == TradingMode::Mock {
            let positions = mock_client.get_delta_neutral_positions().await;
            if !positions.is_empty() {
                debug!(
                    "⚖️  [REBALANCE] Checking {} positions for delta drift",
                    positions.len()
                );

                let funding_rates: HashMap<String, Decimal> = qualified_pairs
                    .iter()
                    .map(|p| (p.symbol.clone(), p.funding_rate))
                    .collect();
                let prices = fetch_prices(&real_client, &qualified_pairs).await;

                for position in &positions {
                    let funding_rate = funding_rates
                        .get(&position.symbol)
                        .copied()
                        .unwrap_or(Decimal::ZERO);
                    let price = prices.get(&position.symbol).copied().unwrap_or(dec!(50000));

                    let action = rebalancer.analyze_position(position, funding_rate, price);

                    if !matches!(action, funding_fee_farmer::strategy::RebalanceAction::None) {
                        warn!(
                            "⚖️  [REBALANCE] Action needed for {}: {:?}",
                            position.symbol, action
                        );
                        metrics.rebalances_triggered += 1;

                        // Execute rebalance in mock mode
                        match &action {
                            funding_fee_farmer::strategy::RebalanceAction::AdjustSpot {
                                symbol,
                                side,
                                quantity,
                            } => {
                                let order = funding_fee_farmer::exchange::MarginOrder {
                                    symbol: symbol.clone(),
                                    side: *side,
                                    order_type: funding_fee_farmer::exchange::OrderType::Market,
                                    quantity: Some(*quantity),
                                    price: None,
                                    time_in_force: None,
                                    is_isolated: Some(false),
                                    side_effect_type: Some(
                                        funding_fee_farmer::exchange::SideEffectType::AutoBorrowRepay,
                                    ),
                                };

                                match mock_client.place_margin_order(&order).await {
                                    Ok(_) => {
                                        info!(
                                            "✅ [REBALANCE] Adjusted spot {} {:?} {}",
                                            symbol, side, quantity
                                        );
                                    }
                                    Err(e) => {
                                        error!("❌ [REBALANCE] Spot adjustment failed: {}", e);
                                        metrics.errors_count += 1;
                                    }
                                }
                            }
                            funding_fee_farmer::strategy::RebalanceAction::AdjustFutures {
                                symbol,
                                side,
                                quantity,
                            } => {
                                let order = funding_fee_farmer::exchange::NewOrder {
                                    symbol: symbol.clone(),
                                    side: *side,
                                    position_side: None,
                                    order_type: funding_fee_farmer::exchange::OrderType::Market,
                                    quantity: Some(*quantity),
                                    price: None,
                                    time_in_force: None,
                                    reduce_only: Some(true),
                                    new_client_order_id: None,
                                };

                                match mock_client.place_futures_order(&order).await {
                                    Ok(_) => {
                                        info!(
                                            "✅ [REBALANCE] Adjusted futures {} {:?} {}",
                                            symbol, side, quantity
                                        );
                                    }
                                    Err(e) => {
                                        error!("❌ [REBALANCE] Futures adjustment failed: {}", e);
                                        metrics.errors_count += 1;
                                    }
                                }
                            }
                            funding_fee_farmer::strategy::RebalanceAction::FlipPosition {
                                symbol,
                                new_funding_direction,
                            } => {
                                warn!(
                                    "⚠️  [REBALANCE] Position flip for {} to {:?} requires manual review",
                                    symbol, new_funding_direction
                                );
                                // Flipping is complex - log for now, would need to close both legs
                                // and re-enter with opposite direction
                            }
                            funding_fee_farmer::strategy::RebalanceAction::ClosePosition {
                                symbol,
                                spot_symbol: _,
                                futures_qty,
                                spot_qty,
                            } => {
                                warn!(
                                    "⚠️  [REBALANCE] Position close executed for {} (futures: {}, spot: {})",
                                    symbol, futures_qty, spot_qty
                                );
                            }
                            funding_fee_farmer::strategy::RebalanceAction::None => {}
                        }
                    }
                }
            }
        }

        // ═══════════════════════════════════════════════════════════════
        // PHASE 6: Funding Collection & Verification
        // ═══════════════════════════════════════════════════════════════
        let current_hour = Utc::now().hour();
        let is_funding_hour = current_hour == 0 || current_hour == 8 || current_hour == 16;

        if is_funding_hour && last_funding_hour != Some(current_hour) {
            if trading_mode == TradingMode::Mock {
                info!("💸 [FUNDING] Collecting funding payments...");
                let per_position_funding = mock_client.collect_funding().await;
                let total_funding: Decimal = per_position_funding.values().sum();
                info!("💸 [FUNDING] Received: ${:.4} across {} positions", total_funding, per_position_funding.len());
                metrics.funding_collections += 1;

                // Verify funding for each position using actual per-position data
                for (symbol, actual_funding) in &per_position_funding {
                    if risk_orchestrator.get_tracked_position(symbol).is_some() {
                        // Record and verify funding with actual per-position amount
                        risk_orchestrator.record_funding(symbol, *actual_funding);
                        let verification = risk_orchestrator.verify_funding(symbol, *actual_funding);

                        if verification.is_anomaly {
                            warn!(
                                "⚠️  [FUNDING] Anomaly for {}: expected ${:.4}, got ${:.4} ({:.1}% deviation)",
                                symbol,
                                verification.funding_expected,
                                verification.funding_received,
                                verification.deviation_pct * dec!(100)
                            );
                        }
                    }
                }
            }
            // Save state after funding collection (critical checkpoint)
            if trading_mode == TradingMode::Mock {
                let state_to_save = mock_client.export_state().await;
                if let Err(e) = persistence.save_state(&state_to_save) {
                    warn!("⚠️  [PERSISTENCE] Failed to save state after funding: {}", e);
                } else {
                    debug!("💾 [PERSISTENCE] State saved after funding collection");
                }
            }

            last_funding_hour = Some(current_hour);
        }

        // Accrue interest periodically
        if trading_mode == TradingMode::Mock {
            // accrue_interest now returns per-position interest amounts
            let per_position_interest = mock_client.accrue_interest(dec!(0.0167)).await; // ~1 minute in hours

            // Record actual per-position interest in risk tracker
            for (symbol, interest) in &per_position_interest {
                risk_orchestrator.record_interest(symbol, *interest);
            }
        }

        // ═══════════════════════════════════════════════════════════════
        // PHASE 7: Comprehensive Risk Check
        // ═══════════════════════════════════════════════════════════════
        if trading_mode == TradingMode::Mock {
            let state = mock_client.get_state().await;
            let (realized_pnl, unrealized_pnl) = mock_client.calculate_pnl().await;
            let total_equity = state.balance + unrealized_pnl;

            // Build position list for risk checks
            let positions = mock_client.get_delta_neutral_positions().await;
            let exchange_positions: Vec<funding_fee_farmer::exchange::Position> = positions
                .iter()
                .map(|p| funding_fee_farmer::exchange::Position {
                    symbol: p.symbol.clone(),
                    position_amt: p.futures_qty,
                    entry_price: p.futures_entry_price,
                    unrealized_profit: p.funding_pnl - p.interest_paid, // Net PnL
                    leverage: 5,
                    notional: p.futures_entry_price * p.futures_qty.abs(),
                    isolated_margin: Decimal::ZERO,
                    mark_price: p.futures_entry_price, // Simplified
                    liquidation_price: Decimal::ZERO,
                    position_side: funding_fee_farmer::exchange::PositionSide::Both,
                    margin_type: funding_fee_farmer::exchange::MarginType::Cross,
                })
                .collect();

            // Run comprehensive risk check
            // Mock mode: use default maintenance rate since we don't have real leverage brackets
            let maintenance_rates: HashMap<String, Decimal> = HashMap::new();
            let risk_result = risk_orchestrator.check_all(&exchange_positions, total_equity, state.balance, &maintenance_rates);

            // Check for drawdown warnings
            let drawdown_stats = risk_orchestrator.get_drawdown_stats();
            let max_drawdown = config.risk.max_drawdown;
            let distance = max_drawdown - drawdown_stats.current_drawdown;
            let warning_threshold = max_drawdown * dec!(0.2); // 20% buffer

            if distance <= warning_threshold {
                warn!(
                    current_dd = %drawdown_stats.current_drawdown,
                    distance_to_limit = %distance,
                    "⚠️  Approaching maximum drawdown - consider reducing exposure"
                );

                // Graduated response based on distance to limit
                let distance_pct = distance / max_drawdown;

                if distance_pct <= dec!(0.05) { // Within 5% of limit (95% threshold)
                    warn!("🚨 Drawdown at 95% of limit - reducing all positions by 25%");

                    for pos in &positions {
                        if pos.futures_qty.abs() < dec!(0.0001) {
                            continue; // Skip positions with negligible size
                        }

                        let reduce_qty = pos.futures_qty.abs() * dec!(0.25);

                        // Close 25% of futures position
                        let futures_side = if pos.futures_qty > Decimal::ZERO {
                            funding_fee_farmer::exchange::OrderSide::Sell
                        } else {
                            funding_fee_farmer::exchange::OrderSide::Buy
                        };

                        let futures_order = funding_fee_farmer::exchange::NewOrder {
                            symbol: pos.symbol.clone(),
                            side: futures_side,
                            position_side: None,
                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                            quantity: Some(reduce_qty),
                            price: None,
                            time_in_force: None,
                            reduce_only: Some(true),
                            new_client_order_id: None,
                        };

                        if let Err(e) = mock_client.place_futures_order(&futures_order).await {
                            error!("❌ Failed to reduce futures position for {}: {}", pos.symbol, e);
                        } else {
                            info!("✅ Reduced futures position for {} by 25%", pos.symbol);
                        }

                        // Close 25% of spot position
                        if pos.spot_qty.abs() >= dec!(0.0001) {
                            let spot_side = if pos.spot_qty > Decimal::ZERO {
                                funding_fee_farmer::exchange::OrderSide::Sell
                            } else {
                                funding_fee_farmer::exchange::OrderSide::Buy
                            };

                            let spot_order = funding_fee_farmer::exchange::MarginOrder {
                                symbol: pos.spot_symbol.clone(),
                                side: spot_side,
                                order_type: funding_fee_farmer::exchange::OrderType::Market,
                                quantity: Some(pos.spot_qty.abs() * dec!(0.25)),
                                price: None,
                                time_in_force: None,
                                is_isolated: Some(false),
                                side_effect_type: Some(funding_fee_farmer::exchange::SideEffectType::AutoBorrowRepay),
                            };

                            if let Err(e) = mock_client.place_margin_order(&spot_order).await {
                                error!("❌ Failed to reduce spot position for {}: {}", pos.spot_symbol, e);
                            } else {
                                info!("✅ Reduced spot position for {} by 25%", pos.spot_symbol);
                            }
                        }
                    }
                } else if distance_pct <= dec!(0.10) { // Within 10% of limit (90% threshold)
                    warn!("⚠️  Drawdown at 90% of limit - stopping new positions");
                    // Note: New position logic would need to check this condition
                    // For now, just log the warning
                } else {
                    // Between 80-90% of limit - just log warning (already done above)
                    info!("📊 Drawdown warning logged - monitoring closely");
                }
            }

            // Handle risk alerts
            if !risk_result.alerts.is_empty() {
                for alert in &risk_result.alerts {
                    match &alert.alert_type {
                        RiskAlertType::DrawdownExceeded { current, limit } => {
                            error!(
                                "🚨 [RISK] Drawdown {:.2}% exceeds limit {:.2}%!",
                                current * dec!(100),
                                limit * dec!(100)
                            );
                        }
                        RiskAlertType::MarginWarning { health, action } => {
                            warn!("⚠️  [RISK] Margin health: {:?} - {}", health, action);

                            // Automatic position reduction for margin health warnings
                            let reduction_pct = match health {
                                MarginHealth::Red => Some(dec!(0.50)),    // 50% reduction for critical
                                MarginHealth::Orange => Some(dec!(0.25)), // 25% reduction for warning
                                _ => None,
                            };

                            if let Some(pct) = reduction_pct {
                                info!("🤖 [AUTO-REDUCE] Executing {}% reduction for all positions due to {:?} margin health",
                                    pct * dec!(100), health);

                                for pos in &positions {
                                    if pos.futures_qty.abs() < dec!(0.0001) {
                                        continue;
                                    }

                                    let reduce_qty = pos.futures_qty.abs() * pct;

                                    // Reduce futures
                                    let futures_side = if pos.futures_qty > Decimal::ZERO {
                                        funding_fee_farmer::exchange::OrderSide::Sell
                                    } else {
                                        funding_fee_farmer::exchange::OrderSide::Buy
                                    };

                                    let futures_order = funding_fee_farmer::exchange::NewOrder {
                                        symbol: pos.symbol.clone(),
                                        side: futures_side,
                                        position_side: None,
                                        order_type: funding_fee_farmer::exchange::OrderType::Market,
                                        quantity: Some(reduce_qty),
                                        price: None,
                                        time_in_force: None,
                                        reduce_only: Some(true),
                                        new_client_order_id: None,
                                    };

                                    match mock_client.place_futures_order(&futures_order).await {
                                        Ok(_) => {
                                            info!("✅ [AUTO-REDUCE] Reduced futures {} by {}%", pos.symbol, pct * dec!(100));
                                            metrics.rebalances_triggered += 1;
                                        }
                                        Err(e) => {
                                            error!("❌ [AUTO-REDUCE] Futures reduction failed for {}: {}", pos.symbol, e);
                                            metrics.errors_count += 1;
                                        }
                                    }

                                    // Reduce spot
                                    if pos.spot_qty.abs() >= dec!(0.0001) {
                                        let spot_side = if pos.spot_qty > Decimal::ZERO {
                                            funding_fee_farmer::exchange::OrderSide::Sell
                                        } else {
                                            funding_fee_farmer::exchange::OrderSide::Buy
                                        };

                                        let spot_order = funding_fee_farmer::exchange::MarginOrder {
                                            symbol: pos.spot_symbol.clone(),
                                            side: spot_side,
                                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                                            quantity: Some(pos.spot_qty.abs() * pct),
                                            price: None,
                                            time_in_force: None,
                                            is_isolated: Some(false),
                                            side_effect_type: Some(funding_fee_farmer::exchange::SideEffectType::AutoBorrowRepay),
                                        };

                                        if let Err(e) = mock_client.place_margin_order(&spot_order).await {
                                            error!("❌ [AUTO-REDUCE] Spot reduction failed for {}: {}", pos.spot_symbol, e);
                                        } else {
                                            info!("✅ [AUTO-REDUCE] Reduced spot {} by {}%", pos.spot_symbol, pct * dec!(100));
                                        }
                                    }
                                }
                            }
                        }
                        RiskAlertType::PositionLoss { symbol, reason, hours } => {
                            warn!(
                                "⚠️  [RISK] Position {} unprofitable: {} ({}h)",
                                symbol, reason, hours
                            );
                        }
                        RiskAlertType::FundingAnomaly { symbol, deviation } => {
                            warn!(
                                "⚠️  [RISK] Funding anomaly {}: {:.1}% deviation",
                                symbol,
                                deviation * dec!(100)
                            );
                        }
                        RiskAlertType::Malfunction { malfunction_type } => {
                            error!("🚨 [RISK] Malfunction detected: {:?}", malfunction_type);
                        }
                        RiskAlertType::LiquidationRisk { action } => {
                            error!("🚨 [RISK] Liquidation risk! Action: {:?}", action);

                            // Automatic position reduction for liquidation risk
                            match action {
                                LiquidationAction::ReducePosition { symbol, reduction_pct } => {
                                    info!("🤖 [AUTO-REDUCE] Executing {}% reduction for {}", reduction_pct * dec!(100), symbol);

                                    if let Some(pos) = positions.iter().find(|p| &p.symbol == symbol) {
                                        let reduce_qty = pos.futures_qty.abs() * *reduction_pct;

                                        if reduce_qty >= dec!(0.0001) {
                                            // Close portion of futures
                                            let futures_side = if pos.futures_qty > Decimal::ZERO {
                                                funding_fee_farmer::exchange::OrderSide::Sell
                                            } else {
                                                funding_fee_farmer::exchange::OrderSide::Buy
                                            };

                                            let futures_order = funding_fee_farmer::exchange::NewOrder {
                                                symbol: symbol.clone(),
                                                side: futures_side,
                                                position_side: None,
                                                order_type: funding_fee_farmer::exchange::OrderType::Market,
                                                quantity: Some(reduce_qty),
                                                price: None,
                                                time_in_force: None,
                                                reduce_only: Some(true),
                                                new_client_order_id: None,
                                            };

                                            match mock_client.place_futures_order(&futures_order).await {
                                                Ok(_) => {
                                                    info!("✅ [AUTO-REDUCE] Reduced futures {} by {}%", symbol, reduction_pct * dec!(100));
                                                    metrics.rebalances_triggered += 1;
                                                }
                                                Err(e) => {
                                                    error!("❌ [AUTO-REDUCE] Futures reduction failed for {}: {}", symbol, e);
                                                    metrics.errors_count += 1;
                                                }
                                            }

                                            // Close matching spot position
                                            let spot_reduce_qty = pos.spot_qty.abs() * *reduction_pct;
                                            if spot_reduce_qty >= dec!(0.0001) {
                                                let spot_side = if pos.spot_qty > Decimal::ZERO {
                                                    funding_fee_farmer::exchange::OrderSide::Sell
                                                } else {
                                                    funding_fee_farmer::exchange::OrderSide::Buy
                                                };

                                                let spot_order = funding_fee_farmer::exchange::MarginOrder {
                                                    symbol: pos.spot_symbol.clone(),
                                                    side: spot_side,
                                                    order_type: funding_fee_farmer::exchange::OrderType::Market,
                                                    quantity: Some(spot_reduce_qty),
                                                    price: None,
                                                    time_in_force: None,
                                                    is_isolated: Some(false),
                                                    side_effect_type: Some(funding_fee_farmer::exchange::SideEffectType::AutoBorrowRepay),
                                                };

                                                if let Err(e) = mock_client.place_margin_order(&spot_order).await {
                                                    error!("❌ [AUTO-REDUCE] Spot reduction failed for {}: {}", pos.spot_symbol, e);
                                                } else {
                                                    info!("✅ [AUTO-REDUCE] Reduced spot {} by {}%", pos.spot_symbol, reduction_pct * dec!(100));
                                                }
                                            }
                                        }
                                    }
                                }
                                LiquidationAction::ClosePosition { symbol } => {
                                    warn!("🤖 [AUTO-CLOSE] Position {} flagged for emergency close", symbol);
                                    // This will be handled by positions_to_close below
                                }
                                _ => {}
                            }
                        }
                        RiskAlertType::DeltaDrift { symbol, drift_pct } => {
                            warn!(
                                "⚠️  [RISK] Delta drift on {}: {:.2}%",
                                symbol,
                                drift_pct * dec!(100)
                            );
                        }
                    }
                }
            }

            // Handle positions to close
            for symbol in &risk_result.positions_to_close {
                warn!("🚨 [RISK] Position {} flagged for closure by risk orchestrator", symbol);

                // Find position data for this symbol
                if let Some(pos) = positions.iter().find(|p| &p.symbol == symbol) {
                    info!(
                        "🔄 [RISK] Executing position closure for {} (futures: {}, spot: {})",
                        symbol, pos.futures_qty, pos.spot_qty
                    );

                    let mut close_success = true;
                    let mut close_errors = Vec::new();

                    // Step 1: Close futures leg
                    if pos.futures_qty != Decimal::ZERO {
                        let futures_side = if pos.futures_qty > Decimal::ZERO {
                            funding_fee_farmer::exchange::OrderSide::Sell
                        } else {
                            funding_fee_farmer::exchange::OrderSide::Buy
                        };

                        let futures_order = funding_fee_farmer::exchange::NewOrder {
                            symbol: pos.symbol.clone(),
                            side: futures_side,
                            position_side: None,
                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                            quantity: Some(pos.futures_qty.abs()),
                            price: None,
                            time_in_force: None,
                            reduce_only: Some(true),
                            new_client_order_id: None,
                        };

                        if let Err(e) = mock_client.place_futures_order(&futures_order).await {
                            close_success = false;
                            close_errors.push(format!("Futures: {}", e));
                        }
                    }

                    // Step 2: Close spot leg
                    if pos.spot_qty != Decimal::ZERO {
                        let spot_side = if pos.spot_qty > Decimal::ZERO {
                            funding_fee_farmer::exchange::OrderSide::Sell
                        } else {
                            funding_fee_farmer::exchange::OrderSide::Buy
                        };

                        let spot_order = funding_fee_farmer::exchange::MarginOrder {
                            symbol: pos.spot_symbol.clone(),
                            side: spot_side,
                            order_type: funding_fee_farmer::exchange::OrderType::Market,
                            quantity: Some(pos.spot_qty.abs()),
                            price: None,
                            time_in_force: None,
                            is_isolated: Some(false),
                            side_effect_type: Some(funding_fee_farmer::exchange::SideEffectType::AutoBorrowRepay),
                        };

                        if let Err(e) = mock_client.place_margin_order(&spot_order).await {
                            close_success = false;
                            close_errors.push(format!("Spot: {}", e));
                        }
                    }

                    if close_success {
                        info!("✅ [RISK] Successfully closed position {}", symbol);
                        risk_orchestrator.close_position(symbol);
                        metrics.positions_exited += 1;
                    } else {
                        error!(
                            "❌ [RISK] Failed to close position {}: {}",
                            symbol, close_errors.join("; ")
                        );
                        risk_orchestrator.record_error(&format!(
                            "Position close failed for {}: {}",
                            symbol, close_errors.join("; ")
                        ));
                    }
                } else {
                    warn!("⚠️  [RISK] Position {} not found in active positions", symbol);
                }
            }

            // Check halt conditions
            if risk_result.should_halt {
                error!("🚨 [RISK] CRITICAL: Trading halted by risk orchestrator!");
                break;
            }

            // Log status every 5 minutes
            if (Utc::now() - last_status_log).num_minutes() >= 5 {
                log_status_with_risk(
                    &metrics,
                    &state,
                    realized_pnl,
                    unrealized_pnl,
                    &risk_orchestrator,
                );
                last_status_log = Utc::now();
            }
        } else {
            // Live Mode Risk Check
            if let Ok(balances) = real_client.get_account_balance().await {
                let total_equity: Decimal = balances
                    .iter()
                    .map(|b| b.wallet_balance + b.unrealized_profit)
                    .sum();

                let margin_balance: Decimal = balances
                    .iter()
                    .map(|b| b.wallet_balance)
                    .sum();

                // Get positions for live mode
                let live_positions = match real_client.get_positions().await {
                    Ok(pos) => pos.into_iter().filter(|p| p.position_amt != Decimal::ZERO).collect(),
                    Err(_) => vec![],
                };

                // Build maintenance rate map from leverage brackets
                let maintenance_rates = match real_client.get_leverage_brackets().await {
                    Ok(brackets) => MarginMonitor::build_maintenance_rate_map(&brackets, &live_positions),
                    Err(_) => HashMap::new(), // Fallback to default rates
                };

                let risk_result = risk_orchestrator.check_all(&live_positions, total_equity, margin_balance, &maintenance_rates);

                if risk_result.should_halt {
                    error!("🚨 [RISK] CRITICAL: Trading halted by risk orchestrator!");
                    break;
                }
            }
        }

        // Periodic state save (hourly) for crash recovery
        if trading_mode == TradingMode::Mock {
            let now = Utc::now();
            if (now - last_state_save).num_minutes() >= 60 {
                let state_to_save = mock_client.export_state().await;
                if let Err(e) = persistence.save_state(&state_to_save) {
                    warn!("⚠️  [PERSISTENCE] Failed periodic state save: {}", e);
                } else {
                    info!("💾 [PERSISTENCE] Hourly state checkpoint saved");
                    // Also record equity snapshot for analysis
                    let (realized_pnl, unrealized_pnl) = mock_client.calculate_pnl().await;
                    let total_equity = state_to_save.balance + unrealized_pnl;
                    let max_drawdown = risk_orchestrator.get_drawdown_stats().session_mdd;
                    let _ = persistence.record_snapshot(
                        state_to_save.balance,
                        unrealized_pnl,
                        total_equity,
                        realized_pnl,
                        state_to_save.positions.len(),
                        max_drawdown,
                    );
                }
                last_state_save = now;
            }
        }

        // Sleep before next iteration
        let loop_duration = (Utc::now() - loop_start).num_milliseconds();
        debug!("⏱️  Loop completed in {}ms", loop_duration);

        tokio::time::sleep(Duration::from_secs(60)).await; // 1 minute between scans
    }

    // Save final state before shutdown
    if trading_mode == TradingMode::Mock {
        info!("💾 [PERSISTENCE] Saving final state before shutdown...");
        let state_to_save = mock_client.export_state().await;
        if let Err(e) = persistence.save_state(&state_to_save) {
            error!("❌ [PERSISTENCE] Failed to save final state: {}", e);
        } else {
            info!("✅ [PERSISTENCE] Final state saved successfully");
        }
    }

    // Final status log
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    info!("🏁 Final Statistics:");
    if trading_mode == TradingMode::Mock {
        let state = mock_client.get_state().await;
        let (realized_pnl, unrealized_pnl) = mock_client.calculate_pnl().await;
        log_status_with_risk(
            &metrics,
            &state,
            realized_pnl,
            unrealized_pnl,
            &risk_orchestrator,
        );
    }

    info!("👋 Funding Fee Farmer shutdown complete");
    Ok(())
}

/// Initialize comprehensive logging with file output.
fn init_logging() -> Result<()> {
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    // Create logs directory
    std::fs::create_dir_all("logs")?;

    // File appender for detailed logs
    let file_appender = tracing_appender::rolling::hourly("logs", "funding-farmer.log");
    let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);

    // Leak the guard to keep it alive for the program duration
    Box::leak(Box::new(_guard));

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("funding_fee_farmer=debug".parse()?)
                .add_directive(Level::INFO.into()),
        )
        .with_writer(std::io::stdout.and(file_writer))
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_ansi(true)
        .init();

    Ok(())
}

/// Log configuration on startup.
fn log_config(config: &Config) {
    info!("📋 Configuration:");
    info!(
        "   Capital Utilization: {:.0}%",
        config.capital.max_utilization * dec!(100)
    );
    info!(
        "   Reserve Buffer: {:.0}%",
        config.capital.reserve_buffer * dec!(100)
    );
    info!(
        "   Min Position Size: ${}",
        config.capital.min_position_size
    );
    info!(
        "   Max Drawdown: {:.0}%",
        config.risk.max_drawdown * dec!(100)
    );
    info!("   Min Margin Ratio: {}x", config.risk.min_margin_ratio);
    info!(
        "   Default Leverage: {}x",
        config.execution.default_leverage
    );
    info!(
        "   Min Funding Rate: {:.4}%",
        config.pair_selection.min_funding_rate * dec!(100)
    );
    info!(
        "   Min Volume 24h: ${:.0}M",
        config.pair_selection.min_volume_24h / dec!(1_000_000)
    );
}

/// Fetch real positions.
async fn fetch_real_positions(client: &BinanceClient) -> Result<HashMap<String, Decimal>> {
    match client.get_positions().await {
        Ok(positions) => Ok(positions
            .into_iter()
            .filter(|p| p.position_amt != Decimal::ZERO)
            .map(|p| (p.symbol, p.position_amt))
            .collect()),
        Err(e) => {
            error!("Failed to fetch real positions: {}", e);
            Err(e.into())
        }
    }
}

/// Fetch current prices from real client.
async fn fetch_prices(
    client: &BinanceClient,
    pairs: &[funding_fee_farmer::exchange::QualifiedPair],
) -> HashMap<String, Decimal> {
    match client.get_book_tickers().await {
        Ok(tickers) => tickers
            .into_iter()
            .filter(|t| pairs.iter().any(|p| p.symbol == t.symbol))
            .map(|t| {
                let mid_price = (t.bid_price + t.ask_price) / dec!(2);
                (t.symbol, mid_price)
            })
            .collect(),
        Err(e) => {
            warn!("Failed to fetch prices: {}", e);
            HashMap::new()
        }
    }
}

/// Log comprehensive status with risk orchestrator metrics.
fn log_status_with_risk(
    metrics: &AppMetrics,
    state: &funding_fee_farmer::exchange::mock::MockTradingState,
    realized_pnl: Decimal,
    unrealized_pnl: Decimal,
    risk_orchestrator: &RiskOrchestrator,
) {
    let runtime = Utc::now() - metrics.start_time;
    let hours = runtime.num_hours();
    let minutes = runtime.num_minutes() % 60;

    let drawdown_stats = risk_orchestrator.get_drawdown_stats();
    let active_alerts = risk_orchestrator.get_active_alerts();
    let tracked_positions = risk_orchestrator.get_all_tracked_positions();

    info!("╔════════════════════════════════════════════════════════════╗");
    info!("║                    STATUS REPORT                           ║");
    info!("╠════════════════════════════════════════════════════════════╣");
    info!(
        "║ Runtime: {}h {}m                                           ",
        hours, minutes
    );
    info!("╠════════════════════════════════════════════════════════════╣");
    info!("║ 💰 ACCOUNT                                                 ║");
    info!(
        "║    Initial Balance:     ${:>12.2}                     ",
        state.initial_balance
    );
    info!(
        "║    Current Balance:     ${:>12.2}                     ",
        state.balance
    );
    info!(
        "║    Unrealized PnL:      ${:>12.2}                     ",
        unrealized_pnl
    );
    info!(
        "║    Total Equity:        ${:>12.2}                     ",
        state.balance + unrealized_pnl
    );
    info!("╠════════════════════════════════════════════════════════════╣");
    info!("║ 📊 P&L BREAKDOWN                                          ║");
    info!(
        "║    Funding Received:    ${:>12.4}                     ",
        state.total_funding_received
    );
    info!(
        "║    Trading Fees:       -${:>12.4}                     ",
        state.total_trading_fees
    );
    info!(
        "║    Borrow Interest:    -${:>12.4}                     ",
        state.total_borrow_interest
    );
    info!(
        "║    Realized PnL:        ${:>12.4}                     ",
        realized_pnl
    );
    info!("╠════════════════════════════════════════════════════════════╣");
    info!("║ 📈 ACTIVITY                                                ║");
    info!(
        "║    Scans:              {:>6}                              ",
        metrics.scan_count
    );
    info!(
        "║    Opportunities:      {:>6}                              ",
        metrics.opportunities_found
    );
    info!(
        "║    Positions Entered:  {:>6}                              ",
        metrics.positions_entered
    );
    info!(
        "║    Rebalances:         {:>6}                              ",
        metrics.rebalances_triggered
    );
    info!(
        "║    Funding Collections:{:>6}                              ",
        metrics.funding_collections
    );
    info!(
        "║    Orders Placed:      {:>6}                              ",
        state.order_count
    );
    info!(
        "║    Errors:             {:>6}                              ",
        metrics.errors_count
    );
    info!("╠════════════════════════════════════════════════════════════╣");
    info!("║ ⚠️  RISK                                                   ║");
    info!(
        "║    Current Drawdown:   {:>6.2}%                            ",
        drawdown_stats.current_drawdown * dec!(100)
    );
    info!(
        "║    Session MDD:        {:>6.2}%                            ",
        drawdown_stats.session_mdd * dec!(100)
    );
    info!(
        "║    Peak Equity:        ${:>12.2}                     ",
        drawdown_stats.peak_equity
    );
    info!(
        "║    Active Positions:   {:>6}                              ",
        state.positions.len()
    );
    info!(
        "║    Tracked Positions:  {:>6}                              ",
        tracked_positions.len()
    );
    info!(
        "║    Active Alerts:      {:>6}                              ",
        active_alerts.len()
    );
    info!("╚════════════════════════════════════════════════════════════╝");

    // Log per-position health if any positions tracked
    if !tracked_positions.is_empty() {
        info!("╔════════════════════════════════════════════════════════════╗");
        info!("║                 POSITION HEALTH                            ║");
        info!("╠════════════════════════════════════════════════════════════╣");
        for pos in &tracked_positions {
            let net_pnl = pos.net_pnl();
            let status = if net_pnl >= Decimal::ZERO { "✅" } else { "⚠️" };
            info!(
                "║ {} {:12} | Fund: ${:>8.4} | Net: ${:>8.4}          ",
                status,
                pos.symbol,
                pos.total_funding_received,
                net_pnl
            );
        }
        info!("╚════════════════════════════════════════════════════════════╝");
    }
}

/// Show current mock farmer status from persisted state.
fn show_status(db_path: &str, verbose: bool) -> Result<()> {
    use std::path::Path;

    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║              MOCK FARMER STATUS                            ║");
    println!("╚════════════════════════════════════════════════════════════╝");

    if !Path::new(db_path).exists() {
        println!("\n❌ Database not found: {}", db_path);
        println!("   The mock farmer has not been started yet, or the database path is incorrect.");
        return Ok(());
    }

    let persistence = PersistenceManager::new(db_path)?;

    let Some(state) = persistence.load_state()? else {
        println!("\n❌ No saved state found in database.");
        println!("   The mock farmer may not have run yet.");
        return Ok(());
    };

    // Calculate stats
    let pnl = state.balance - state.initial_balance;
    let pnl_pct = if state.initial_balance > Decimal::ZERO {
        (pnl / state.initial_balance) * dec!(100)
    } else {
        Decimal::ZERO
    };
    let net_yield = state.total_funding_received - state.total_trading_fees - state.total_borrow_interest;

    println!("\n📊 Account Summary");
    println!("   ├─ Initial Balance:  ${:.2}", state.initial_balance);
    println!("   ├─ Current Balance:  ${:.2}", state.balance);
    println!("   ├─ PnL:              ${:.2} ({:+.2}%)", pnl, pnl_pct);
    println!("   └─ Last Updated:     {}", state.last_saved.format("%Y-%m-%d %H:%M:%S UTC"));

    println!("\n💰 Funding & Costs");
    println!("   ├─ Total Funding:    ${:.4}", state.total_funding_received);
    println!("   ├─ Trading Fees:     ${:.4}", state.total_trading_fees);
    println!("   ├─ Borrow Interest:  ${:.4}", state.total_borrow_interest);
    println!("   └─ Net Yield:        ${:.4}", net_yield);

    println!("\n📈 Activity");
    println!("   ├─ Total Orders:     {}", state.order_count);
    println!("   └─ Open Positions:   {}", state.positions.len());

    if !state.positions.is_empty() {
        println!("\n🔓 Open Positions");
        for (symbol, pos) in &state.positions {
            let pos_pnl = pos.total_funding_received - pos.total_interest_paid;
            println!("   ┌─ {}", symbol);
            println!("   ├─ Futures: {} @ ${:.2}", pos.futures_qty, pos.futures_entry_price);
            println!("   ├─ Spot:    {} @ ${:.2}", pos.spot_qty, pos.spot_entry_price);
            if pos.borrowed_amount > Decimal::ZERO {
                println!("   ├─ Borrowed: ${:.2}", pos.borrowed_amount);
            }
            println!("   ├─ Funding Collected: ${:.4} ({} times)", pos.total_funding_received, pos.funding_collections);
            if pos.total_interest_paid > Decimal::ZERO {
                println!("   ├─ Interest Paid:    ${:.4}", pos.total_interest_paid);
            }
            println!("   ├─ Net P/L:          ${:.4}", pos_pnl);
            println!("   └─ Opened:           {}", pos.opened_at.format("%Y-%m-%d %H:%M:%S UTC"));

            if verbose {
                let duration = Utc::now() - pos.opened_at;
                let hours = duration.num_hours();
                let funding_periods = hours / 8;
                println!("       Duration: {}h ({} funding periods)", hours, funding_periods);
            }
        }
    }

    // Get funding stats per symbol
    if verbose {
        if let Ok(funding_stats) = persistence.get_funding_stats() {
            if !funding_stats.is_empty() {
                println!("\n📊 Funding by Symbol");
                for (symbol, total) in &funding_stats {
                    println!("   ├─ {}: ${:.4}", symbol, total);
                }
            }
        }

        if let Ok(snapshots) = persistence.get_recent_snapshots(5) {
            if !snapshots.is_empty() {
                println!("\n📉 Recent Equity Snapshots");
                for (ts, equity) in &snapshots {
                    println!("   ├─ {}: ${:.2}", ts.format("%Y-%m-%d %H:%M"), equity);
                }
            }
        }
    }

    println!();
    Ok(())
}

/// Run a single backtest with the given parameters.
async fn run_backtest(
    data_path: &str,
    start_str: &str,
    end_str: &str,
    initial_balance: f64,
    output_dir: Option<&str>,
) -> Result<()> {
    info!("╔════════════════════════════════════════════════════════════╗");
    info!("║              BACKTEST MODE                                 ║");
    info!("╚════════════════════════════════════════════════════════════╝");

    // Parse dates
    let start_date = NaiveDate::parse_from_str(start_str, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("Invalid start date '{}': {}", start_str, e))?;
    let end_date = NaiveDate::parse_from_str(end_str, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("Invalid end date '{}': {}", end_str, e))?;

    let start = start_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let end = end_date.and_hms_opt(23, 59, 59).unwrap().and_utc();

    info!("📊 Loading data from: {}", data_path);
    let data_loader = CsvDataLoader::new(data_path)?;

    if let Some((data_start, data_end)) = data_loader.available_range() {
        info!(
            "   Data range: {} to {}",
            data_start.format("%Y-%m-%d"),
            data_end.format("%Y-%m-%d")
        );
    }

    info!("   Symbols: {}", data_loader.available_symbols().len());
    info!("   Snapshots: {}", data_loader.len());

    // Load trading config
    let config = Config::load()?;

    // Create backtest config
    let backtest_config = BacktestConfig {
        initial_balance: Decimal::from_f64_retain(initial_balance).unwrap_or(dec!(10000)),
        time_step_minutes: 60,
        record_equity_curve: true,
        record_trades: true,
        output_path: output_dir.map(String::from),
    };

    info!("💰 Initial balance: ${:.2}", initial_balance);
    info!("📅 Period: {} to {}", start_str, end_str);

    // Run backtest
    let mut engine = BacktestEngine::new(data_loader, config, backtest_config);
    let result = engine.run(start, end).await?;

    // Print results
    println!("\n{}", result.summary());

    // Save results if output directory specified
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;

        let equity_path = format!("{}/equity_curve.csv", dir);
        result.equity_to_csv(&equity_path)?;
        info!("📁 Equity curve saved to: {}", equity_path);
    }

    Ok(())
}

/// Run a parameter sweep optimization.
async fn run_sweep(
    data_path: &str,
    start_str: &str,
    end_str: &str,
    initial_balance: f64,
    parallelism: usize,
    output_dir: Option<&str>,
    minimal: bool,
) -> Result<()> {
    info!("╔════════════════════════════════════════════════════════════╗");
    info!("║           PARAMETER SWEEP MODE                             ║");
    info!("╚════════════════════════════════════════════════════════════╝");

    // Parse dates
    let start_date = NaiveDate::parse_from_str(start_str, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("Invalid start date '{}': {}", start_str, e))?;
    let end_date = NaiveDate::parse_from_str(end_str, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("Invalid end date '{}': {}", end_str, e))?;

    let start = start_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let end = end_date.and_hms_opt(23, 59, 59).unwrap().and_utc();

    info!("📊 Loading data from: {}", data_path);
    let data_loader = CsvDataLoader::new(data_path)?;

    if let Some((data_start, data_end)) = data_loader.available_range() {
        info!(
            "   Data range: {} to {}",
            data_start.format("%Y-%m-%d"),
            data_end.format("%Y-%m-%d")
        );
    }

    // Load base config
    let base_config = Config::load()?;

    // Create parameter space
    let param_space = if minimal {
        info!("🔧 Using minimal parameter space (quick test)");
        ParameterSpace::minimal()
    } else {
        info!("🔧 Using full parameter space");
        ParameterSpace::default()
    };

    info!("   Combinations to test: {}", param_space.combination_count());

    // Create backtest config
    let backtest_config = BacktestConfig {
        initial_balance: Decimal::from_f64_retain(initial_balance).unwrap_or(dec!(10000)),
        time_step_minutes: 60,
        record_equity_curve: false, // Save memory during sweeps
        record_trades: false,
        output_path: None,
    };

    info!("💰 Initial balance: ${:.2}", initial_balance);
    info!("📅 Period: {} to {}", start_str, end_str);
    info!("⚡ Parallelism: {}", parallelism);

    // Create and run sweep
    let runner = SweepRunner::new(param_space, base_config, backtest_config, parallelism);
    let results = runner.run(data_loader, start, end).await?;

    // Print summary
    println!("\n{}", results.summary());

    // Save results if output directory specified
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;

        let results_path = format!("{}/sweep_results.csv", dir);
        results.to_csv(&results_path)?;
        info!("📁 Sweep results saved to: {}", results_path);
    }

    Ok(())
}
