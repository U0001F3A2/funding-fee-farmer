# Funding Fee Farmer - Design Document

## Overview

A high-performance Rust application for delta-neutral funding fee farming on Binance Futures. The system captures funding rate payments while maintaining market-neutral exposure, optimizing for maximum capital utilization and minimal drawdown.

## Strategy

### Core Mechanism

Funding rates are periodic payments between long and short positions in perpetual futures. When funding is positive, longs pay shorts; when negative, shorts pay longs.

**Delta-Neutral Position:**
```
Positive Funding: Long Spot + Short Perpetual → Receive funding every 8h
Negative Funding: Short Spot (Margin Borrow) + Long Perpetual → Receive funding every 8h
```

### Strategy Workflow

```
1. SCAN: Find pairs with highest |funding rate| in USDT perpetuals
2. FILTER: Verify spot margin trading is enabled (required for hedging)
3. QUALIFY: Check borrow rates and calculate net profitability
4. ENTER: Open delta-neutral position (futures + spot hedge simultaneously)
5. MONITOR: Track delta drift and funding rate changes
6. REBALANCE: Adjust hedge if delta drifts > 3% from neutral
7. EXIT/FLIP: Close or reverse position when funding direction changes
```

### Spot Margin Hedging

For delta-neutrality, we use Binance's cross-margin spot trading:

| Funding Direction | Futures Side | Spot Side | Margin Action |
|-------------------|--------------|-----------|---------------|
| Positive (>0) | Short | Long | Normal buy (no borrow) |
| Negative (<0) | Long | Short | Auto-borrow base asset |

**Borrow Cost Consideration:**
```
Net Profit = |Funding Rate| - Borrow Rate (if shorting spot)
```
Pairs are only qualified if: `|Funding Rate| > Borrow Rate × Safety Margin`

### Why Binance?

- Highest liquidity across major pairs
- 8-hour funding intervals (00:00, 08:00, 16:00 UTC)
- Competitive trading fees (0.02% maker / 0.04% taker with BNB)
- Cross-margin efficiency for both futures and spot
- Spot margin trading with reasonable borrow rates
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

## Hedge Rebalancing

### Why Rebalancing is Critical

Even with simultaneous entry, positions can drift from delta-neutral due to:
- Partial fills on entry
- Price movement during execution
- Position adjustments by exchange (auto-deleveraging)
- Manual intervention

### Rebalancing Configuration

```toml
[rebalance]
max_delta_drift = 0.03    # 3% drift triggers rebalance
min_rebalance_size = 100  # Minimum $100 trade (avoid dust)
auto_flip_on_reversal = true  # Flip position when funding reverses
```

### Rebalancing Actions

| Condition | Action | Priority |
|-----------|--------|----------|
| Delta drift > 3% | Adjust smaller leg | Normal |
| Funding rate reversal | Flip entire position | High |
| Delta drift > 10% | Emergency rebalance | Critical |
| Funding < borrow cost | Close position | High |

### Delta Drift Calculation

```
Delta % = |Net Exposure| / Max(|Futures Qty|, |Spot Qty|)

Example:
  Futures: -1.0 BTC (short)
  Spot: +1.05 BTC (long)
  Net: +0.05 BTC (net long)
  Delta %: 0.05 / 1.05 = 4.76% → Triggers rebalance
```

## Pair Selection Criteria

### Mandatory Filters

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| 24h Trading Volume | >$100M | Ensures liquidity for entry/exit |
| Funding Rate | >0.01% or <-0.01% | Minimum profitability threshold |
| Spread (bid-ask) | <0.02% | Minimizes entry/exit costs |
| Open Interest | >$50M | Market depth indicator |
| Spot Margin Enabled | Required | Must be able to hedge via margin |
| Borrow Rate | < Funding Rate | Net profit must be positive |

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
┌─────────────────────────────────────────────────────────────────────┐
│                         Funding Fee Farmer                           │
├─────────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌───────────────────────────┐    │
│  │   Market    │  │   Order     │  │       Risk Manager        │    │
│  │   Scanner   │  │  Executor   │  │  - Margin Monitor         │    │
│  │  - Funding  │  │  - Entry    │  │  - Liquidation Guard      │    │
│  │  - Volume   │  │  - Exit     │  │  - MDD Tracker            │    │
│  │  - Spread   │  │  - Retry    │  │  - Auto-Deleverage        │    │
│  │  - Margin✓  │  └──────┬──────┘  └─────────────┬─────────────┘    │
│  └──────┬──────┘         │                       │                  │
│         │                │                       │                  │
│  ┌──────┴────────────────┴───────────────────────┴───────────────┐  │
│  │                     Strategy Engine                            │  │
│  │  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐ │  │
│  │  │ Capital         │  │ Order           │  │ Hedge          │ │  │
│  │  │ Allocator       │  │ Executor        │  │ Rebalancer     │ │  │
│  │  │ - Sizing        │  │ - Spot Margin   │  │ - Delta Drift  │ │  │
│  │  │ - Ranking       │  │ - Futures       │  │ - Auto-Flip    │ │  │
│  │  │ - Utilization   │  │ - Unwind        │  │ - Thresholds   │ │  │
│  │  └─────────────────┘  └─────────────────┘  └────────────────┘ │  │
│  └───────────────────────────┬───────────────────────────────────┘  │
│                              │                                      │
│  ┌───────────────────────────┴───────────────────────────────────┐  │
│  │                    Binance Connector                           │  │
│  │  - REST API (orders, account, margin)                          │  │
│  │  - WebSocket (market data, user stream)                        │  │
│  │  - Rate Limiter                                                │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
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

