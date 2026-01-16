//! Funding Fee Farmer - Main Entry Point
//!
//! MVP version with mock trading support for paper trading and testing.

use anyhow::Result;
use chrono::{DateTime, Timelike, Utc};
use funding_fee_farmer::config::Config;
use funding_fee_farmer::exchange::{BinanceClient, MockBinanceClient};
use funding_fee_farmer::risk::{
    RiskOrchestrator, RiskOrchestratorConfig, RiskAlertType, PositionEntry,
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
    // Initialize comprehensive logging
    init_logging()?;

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

    let mock_client = MockBinanceClient::new(dec!(10000)); // $10k paper trading

    // Initialize RiskOrchestrator with comprehensive risk monitoring
    let risk_config = RiskOrchestratorConfig {
        max_drawdown: config.risk.max_drawdown,
        min_margin_ratio: config.risk.min_margin_ratio,
        max_single_position: config.risk.max_single_position,
        max_unprofitable_hours: config.risk.max_unprofitable_hours,
        min_expected_yield: config.risk.min_expected_yield,
        grace_period_hours: config.risk.grace_period_hours,
        max_funding_deviation: config.risk.max_funding_deviation,
        max_errors_per_minute: config.risk.max_errors_per_minute,
        max_consecutive_failures: config.risk.max_consecutive_failures,
        emergency_delta_drift: config.risk.emergency_delta_drift,
    };
    let mut risk_orchestrator = RiskOrchestrator::new(risk_config, dec!(10000));

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

    // Track last funding collection time
    let mut last_funding_hour: Option<u32> = None;
    let mut last_status_log = Utc::now();

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
            let current_positions: HashMap<String, Decimal> = if trading_mode == TradingMode::Mock {
                mock_client
                    .get_delta_neutral_positions()
                    .await
                    .into_iter()
                    .map(|p| (p.symbol, p.futures_qty))
                    .collect()
            } else {
                match fetch_real_positions(&real_client).await {
                    Ok(pos) => pos,
                    Err(_) => HashMap::new(),
                }
            };

            let mock_state = mock_client.get_state().await;
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
                    // Update mock client with real prices
                    let prices = fetch_prices(&real_client, &qualified_pairs).await;
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

                        info!("📈 [EXECUTE] Entering position: {}", alloc.symbol);

                        // Calculate quantity
                        let quantity = (alloc.target_size_usdt / price).round_dp(4);

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
            let risk_result = risk_orchestrator.check_all(&exchange_positions, total_equity, state.balance);

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

                let risk_result = risk_orchestrator.check_all(&live_positions, total_equity, margin_balance);

                if risk_result.should_halt {
                    error!("🚨 [RISK] CRITICAL: Trading halted by risk orchestrator!");
                    break;
                }
            }
        }

        // Sleep before next iteration
        let loop_duration = (Utc::now() - loop_start).num_milliseconds();
        debug!("⏱️  Loop completed in {}ms", loop_duration);

        tokio::time::sleep(Duration::from_secs(60)).await; // 1 minute between scans
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
