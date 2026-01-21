# Funding Fee Farming Profitability Analysis

Historical analysis of single-venue delta-neutral funding fee farming on Binance.

## Executive Summary

**Long-term expected return: ~14% net APY** (based on 6.4 years of data)

This strategy is profitable but "marginal" - comparable to passive index investing but with active management overhead and execution risk. Recent high returns (35-111% APY) observed in bull markets are anomalies, not the norm.

## Methodology

### Strategy

- **Approach**: Delta-neutral (long spot + short perp for positive funding, or vice versa)
- **Venue**: Binance only (single-venue)
- **Capital allocation**: Max 3 positions at 30% each (90% max deployment)
- **Entry threshold**: 0.10% funding rate per 8h period
- **Volume filter**: $100M minimum 24h volume

### Cost Model

| Cost Component | Rate |
|----------------|------|
| Futures taker fee | 0.04% |
| Spot taker fee | 0.10% |
| Slippage estimate | 0.01% |
| **Entry cost (both legs)** | ~0.30% |
| **Exit cost (both legs)** | ~0.30% |
| **Round-trip total** | ~0.60% |

## Historical Results (6.4 Years)

Analysis period: September 2019 - January 2026 (2,324 days)

### Overall Performance

| Metric | Value |
|--------|-------|
| Gross Funding Collected | $187,352 (187.4% of capital) |
| Trading Costs | $96,120 |
| Net Profit | $91,232 (91.2% of capital) |
| Positions Opened | 534 |
| Funding Events Captured | 4,192 |
| **Gross APY** | 29.4% |
| **Net APY** | 14.3% |
| **Cost Ratio** | 51.3% of funding |

### Funding Rate Distribution

| Threshold | Events | % of Total |
|-----------|--------|------------|
| >= 0.1% per 8h | 3,162 | 3.3% |
| >= 0.2% per 8h | 960 | 1.0% |
| >= 0.5% per 8h | 172 | 0.2% |

Only ~3% of funding events exceed the entry threshold - opportunities are infrequent.

## Threshold Sensitivity Analysis

Testing different minimum funding rate thresholds:

| Threshold | Positions | Gross APY | Net APY | Cost Ratio |
|-----------|-----------|-----------|---------|------------|
| 0.05% | 1,064 | 34.9% | 4.8% | 86.2% |
| **0.10%** | 486 | 27.4% | **13.7%** | 50.1% |
| 0.20% | 211 | 18.4% | 12.4% | 32.5% |
| 0.30% | 123 | 13.4% | 9.9% | 26.0% |
| 0.50% | 52 | 9.1% | 7.6% | 16.1% |

**Findings:**
- 0.10% threshold is near-optimal for maximizing net APY
- Lower thresholds capture more gross funding but costs dominate
- Higher thresholds reduce costs but miss too many opportunities

## Time Period Comparison

Returns vary significantly based on market conditions:

| Period | Net APY | Market Conditions |
|--------|---------|-------------------|
| Last 90 days | ~111% | Bull market, extreme funding |
| Last 365 days | ~35% | Mixed, above average |
| **Full history (6.4 years)** | **~14%** | Long-term average |

Recent high returns are anomalies driven by bull market conditions with elevated funding rates.

## Cross-Venue Arbitrage (Ruled Out)

Investigation of Hyperliquid + Binance cross-venue strategies:

### Perp-Perp Arbitrage
- **Finding**: NOT profitable
- **Reason**: Funding rates across venues are highly correlated (0.85+ correlation)
- **Spread**: Typically < 0.05% - insufficient to cover costs

### HL Perp + Binance Spot Margin
- **Finding**: Marginal (~1% APY improvement)
- **Reason**: Additional complexity not justified by returns

**Conclusion**: Cross-venue support removed from codebase.

## Key Insights

1. **Half of funding goes to trading costs** - This is the fundamental limitation of the strategy

2. **Entry/exit timing matters** - JIT (Just-In-Time) entry within 30 minutes of funding reduces capital exposure

3. **Opportunity frequency is low** - Only 3% of funding events meet criteria

4. **Market regime dependent** - Returns can vary 3-10x between bear and bull markets

5. **Compounding effect** - 14% APY compounded over years is still meaningful

## Risk Considerations

| Risk | Mitigation |
|------|------------|
| Execution slippage | High-volume pairs only ($100M+ daily) |
| Funding direction flip | Monitor and close positions before reversal |
| Exchange risk | Single venue (Binance) concentration |
| Liquidation | Maintain >300% margin ratio |
| API failures | Automatic retry with exponential backoff |

## Verdict

| Assessment | Rating |
|------------|--------|
| Profitability | Marginal (14% APY) |
| Complexity | High |
| Risk-adjusted return | Moderate |
| Recommendation | Viable for automated systems with low operational overhead |

The strategy is profitable but may not justify the operational complexity compared to simpler alternatives (staking, lending, passive holding). Best suited for fully automated deployment where marginal costs approach zero.

## Analysis Scripts

```bash
# Run full historical analysis
python scripts/analyze_funding_profitability.py --days 2324 --symbols 20

# Run shorter period analysis
python scripts/analyze_funding_profitability.py --days 365

# Customize capital
python scripts/analyze_funding_profitability.py --days 365 --capital 50000
```

## Data Sources

- Binance Futures API: `/fapi/v1/fundingRate` (historical funding rates)
- Binance Futures API: `/fapi/v1/ticker/24hr` (volume data)
- Data availability: September 2019 - present (~6.4 years)
