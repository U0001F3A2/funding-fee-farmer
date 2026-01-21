#!/usr/bin/env python3
"""
Cross-venue funding rate historical analysis.

Compares historical funding rates between Hyperliquid and Binance
to analyze potential profitability of cross-venue arbitrage.

Usage:
    # Analyze last 30 days for major pairs
    python scripts/analyze_cross_venue.py --days 30

    # Analyze specific coins
    python scripts/analyze_cross_venue.py --days 90 --coins BTC ETH SOL

    # Full analysis with cost simulation
    python scripts/analyze_cross_venue.py --days 180 --simulate
"""

import argparse
import json
import time
from datetime import datetime, timedelta, timezone
from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple
import requests
from collections import defaultdict

# API endpoints
BINANCE_FUTURES_URL = "https://fapi.binance.com"
HYPERLIQUID_URL = "https://api.hyperliquid.xyz"

# Request rate limiting
REQUEST_DELAY = 0.2  # seconds

# Major coins available on both venues
MAJOR_COINS = [
    "BTC", "ETH", "SOL", "XRP", "DOGE", "ADA", "AVAX", "LINK",
    "DOT", "MATIC", "LTC", "ATOM", "ARB", "OP", "APT", "SUI",
    "INJ", "TIA", "SEI", "NEAR", "FTM", "AAVE", "UNI", "MKR"
]


@dataclass
class FundingRecord:
    """Single funding rate record."""
    timestamp: datetime
    coin: str
    rate_8h: float  # Normalized to 8h
    venue: str


@dataclass
class SpreadOpportunity:
    """Historical spread between venues."""
    timestamp: datetime
    coin: str
    hl_rate_8h: float
    binance_rate_8h: float
    spread_8h: float
    direction: str  # "long_hl_short_binance" or "long_binance_short_hl"


def fetch_hyperliquid_funding_history(coin: str, start_time_ms: int, end_time_ms: Optional[int] = None) -> List[dict]:
    """Fetch historical funding rates from Hyperliquid."""
    url = f"{HYPERLIQUID_URL}/info"
    payload = {
        "type": "fundingHistory",
        "coin": coin,
        "startTime": start_time_ms
    }
    if end_time_ms:
        payload["endTime"] = end_time_ms

    try:
        response = requests.post(url, json=payload, timeout=30)
        response.raise_for_status()
        return response.json()
    except Exception as e:
        print(f"  Warning: Failed to fetch HL funding for {coin}: {e}")
        return []


def fetch_binance_funding_history(symbol: str, start_time_ms: int, end_time_ms: int) -> List[dict]:
    """Fetch historical funding rates from Binance."""
    url = f"{BINANCE_FUTURES_URL}/fapi/v1/fundingRate"
    all_rates = []
    current_start = start_time_ms

    while current_start < end_time_ms:
        params = {
            'symbol': symbol,
            'startTime': current_start,
            'endTime': end_time_ms,
            'limit': 1000
        }

        try:
            response = requests.get(url, params=params, timeout=30)
            if response.status_code == 429:
                print("  Rate limited, waiting...")
                time.sleep(60)
                continue
            response.raise_for_status()

            data = response.json()
            if not data:
                break

            all_rates.extend(data)

            # Move to next batch
            last_time = data[-1]['fundingTime']
            if last_time <= current_start:
                break
            current_start = last_time + 1

            time.sleep(REQUEST_DELAY)
        except Exception as e:
            print(f"  Warning: Failed to fetch Binance funding for {symbol}: {e}")
            break

    return all_rates


def collect_funding_data(coins: List[str], start_time: datetime, end_time: datetime) -> Tuple[Dict, Dict]:
    """Collect funding data from both venues."""
    start_ms = int(start_time.timestamp() * 1000)
    end_ms = int(end_time.timestamp() * 1000)

    hl_data = {}  # coin -> [(timestamp, rate_hourly), ...]
    binance_data = {}  # coin -> [(timestamp, rate_8h), ...]

    print(f"\nFetching data for {len(coins)} coins...")

    for i, coin in enumerate(coins):
        print(f"  [{i+1}/{len(coins)}] {coin}...")

        # Fetch Hyperliquid
        hl_records = fetch_hyperliquid_funding_history(coin, start_ms, end_ms)
        if hl_records:
            hl_data[coin] = [
                (datetime.fromtimestamp(r['time'] / 1000, tz=timezone.utc), float(r['fundingRate']))
                for r in hl_records
            ]
            print(f"    HL: {len(hl_records)} records")

        time.sleep(REQUEST_DELAY)

        # Fetch Binance
        binance_symbol = f"{coin}USDT"
        binance_records = fetch_binance_funding_history(binance_symbol, start_ms, end_ms)
        if binance_records:
            binance_data[coin] = [
                (datetime.fromtimestamp(r['fundingTime'] / 1000, tz=timezone.utc), float(r['fundingRate']))
                for r in binance_records
            ]
            print(f"    Binance: {len(binance_records)} records")

        time.sleep(REQUEST_DELAY)

    return hl_data, binance_data


