#!/usr/bin/env python3
"""
Analyze Hyperliquid perp + Binance spot margin delta-neutral strategy.

Strategy:
- Positive HL funding: Short HL perp + Long Binance spot (collect funding)
- Negative HL funding: Long HL perp + Short Binance spot margin (collect funding)

Costs:
- Binance spot margin borrow: ~10-15% APY (USDT) or ~5-10% APY (crypto)
- Binance spot trading: 0.1% (can be lower)
- HL perp trading: 0.02% maker / 0.05% taker
- HL funding: hourly settlement

Usage:
    python scripts/analyze_hl_spot_hedge.py --days 30
    python scripts/analyze_hl_spot_hedge.py --days 90 --coins BTC ETH SOL --simulate
"""

import argparse
import time
from datetime import datetime, timedelta, timezone
from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple
from collections import defaultdict
import requests

HYPERLIQUID_URL = "https://api.hyperliquid.xyz"
BINANCE_URL = "https://api.binance.com"

REQUEST_DELAY = 0.2

# Coins with good liquidity on both venues
LIQUID_COINS = [
    "BTC", "ETH", "SOL", "XRP", "DOGE", "ADA", "AVAX", "LINK",
    "DOT", "LTC", "ATOM", "ARB", "OP", "APT", "SUI", "INJ",
    "TIA", "SEI", "NEAR", "FTM", "AAVE", "UNI"
]

# Approximate Binance margin borrow rates (APY) - varies by asset
MARGIN_BORROW_RATES = {
    "USDT": 0.10,   # ~10% APY for USDT
    "BTC": 0.05,    # ~5% APY
    "ETH": 0.05,
    "DEFAULT": 0.08  # ~8% APY for most alts
}


@dataclass
class HLFundingRecord:
    timestamp: datetime
    coin: str
    funding_rate: float  # Hourly rate


@dataclass
class StrategyOpportunity:
    """Single funding period opportunity."""
    timestamp: datetime
    coin: str
    hl_funding_hourly: float
    hl_funding_8h: float  # Normalized for comparison
    direction: str  # "short_hl" or "long_hl"
    gross_yield_8h: float
    borrow_cost_8h: float
    net_yield_8h: float


def fetch_hl_funding_history(coin: str, start_ms: int, end_ms: Optional[int] = None) -> List[dict]:
    """Fetch HL funding history."""
    url = f"{HYPERLIQUID_URL}/info"
    payload = {
        "type": "fundingHistory",
        "coin": coin,
        "startTime": start_ms
    }
    if end_ms:
        payload["endTime"] = end_ms

    try:
        response = requests.post(url, json=payload, timeout=30)
        response.raise_for_status()
        return response.json()
    except Exception as e:
        print(f"  Warning: Failed to fetch HL funding for {coin}: {e}")
        return []


def fetch_binance_margin_rates() -> Dict[str, float]:
    """Fetch current Binance margin borrow rates."""
    # Note: Binance doesn't have a public historical margin rate API
    # Using approximated rates
    return MARGIN_BORROW_RATES


def get_borrow_cost_8h(coin: str, direction: str, rates: Dict[str, float]) -> float:
    """
    Calculate borrow cost for 8h period.

    - short_hl (long spot): No borrow needed (buy spot with cash)
    - long_hl (short spot): Need to borrow the crypto to sell
    """
    if direction == "short_hl":
        # Long spot - no borrowing, just holding
        return 0.0
    else:
        # Short spot margin - borrow the crypto
        annual_rate = rates.get(coin, rates["DEFAULT"])
        # Convert annual to 8h: rate * (8/24/365)
        return annual_rate * (8 / 24 / 365)


def collect_hl_funding(coins: List[str], start_time: datetime, end_time: datetime) -> Dict[str, List[HLFundingRecord]]:
    """Collect HL funding data."""
    start_ms = int(start_time.timestamp() * 1000)
    end_ms = int(end_time.timestamp() * 1000)

    data = {}
    print(f"\nFetching Hyperliquid funding for {len(coins)} coins...")

    for i, coin in enumerate(coins):
        print(f"  [{i+1}/{len(coins)}] {coin}...", end=" ")

        records = fetch_hl_funding_history(coin, start_ms, end_ms)
        if records:
            data[coin] = [
                HLFundingRecord(
                    timestamp=datetime.fromtimestamp(r['time'] / 1000, tz=timezone.utc),
                    coin=coin,
                    funding_rate=float(r['fundingRate'])
                )
                for r in records
            ]
            print(f"{len(records)} records")
        else:
            print("no data")

        time.sleep(REQUEST_DELAY)

    return data


