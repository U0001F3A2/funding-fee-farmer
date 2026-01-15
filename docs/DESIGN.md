# Funding Fee Farmer - Design Document

## Overview

A high-performance Rust application for delta-neutral funding fee farming on Binance Futures. The system captures funding rate payments while maintaining market-neutral exposure, optimizing for maximum capital utilization and minimal drawdown.

## Strategy

### Core Mechanism

Funding rates are periodic payments between long and short positions in perpetual futures. When funding is positive, longs pay shorts; when negative, shorts pay longs.

**Delta-Neutral Position:**
```
Positive Funding: Long Spot + Short Perpetual → Receive funding every 8h
Negative Funding: Short Spot (Margin) + Long Perpetual → Receive funding every 8h
```

### Why Binance?

- Highest liquidity across major pairs
- 8-hour funding intervals (00:00, 08:00, 16:00 UTC)
- Competitive trading fees (0.02% maker / 0.04% taker with BNB)
- Cross-margin efficiency
- Robust API with WebSocket support

## Capital Utilization Optimization

### Target: >80% Capital Utilization Rate

**Definition:** `Capital Utilization = Active Position Value / Total Available Capital`

### Strategies

#### 1. Cross-Margin Mode
- Use cross-margin instead of isolated margin
- Allows shared collateral across positions
- Reduces idle margin requirements by ~40%

#### 2. Multi-Position Allocation
```
Total Capital: $100,000
├── Position 1 (BTCUSDT): 30% allocation
├── Position 2 (ETHUSDT): 25% allocation
├── Position 3 (SOLUSDT): 20% allocation
├── Position 4 (BNBUSDT): 15% allocation
└── Reserve Buffer: 10% (liquidation protection)
```

#### 3. Dynamic Leverage Selection
- Base leverage: 3-5x (conservative)
- Maximum leverage: 10x (high-conviction scenarios)
- Auto-deleverage on funding rate reversal signals

#### 4. Position Entry Optimization
- Split large orders to minimize market impact
- Use TWAP/VWAP for entries >$50k
- Target <0.05% slippage per entry

## Maximum Drawdown (MDD) Management

### Target: MDD < 5%

### Risk Factors & Mitigations

| Risk Factor | Impact | Mitigation |
|-------------|--------|------------|
| Price divergence (spot vs perp) | Medium | Monitor basis, exit at >0.5% divergence |
| Funding rate reversal | High | Predictive model + quick exit capability |
| Liquidation | Critical | Maintain 300% margin ratio minimum |
| Exchange risk | Critical | Position limits per exchange |
| Slippage on exit | Medium | Volume filters + staged exits |

### Liquidation Prevention

```
Margin Ratio Thresholds:
├── Green (>500%): Normal operation
├── Yellow (300-500%): Reduce position size by 25%
├── Orange (200-300%): Emergency deleveraging
└── Red (<200%): Full position closure
```

### Position Sizing Formula

```
Max Position Size = (Account Equity × Target Utilization × Leverage) / (1 + Safety Buffer)

Where:
- Target Utilization = 0.85
- Leverage = 5x (default)
- Safety Buffer = 0.15 (for adverse price movement)
```

## Pair Selection Criteria

### Mandatory Filters

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| 24h Trading Volume | >$100M | Ensures liquidity for entry/exit |
| Funding Rate | >0.01% or <-0.01% | Minimum profitability threshold |
| Spread (bid-ask) | <0.02% | Minimizes entry/exit costs |
| Open Interest | >$50M | Market depth indicator |

### Scoring Model

```
Score = (Funding_Rate × 0.4) + (Volume_Score × 0.3) + (Stability_Score × 0.2) + (Spread_Score × 0.1)

Where:
- Funding_Rate: Absolute funding rate (higher = better)
- Volume_Score: log(volume) normalized
- Stability_Score: 1 / funding_rate_volatility
- Spread_Score: 1 / spread_percentage
```

### Typical High-Yield Pairs

- BTCUSDT, ETHUSDT (always liquid)
- SOLUSDT, BNBUSDT (high activity)
- Trending altcoins during high volatility periods