def align_and_calculate_spreads(
    hl_data: Dict,
    binance_data: Dict,
    window_hours: int = 4  # Window to match funding times
) -> List[SpreadOpportunity]:
    """
    Align Hyperliquid (hourly) with Binance (8h) funding times
    and calculate historical spreads.
    """
    spreads = []

    for coin in set(hl_data.keys()) & set(binance_data.keys()):
        hl_records = hl_data[coin]
        binance_records = binance_data[coin]

        # Index HL records by timestamp for lookup
        hl_by_time = {ts: rate for ts, rate in hl_records}

        # For each Binance funding time, find surrounding HL rates
        for binance_ts, binance_rate in binance_records:
            # Accumulate HL hourly rates over the 8h period before Binance settlement
            # HL settles hourly, so we sum 8 hours of HL funding
            hl_8h_equivalent = 0.0
            hl_count = 0

            for hours_back in range(8):
                check_time = binance_ts - timedelta(hours=hours_back)
                # Look for HL rate within +/- 30 min window
                for hl_ts, hl_rate in hl_records:
                    if abs((hl_ts - check_time).total_seconds()) < 1800:  # 30 min
                        hl_8h_equivalent += hl_rate
                        hl_count += 1
                        break

            if hl_count < 4:  # Need at least half the HL records
                continue

            # Calculate spread
            spread = hl_8h_equivalent - binance_rate

            # Determine direction
            if spread > 0:
                direction = "long_binance_short_hl"
            else:
                direction = "long_hl_short_binance"

            spreads.append(SpreadOpportunity(
                timestamp=binance_ts,
                coin=coin,
                hl_rate_8h=hl_8h_equivalent,
                binance_rate_8h=binance_rate,
                spread_8h=spread,
                direction=direction
            ))

    return sorted(spreads, key=lambda x: x.timestamp)


def analyze_profitability(
    spreads: List[SpreadOpportunity],
    min_spread: float = 0.0005,  # 0.05% minimum spread to trade
    entry_fee: float = 0.0002,   # 0.02% per leg (2 legs = 0.04% total)
    exit_fee: float = 0.0002,
    slippage: float = 0.0001,    # 0.01% per leg
) -> Dict:
    """
    Analyze profitability of cross-venue arbitrage.

    Cost model:
    - Entry: 2 legs × (fee + slippage) = 2 × 0.03% = 0.06%
    - Exit: 2 legs × (fee + slippage) = 2 × 0.03% = 0.06%
    - Total round-trip: 0.12%
    """
    total_cost_per_trade = 2 * (entry_fee + slippage + exit_fee + slippage)

    results = {
        'total_opportunities': len(spreads),
        'tradeable_opportunities': 0,
        'profitable_after_costs': 0,
        'total_gross_spread': 0.0,
        'total_net_profit': 0.0,
        'by_coin': defaultdict(lambda: {
            'count': 0, 'gross': 0.0, 'net': 0.0, 'avg_spread': 0.0
        }),
        'by_month': defaultdict(lambda: {
            'count': 0, 'gross': 0.0, 'net': 0.0
        }),
        'spread_distribution': {
            '0-0.05%': 0, '0.05-0.1%': 0, '0.1-0.2%': 0,
            '0.2-0.5%': 0, '0.5-1%': 0, '>1%': 0
        }
    }

    for opp in spreads:
        abs_spread = abs(opp.spread_8h)

        # Categorize spread
        if abs_spread < 0.0005:
            results['spread_distribution']['0-0.05%'] += 1
        elif abs_spread < 0.001:
            results['spread_distribution']['0.05-0.1%'] += 1
        elif abs_spread < 0.002:
            results['spread_distribution']['0.1-0.2%'] += 1
        elif abs_spread < 0.005:
            results['spread_distribution']['0.2-0.5%'] += 1
        elif abs_spread < 0.01:
            results['spread_distribution']['0.5-1%'] += 1
        else:
            results['spread_distribution']['>1%'] += 1

        if abs_spread >= min_spread:
            results['tradeable_opportunities'] += 1
            results['total_gross_spread'] += abs_spread

            net_profit = abs_spread - total_cost_per_trade
            if net_profit > 0:
                results['profitable_after_costs'] += 1
                results['total_net_profit'] += net_profit

            # By coin
            results['by_coin'][opp.coin]['count'] += 1
            results['by_coin'][opp.coin]['gross'] += abs_spread
            results['by_coin'][opp.coin]['net'] += max(0, net_profit)

            # By month
            month_key = opp.timestamp.strftime('%Y-%m')
            results['by_month'][month_key]['count'] += 1
            results['by_month'][month_key]['gross'] += abs_spread
            results['by_month'][month_key]['net'] += max(0, net_profit)

    # Calculate averages
    for coin_stats in results['by_coin'].values():
        if coin_stats['count'] > 0:
            coin_stats['avg_spread'] = coin_stats['gross'] / coin_stats['count']

    return results