### 2. Position Entry (Delta-Neutral)
```
1. Verify funding rate direction and magnitude
2. Check spot margin availability and borrow rate
3. Calculate optimal position size (USDT value / price)
4. Set futures account (cross margin, target leverage)
5. Execute futures order FIRST (market, critical for funding capture)
   - If fails: abort entry
6. Execute spot hedge immediately after
   - Positive funding: Buy spot (normal)
   - Negative funding: Sell spot (auto-borrow via margin)
   - If fails: UNWIND futures position immediately
7. Verify delta-neutral state (< 5% drift)
8. Log position and set monitoring
```

### 3. Position Exit (Before Funding Reversal)
```
1. Detect funding rate trend reversal signal
2. Calculate exit priority (highest funding loss risk first)
3. Close futures position (limit preferred, market if urgent)
4. Close spot position (repay borrow if shorting)
5. Reconcile P&L
```

### 4. Hedge Rebalancing Loop (Every 5 minutes)
```rust
loop {
    let positions = client.get_delta_neutral_positions().await;
    let funding_rates = client.get_funding_rates().await;
    let prices = client.get_prices().await;

    for position in positions {
        let action = rebalancer.analyze_position(&position, funding_rate, price);
        match action {
            RebalanceAction::None => continue,
            RebalanceAction::AdjustSpot { .. } => /* sell/buy spot */,
            RebalanceAction::AdjustFutures { .. } => /* sell/buy futures */,
            RebalanceAction::FlipPosition { .. } => /* close & reverse both legs */,
            RebalanceAction::ClosePosition { .. } => /* close both legs */,
        }
    }
    sleep(Duration::from_secs(300)).await;
}
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

[rebalance]
max_delta_drift = 0.03        # 3% drift triggers rebalance
min_rebalance_size = 100.0    # Minimum $100 trade
auto_flip_on_reversal = true  # Auto-flip when funding reverses
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

---

## CI/CD: Automated Log Analysis & Improvements

The project includes GitHub Actions workflows for automated monitoring and continuous improvement using Claude Code.

### Workflow Overview

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Log Analysis   │────>│  Create Issue    │────>│  Claude Code    │
│  (every 4 hrs)  │     │  (if problems)   │     │  Implementation │
└─────────────────┘     └──────────────────┘     └─────────────────┘
        │                        │                        │
        v                        v                        v
   SSH to EC2              Label: claude-           Create PR for
   Collect logs            improvement              human review
```

### Workflows

| Workflow | Trigger | Purpose |
|----------|---------|---------|
| `log-analysis.yml` | Schedule (every 4h) | Collects logs, analyzes with Claude, creates improvement issues |
| `claude-implement.yml` | Issue labeled `claude-improvement` | Implements suggested fixes, creates PRs |

### Required GitHub Secrets

Configure these in your repository: **Settings → Secrets and variables → Actions**

| Secret | Description | How to Get |
|--------|-------------|------------|
| `ANTHROPIC_API_KEY` | Claude API key for analysis | [console.anthropic.com](https://console.anthropic.com) |
| `EC2_HOST` | EC2 instance public IP | e.g., `43.203.183.140` |
| `EC2_SSH_KEY` | Private SSH key for EC2 | Contents of `~/.ssh/fff-key.pem` |

### Setup Instructions

1. **Add Anthropic API Key**
   ```bash
   # Go to GitHub repo → Settings → Secrets → Actions → New repository secret
   # Name: ANTHROPIC_API_KEY
   # Value: sk-ant-api03-...
   ```

2. **Add EC2 Host**
   ```bash
   # Name: EC2_HOST
   # Value: 43.203.183.140  (your EC2 public IP)
   ```

3. **Add SSH Key**
   ```bash
   # Name: EC2_SSH_KEY
   # Value: (paste entire contents of ~/.ssh/fff-key.pem including BEGIN/END lines)
   ```

### Manual Trigger

You can manually trigger log analysis:
```bash
gh workflow run log-analysis.yml
```

Or trigger Claude to implement an existing issue by commenting:
```
@claude please implement this
```

### Issue Labels

| Label | Meaning |
|-------|---------|
| `claude-improvement` | Triggers Claude Code to implement |
| `automated` | Created by automation |
| `high` / `medium` / `low` | Priority level |
| `claude-code` | PR created by Claude |

### How It Works

1. **Log Analysis** (scheduled every 4 hours):
   - SSHs to EC2 instance
   - Collects last 4 hours of systemd journal logs
   - Sends to Claude for analysis
   - Creates GitHub issues for HIGH priority improvements

2. **Claude Implementation** (triggered by label):
   - Reads issue description
   - Checks out code on new branch
   - Uses Claude Code to implement fix
   - Runs `cargo build` and `cargo test`
   - Creates PR if changes are made

3. **Human Review**:
   - Review the PR created by Claude
   - Check the automated test results
   - Merge or request changes