## System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Funding Fee Farmer                        │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │   Market    │  │  Position   │  │     Risk Manager        │  │
│  │   Scanner   │  │   Manager   │  │  - Margin Monitor       │  │
│  │  - Funding  │  │  - Entry    │  │  - Liquidation Guard    │  │
│  │  - Volume   │  │  - Exit     │  │  - MDD Tracker          │  │
│  │  - Spread   │  │  - Hedge    │  │  - Auto-Deleverage      │  │
│  └──────┬──────┘  └──────┬──────┘  └───────────┬─────────────┘  │
│         │                │                      │                │
│         └────────────────┼──────────────────────┘                │
│                          │                                       │
│  ┌───────────────────────┴───────────────────────────────────┐  │
│  │                    Strategy Engine                         │  │
│  │  - Capital Allocator                                       │  │
│  │  - Opportunity Ranker                                      │  │
│  │  - Execution Scheduler                                     │  │
│  └───────────────────────┬───────────────────────────────────┘  │
│                          │                                       │
│  ┌───────────────────────┴───────────────────────────────────┐  │
│  │                 Binance Connector                          │  │
│  │  - REST API (orders, account)                              │  │
│  │  - WebSocket (market data, user stream)                    │  │
│  │  - Rate Limiter                                            │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Execution Flow

### 1. Opportunity Discovery (Every 1 minute)
```rust
loop {
    let pairs = scanner.get_qualifying_pairs().await;
    let ranked = strategy.rank_opportunities(pairs);
    let allocation = strategy.calculate_allocation(ranked, account.equity);
    executor.apply_changes(allocation).await;
    sleep(Duration::from_secs(60)).await;
}
```

### 2. Position Entry
```
1. Verify funding rate direction and magnitude
2. Calculate optimal position size
3. Place spot order (limit, post-only)
4. Wait for fill
5. Place corresponding futures order
6. Verify delta-neutral state
7. Set monitoring alerts
```

### 3. Position Exit (Before Funding Reversal)
```
1. Detect funding rate trend reversal signal
2. Calculate exit priority (highest funding loss risk first)
3. Close futures position (limit preferred, market if urgent)
4. Close spot position
5. Reconcile P&L
```

## Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Capital Utilization | >80% | Active capital deployment |
| Maximum Drawdown | <5% | Per month |
| Sharpe Ratio | >2.0 | Risk-adjusted returns |
| Win Rate | >70% | Per funding period |
| Average Daily Return | 0.05-0.15% | Conservative estimate |
| Annual Return (target) | 20-50% | Depends on market conditions |

## Configuration Parameters

```toml
[capital]
max_utilization = 0.85
reserve_buffer = 0.10
min_position_size = 1000.0  # USDT

[risk]
max_drawdown = 0.05
min_margin_ratio = 3.0
max_single_position = 0.30  # 30% of capital

[pair_selection]
min_volume_24h = 100_000_000  # $100M
min_funding_rate = 0.0001     # 0.01%
max_spread = 0.0002           # 0.02%
min_open_interest = 50_000_000

[execution]
default_leverage = 5
max_leverage = 10
slippage_tolerance = 0.0005   # 0.05%
order_timeout_secs = 30
```

## API Rate Limits (Binance)

| Endpoint Type | Limit | Strategy |
|---------------|-------|----------|
| Order placement | 10/sec | Queue + batch |
| Account info | 20/min | Cache 3s |
| Market data (REST) | 1200/min | Use WebSocket |
| WebSocket streams | 5 messages/sec | Aggregate updates |

## Future Enhancements

1. **Multi-Exchange Support**: Expand to OKX, Bybit for arbitrage opportunities
2. **Funding Rate Prediction**: ML model for rate direction prediction
3. **Basis Trading**: Add spot-futures basis arbitrage
4. **Auto-Compounding**: Reinvest profits automatically
5. **Telegram Alerts**: Real-time position and P&L notifications

## Risk Disclaimer

This system involves significant financial risk. Funding rates can reverse quickly, and positions may incur losses from:
- Adverse funding rate changes
- Price divergence between spot and futures
- Exchange technical issues
- Liquidation during extreme volatility

Always start with small position sizes and thoroughly backtest strategies before deploying capital.