def analyze_strategy(
    hl_data: Dict[str, List[HLFundingRecord]],
    min_funding_8h: float = 0.0005,  # 0.05% minimum to trade
    entry_cost: float = 0.001,  # 0.1% spot + HL combined
    exit_cost: float = 0.001,
) -> Tuple[List[StrategyOpportunity], Dict]:
    """
    Analyze delta-neutral strategy: HL perp + Binance spot.

    For each 8h period, aggregate HL hourly funding and calculate net yield.
    """
    margin_rates = fetch_binance_margin_rates()
    opportunities = []

    stats = {
        'total_periods': 0,
        'positive_funding_periods': 0,
        'negative_funding_periods': 0,
        'tradeable_periods': 0,
        'profitable_periods': 0,
        'total_gross_yield': 0.0,
        'total_borrow_cost': 0.0,
        'total_net_yield': 0.0,
        'by_coin': defaultdict(lambda: {
            'periods': 0, 'gross': 0.0, 'borrow': 0.0, 'net': 0.0,
            'positive_count': 0, 'negative_count': 0
        }),
        'by_month': defaultdict(lambda: {
            'periods': 0, 'gross': 0.0, 'net': 0.0
        })
    }

    for coin, records in hl_data.items():
        # Group into 8h periods (aligned to 00:00, 08:00, 16:00 UTC)
        periods = defaultdict(list)
        for rec in records:
            # Find the 8h period this belongs to
            hour = rec.timestamp.hour
            period_hour = (hour // 8) * 8
            period_time = rec.timestamp.replace(hour=period_hour, minute=0, second=0, microsecond=0)
            periods[period_time].append(rec.funding_rate)

        for period_time, hourly_rates in periods.items():
            if len(hourly_rates) < 4:  # Need at least half the hours
                continue

            # Sum hourly rates for 8h equivalent
            funding_8h = sum(hourly_rates)
            avg_hourly = funding_8h / len(hourly_rates)

            stats['total_periods'] += 1

            if funding_8h > 0:
                stats['positive_funding_periods'] += 1
                direction = "short_hl"  # Short HL perp (receive funding), long spot
            else:
                stats['negative_funding_periods'] += 1
                direction = "long_hl"  # Long HL perp (receive funding), short spot margin

            abs_funding = abs(funding_8h)
            borrow_cost = get_borrow_cost_8h(coin, direction, margin_rates)
            net_yield = abs_funding - borrow_cost

            # Track stats
            stats['by_coin'][coin]['periods'] += 1
            stats['by_coin'][coin]['gross'] += abs_funding
            stats['by_coin'][coin]['borrow'] += borrow_cost
            stats['by_coin'][coin]['net'] += max(0, net_yield)
            if funding_8h > 0:
                stats['by_coin'][coin]['positive_count'] += 1
            else:
                stats['by_coin'][coin]['negative_count'] += 1

            month_key = period_time.strftime('%Y-%m')
            stats['by_month'][month_key]['periods'] += 1
            stats['by_month'][month_key]['gross'] += abs_funding
            stats['by_month'][month_key]['net'] += max(0, net_yield)

            if abs_funding >= min_funding_8h:
                stats['tradeable_periods'] += 1
                stats['total_gross_yield'] += abs_funding
                stats['total_borrow_cost'] += borrow_cost

                if net_yield > entry_cost + exit_cost:
                    stats['profitable_periods'] += 1
                    stats['total_net_yield'] += net_yield - entry_cost - exit_cost

                opportunities.append(StrategyOpportunity(
                    timestamp=period_time,
                    coin=coin,
                    hl_funding_hourly=avg_hourly,
                    hl_funding_8h=funding_8h,
                    direction=direction,
                    gross_yield_8h=abs_funding,
                    borrow_cost_8h=borrow_cost,
                    net_yield_8h=net_yield
                ))

    return sorted(opportunities, key=lambda x: x.timestamp), stats


def simulate_trading(
    opportunities: List[StrategyOpportunity],
    capital: float = 10000,
    position_pct: float = 0.25,  # 25% per position
    max_positions: int = 4,
    min_net_yield: float = 0.001,  # 0.1% minimum net yield
    entry_cost: float = 0.001,
    exit_cost: float = 0.001,
    hold_periods: int = 3,  # Hold for 3 funding periods (24h)
) -> Dict:
    """Simulate the strategy with position management."""
    equity = capital
    positions = []  # (coin, entry_time, entry_yield, direction)
    trades = []

    for opp in opportunities:
        # Close mature positions
        new_positions = []
        for pos in positions:
            coin, entry_time, expected_yield, direction = pos
            periods_held = (opp.timestamp - entry_time).total_seconds() / (8 * 3600)

            if periods_held >= hold_periods:
                # Realize PnL
                gross = expected_yield * hold_periods * position_pct * capital
                costs = (entry_cost + exit_cost) * position_pct * capital
                net = gross - costs
                equity += net

                trades.append({
                    'coin': coin,
                    'entry': entry_time,
                    'exit': opp.timestamp,
                    'direction': direction,
                    'gross': gross,
                    'net': net
                })
            else:
                new_positions.append(pos)

        positions = new_positions

        # Open new position if criteria met
        if (opp.net_yield_8h >= min_net_yield and
            len(positions) < max_positions and
            opp.coin not in [p[0] for p in positions]):

            positions.append((opp.coin, opp.timestamp, opp.net_yield_8h, opp.direction))

    # Calculate results
    total_pnl = equity - capital
    total_return = (equity / capital - 1) * 100

    days = 0
    if opportunities:
        days = (opportunities[-1].timestamp - opportunities[0].timestamp).days

    annual_return = total_return * (365 / max(1, days)) if days > 0 else 0

    return {
        'initial_capital': capital,
        'final_equity': equity,
        'total_pnl': total_pnl,
        'total_return_pct': total_return,
        'annualized_return_pct': annual_return,
        'total_trades': len(trades),
        'winning_trades': sum(1 for t in trades if t['net'] > 0),
        'avg_trade_pnl': sum(t['net'] for t in trades) / max(1, len(trades))
    }


def print_report(stats: Dict, simulation: Optional[Dict] = None):
    """Print analysis report."""
    print("\n" + "="*70)
    print("  HYPERLIQUID PERP + BINANCE SPOT MARGIN STRATEGY ANALYSIS")
    print("="*70)

    print(f"\n📊 FUNDING PERIOD ANALYSIS")
    print(f"  Total 8h periods analyzed:       {stats['total_periods']:,}")
    print(f"  Positive funding (short HL):     {stats['positive_funding_periods']:,} "
          f"({stats['positive_funding_periods']/max(1,stats['total_periods'])*100:.1f}%)")
    print(f"  Negative funding (long HL):      {stats['negative_funding_periods']:,} "
          f"({stats['negative_funding_periods']/max(1,stats['total_periods'])*100:.1f}%)")
    print(f"  Tradeable (≥0.05% funding):      {stats['tradeable_periods']:,}")
    print(f"  Profitable after costs:          {stats['profitable_periods']:,}")

    if stats['tradeable_periods'] > 0:
        avg_gross = stats['total_gross_yield'] / stats['tradeable_periods'] * 100
        avg_borrow = stats['total_borrow_cost'] / stats['tradeable_periods'] * 100
        print(f"\n  Average gross yield (8h):        {avg_gross:.4f}%")
        print(f"  Average borrow cost (8h):        {avg_borrow:.4f}%")
        print(f"  Total net yield (tradeable):     {stats['total_net_yield']*100:.2f}%")

    print(f"\n🪙 TOP COINS BY NET YIELD")
    sorted_coins = sorted(
        stats['by_coin'].items(),
        key=lambda x: x[1]['net'],
        reverse=True
    )[:12]

    print(f"  {'Coin':<6} {'Periods':>8} {'Pos%':>6} {'Gross':>10} {'Borrow':>10} {'Net':>10}")
    print(f"  {'-'*6} {'-'*8} {'-'*6} {'-'*10} {'-'*10} {'-'*10}")
    for coin, s in sorted_coins:
        pos_pct = s['positive_count'] / max(1, s['periods']) * 100
        print(f"  {coin:<6} {s['periods']:>8} {pos_pct:>5.0f}% "
              f"{s['gross']*100:>9.3f}% {s['borrow']*100:>9.3f}% {s['net']*100:>9.3f}%")

    print(f"\n📅 MONTHLY BREAKDOWN")
    for month, s in sorted(stats['by_month'].items())[-6:]:
        avg_per_period = s['net'] / max(1, s['periods']) * 100
        print(f"  {month}: {s['periods']:>4} periods, "
              f"gross {s['gross']*100:.2f}%, net {s['net']*100:.2f}%, "
              f"avg/period {avg_per_period:.3f}%")

    if simulation:
        print(f"\n💰 TRADING SIMULATION (${simulation['initial_capital']:,.0f} capital)")
        print(f"  Final Equity:        ${simulation['final_equity']:,.2f}")
        print(f"  Total P&L:           ${simulation['total_pnl']:,.2f}")
        print(f"  Total Return:        {simulation['total_return_pct']:.2f}%")
        print(f"  Annualized Return:   {simulation['annualized_return_pct']:.1f}%")
        print(f"  Total Trades:        {simulation['total_trades']}")
        if simulation['total_trades'] > 0:
            win_rate = simulation['winning_trades'] / simulation['total_trades'] * 100
            print(f"  Win Rate:            {win_rate:.1f}%")
            print(f"  Avg Trade P&L:       ${simulation['avg_trade_pnl']:.2f}")

    # Key insight
    print(f"\n💡 KEY INSIGHT")
    if stats['positive_funding_periods'] > stats['negative_funding_periods']:
        print(f"  HL funding is predominantly POSITIVE ({stats['positive_funding_periods']/max(1,stats['total_periods'])*100:.0f}%)")
        print(f"  → Strategy: SHORT HL perp + LONG Binance spot (no borrow cost)")
    else:
        print(f"  HL funding is predominantly NEGATIVE ({stats['negative_funding_periods']/max(1,stats['total_periods'])*100:.0f}%)")
        print(f"  → Strategy: LONG HL perp + SHORT Binance spot margin (incurs borrow)")

    print("\n" + "="*70)


def main():
    parser = argparse.ArgumentParser(description='Analyze HL perp + Binance spot margin strategy')
    parser.add_argument('--days', type=int, default=30, help='Days of history')
    parser.add_argument('--coins', nargs='+', default=None, help='Specific coins')
    parser.add_argument('--min-funding', type=float, default=0.05, help='Min funding %% to trade')
    parser.add_argument('--simulate', action='store_true', help='Run trading simulation')
    parser.add_argument('--capital', type=float, default=10000, help='Simulation capital')

    args = parser.parse_args()

    end_time = datetime.now(timezone.utc)
    start_time = end_time - timedelta(days=args.days)

    coins = args.coins if args.coins else LIQUID_COINS[:15]
    min_funding = args.min_funding / 100

    print(f"╔══════════════════════════════════════════════════════════════════╗")
    print(f"║  HL PERP + BINANCE SPOT MARGIN ANALYSIS                          ║")
    print(f"╠══════════════════════════════════════════════════════════════════╣")
    print(f"║  Period:     {start_time.strftime('%Y-%m-%d')} to {end_time.strftime('%Y-%m-%d')} ({args.days} days)")
    print(f"║  Coins:      {len(coins)} ({', '.join(coins[:5])}...)")
    print(f"║  Min Funding: {args.min_funding:.2f}%")
    print(f"╚══════════════════════════════════════════════════════════════════╝")

    # Collect data
    hl_data = collect_hl_funding(coins, start_time, end_time)

    # Analyze
    print("\nAnalyzing strategy opportunities...")
    opportunities, stats = analyze_strategy(hl_data, min_funding_8h=min_funding)
    print(f"  Found {len(opportunities)} tradeable opportunities")

    # Simulate if requested
    simulation = None
    if args.simulate:
        print("\nRunning trading simulation...")
        simulation = simulate_trading(opportunities, capital=args.capital)

    # Report
    print_report(stats, simulation)


if __name__ == '__main__':
    main()