def simulate_trading(
    spreads: List[SpreadOpportunity],
    capital: float = 10000,
    position_size_pct: float = 0.20,  # 20% per position
    max_positions: int = 3,
    min_spread: float = 0.0005,
    hold_periods: int = 1,  # Hold for 1 funding period
    entry_cost: float = 0.0003,  # 0.03% per leg
    exit_cost: float = 0.0003,
) -> Dict:
    """
    Simulate actual trading with position management.
    """
    equity = capital
    positions = []  # (coin, entry_time, entry_spread, direction)

    trades = []
    equity_curve = [(spreads[0].timestamp if spreads else datetime.now(timezone.utc), equity)]

    for opp in spreads:
        abs_spread = abs(opp.spread_8h)

        # Close positions that have held long enough
        new_positions = []
        for pos in positions:
            coin, entry_time, entry_spread, direction = pos
            periods_held = (opp.timestamp - entry_time).total_seconds() / (8 * 3600)

            if periods_held >= hold_periods:
                # Close position
                gross_pnl = abs(entry_spread) * position_size_pct * equity
                costs = 2 * exit_cost * position_size_pct * equity
                net_pnl = gross_pnl - costs
                equity += net_pnl

                trades.append({
                    'coin': coin,
                    'entry': entry_time,
                    'exit': opp.timestamp,
                    'gross': gross_pnl,
                    'net': net_pnl
                })
            else:
                new_positions.append(pos)

        positions = new_positions

        # Open new position if opportunity meets criteria
        if (abs_spread >= min_spread and
            len(positions) < max_positions and
            opp.coin not in [p[0] for p in positions]):

            # Pay entry costs
            entry_costs = 2 * entry_cost * position_size_pct * equity
            equity -= entry_costs

            positions.append((opp.coin, opp.timestamp, opp.spread_8h, opp.direction))

        equity_curve.append((opp.timestamp, equity))

    # Calculate final stats
    total_pnl = equity - capital
    total_return_pct = (equity / capital - 1) * 100

    # Annualize
    if spreads:
        days = (spreads[-1].timestamp - spreads[0].timestamp).days
        if days > 0:
            annual_return = total_return_pct * (365 / days)
        else:
            annual_return = 0
    else:
        annual_return = 0

    return {
        'initial_capital': capital,
        'final_equity': equity,
        'total_pnl': total_pnl,
        'total_return_pct': total_return_pct,
        'annualized_return_pct': annual_return,
        'total_trades': len(trades),
        'winning_trades': sum(1 for t in trades if t['net'] > 0),
        'equity_curve': equity_curve[-100:]  # Last 100 points
    }


def print_analysis_report(results: Dict, simulation: Optional[Dict] = None):
    """Print formatted analysis report."""
    print("\n" + "="*70)
    print("  CROSS-VENUE FUNDING ARBITRAGE HISTORICAL ANALYSIS")
    print("="*70)

    print(f"\n📊 SPREAD OPPORTUNITIES")
    print(f"  Total funding events analyzed:   {results['total_opportunities']:,}")
    print(f"  Tradeable (≥0.05% spread):       {results['tradeable_opportunities']:,}")
    print(f"  Profitable after costs (0.12%):  {results['profitable_after_costs']:,}")

    if results['tradeable_opportunities'] > 0:
        win_rate = results['profitable_after_costs'] / results['tradeable_opportunities'] * 100
        avg_gross = results['total_gross_spread'] / results['tradeable_opportunities'] * 100
        print(f"  Win rate (after costs):          {win_rate:.1f}%")
        print(f"  Average gross spread:            {avg_gross:.3f}%")

    print(f"\n📈 SPREAD DISTRIBUTION")
    for bucket, count in results['spread_distribution'].items():
        pct = count / max(1, results['total_opportunities']) * 100
        bar = "█" * int(pct / 2)
        print(f"  {bucket:>10}: {count:5,} ({pct:5.1f}%) {bar}")

    print(f"\n🪙 TOP COINS BY OPPORTUNITY")
    sorted_coins = sorted(
        results['by_coin'].items(),
        key=lambda x: x[1]['net'],
        reverse=True
    )[:10]

    print(f"  {'Coin':<8} {'Opps':>6} {'Avg Spread':>12} {'Gross':>10} {'Net':>10}")
    print(f"  {'-'*8} {'-'*6} {'-'*12} {'-'*10} {'-'*10}")
    for coin, stats in sorted_coins:
        print(f"  {coin:<8} {stats['count']:>6} {stats['avg_spread']*100:>11.3f}% "
              f"{stats['gross']*100:>9.2f}% {stats['net']*100:>9.2f}%")

    print(f"\n📅 MONTHLY BREAKDOWN")
    for month, stats in sorted(results['by_month'].items())[-6:]:  # Last 6 months
        print(f"  {month}: {stats['count']:>4} opportunities, "
              f"gross {stats['gross']*100:.2f}%, net {stats['net']*100:.2f}%")

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

    print("\n" + "="*70)


def main():
    parser = argparse.ArgumentParser(description='Analyze cross-venue funding rate arbitrage')
    parser.add_argument('--days', type=int, default=30, help='Days of history to analyze')
    parser.add_argument('--coins', nargs='+', default=None, help='Specific coins to analyze')
    parser.add_argument('--min-spread', type=float, default=0.05, help='Min spread %% to consider')
    parser.add_argument('--simulate', action='store_true', help='Run trading simulation')
    parser.add_argument('--capital', type=float, default=10000, help='Simulation capital')
    parser.add_argument('--output', type=str, help='Output JSON file for detailed results')

    args = parser.parse_args()

    # Set time range
    end_time = datetime.now(timezone.utc)
    start_time = end_time - timedelta(days=args.days)

    coins = args.coins if args.coins else MAJOR_COINS[:15]  # Top 15 by default
    min_spread = args.min_spread / 100  # Convert to decimal

    print(f"╔══════════════════════════════════════════════════════════════════╗")
    print(f"║  CROSS-VENUE FUNDING RATE ANALYSIS                               ║")
    print(f"╠══════════════════════════════════════════════════════════════════╣")
    print(f"║  Period:     {start_time.strftime('%Y-%m-%d')} to {end_time.strftime('%Y-%m-%d')} ({args.days} days)")
    print(f"║  Coins:      {len(coins)} ({', '.join(coins[:5])}...)")
    print(f"║  Min Spread: {args.min_spread:.2f}%")
    print(f"╚══════════════════════════════════════════════════════════════════╝")

    # Collect data
    hl_data, binance_data = collect_funding_data(coins, start_time, end_time)

    # Calculate spreads
    print("\nCalculating cross-venue spreads...")
    spreads = align_and_calculate_spreads(hl_data, binance_data)
    print(f"  Found {len(spreads)} aligned funding events")

    # Analyze
    results = analyze_profitability(spreads, min_spread=min_spread)

    # Optional simulation
    simulation = None
    if args.simulate:
        print("\nRunning trading simulation...")
        simulation = simulate_trading(
            spreads,
            capital=args.capital,
            min_spread=min_spread
        )

    # Print report
    print_analysis_report(results, simulation)

    # Save detailed results
    if args.output:
        output_data = {
            'config': {
                'days': args.days,
                'coins': coins,
                'min_spread': min_spread
            },
            'results': {
                'total_opportunities': results['total_opportunities'],
                'tradeable': results['tradeable_opportunities'],
                'profitable': results['profitable_after_costs'],
                'spread_distribution': results['spread_distribution']
            },
            'by_coin': dict(results['by_coin']),
            'by_month': dict(results['by_month'])
        }
        if simulation:
            output_data['simulation'] = simulation

        with open(args.output, 'w') as f:
            json.dump(output_data, f, indent=2, default=str)
        print(f"\nDetailed results saved to: {args.output}")


if __name__ == '__main__':
    main()
