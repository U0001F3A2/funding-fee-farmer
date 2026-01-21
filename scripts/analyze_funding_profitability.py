#!/usr/bin/env python3
"""
Analyze historical profitability of single-venue delta-neutral funding farming.

Strategy: Long spot + Short perp (positive funding) OR Short spot + Long perp (negative funding)
Venue: Binance only

This script:
1. Fetches historical funding rates from Binance
2. Simulates the strategy with realistic trading costs and capital constraints
3. Calculates net APY after all costs

Usage:
    python scripts/analyze_funding_profitability.py --days 365
"""

import argparse
import time
from datetime import datetime, timedelta, timezone
from dataclasses import dataclass, field
from typing import List, Dict, Optional
import requests

# Binance API
FUTURES_API = "https://fapi.binance.com"

# Strategy parameters (matching config defaults)
MIN_FUNDING_RATE = 0.001  # 0.1% per 8h threshold to enter
MIN_VOLUME_24H = 100_000_000  # $100M minimum volume

# Cost model
TRADING_FEE = 0.0004  # 0.04% taker fee (Binance futures)
SPOT_FEE = 0.001  # 0.1% spot fee (without BNB discount)
SLIPPAGE = 0.0001  # 0.01% slippage estimate
ENTRY_COST = (TRADING_FEE + SPOT_FEE + SLIPPAGE) * 2  # Both legs ~0.3%
EXIT_COST = ENTRY_COST
ROUND_TRIP_COST = ENTRY_COST + EXIT_COST  # ~0.6% total

# Position parameters
MAX_POSITIONS = 3  # Maximum simultaneous positions
POSITION_SIZE = 0.30  # 30% of capital per position


@dataclass
class FundingEvent:
    timestamp: datetime
    symbol: str
    rate: float  # 8h rate as decimal (0.001 = 0.1%)
    volume_24h: float


@dataclass
class Position:
    symbol: str
    entry_time: datetime
    entry_rate: float
    size: float  # Fraction of capital
    direction: str
    funding_collected: float = 0.0
    periods_held: int = 0


def fetch_funding_rates(symbol: str, start_ms: int, end_ms: int) -> List[dict]:
    """Fetch historical funding rates."""
    url = f"{FUTURES_API}/fapi/v1/fundingRate"
    all_rates = []
    current = start_ms

    while current < end_ms:
        params = {
            'symbol': symbol,
            'startTime': current,
            'endTime': end_ms,
            'limit': 1000
        }

        resp = requests.get(url, params=params)
        if resp.status_code == 429:
            time.sleep(60)
            continue
        resp.raise_for_status()

        data = resp.json()
        if not data:
            break

        all_rates.extend(data)
        current = data[-1]['fundingTime'] + 1
        time.sleep(0.1)

    return all_rates


def fetch_24h_volume(symbol: str) -> float:
    """Get current 24h volume for a symbol."""
    url = f"{FUTURES_API}/fapi/v1/ticker/24hr"
    resp = requests.get(url, params={'symbol': symbol})
    resp.raise_for_status()
    data = resp.json()
    return float(data['quoteVolume'])


def get_top_symbols(n: int = 15) -> List[str]:
    """Get top N futures symbols by volume."""
    url = f"{FUTURES_API}/fapi/v1/ticker/24hr"
    resp = requests.get(url)
    resp.raise_for_status()

    tickers = [t for t in resp.json() if t['symbol'].endswith('USDT')]
    sorted_tickers = sorted(tickers, key=lambda x: float(x['quoteVolume']), reverse=True)

    # Filter out meme/pump coins, keep established ones
    stable_symbols = []
    skip_patterns = ['1000', 'PEPE', 'SHIB', 'FLOKI', 'BONK', 'WIF', 'MEME', 'ALPACA', 'RIVER']
    for t in sorted_tickers:
        symbol = t['symbol']
        if not any(p in symbol for p in skip_patterns):
            stable_symbols.append(symbol)
        if len(stable_symbols) >= n:
            break

    return stable_symbols


def simulate_strategy(events: List[FundingEvent], capital: float = 100000) -> Dict:
    """
    Simulate funding farming with realistic capital constraints.

    - Limited to MAX_POSITIONS simultaneous positions
    - Each position uses POSITION_SIZE of capital
    - Properly accounts for capital allocation
    """

    # Sort events by time
    events = sorted(events, key=lambda e: e.timestamp)

    if not events:
        return {'error': 'No events'}

    # Track state
    positions: Dict[str, Position] = {}

    # Track metrics in USD
    total_funding_usd = 0.0
    total_entry_costs_usd = 0.0
    total_exit_costs_usd = 0.0
    positions_opened = 0
    funding_events_captured = 0

    # Group events by timestamp
    events_by_time: Dict[datetime, List[FundingEvent]] = {}
    for e in events:
        if e.timestamp not in events_by_time:
            events_by_time[e.timestamp] = []
        events_by_time[e.timestamp].append(e)

    for ts in sorted(events_by_time.keys()):
        period_events = events_by_time[ts]

        # Process existing positions first (collect funding or exit)
        symbols_to_close = []
        for symbol, pos in positions.items():
            # Find this symbol's event
            sym_event = next((e for e in period_events if e.symbol == symbol), None)
            if not sym_event:
                continue

            rate = sym_event.rate
            abs_rate = abs(rate)

            # Check direction alignment
            is_positive = rate > 0
            was_positive = "long_spot" in pos.direction

            if (is_positive == was_positive):
                # Still aligned - collect funding
                funding_usd = abs_rate * pos.size * capital
                total_funding_usd += funding_usd
                pos.funding_collected += abs_rate
                pos.periods_held += 1
                funding_events_captured += 1

                # Exit if rate dropped significantly
                if abs_rate < MIN_FUNDING_RATE * 0.3:
                    symbols_to_close.append(symbol)
            else:
                # Direction flipped - exit
                symbols_to_close.append(symbol)

        # Close positions
        for symbol in symbols_to_close:
            pos = positions.pop(symbol)
            exit_cost_usd = EXIT_COST * pos.size * capital
            total_exit_costs_usd += exit_cost_usd

        # Open new positions if we have capacity
        if len(positions) < MAX_POSITIONS:
            # Find best opportunities
            opportunities = []
            for event in period_events:
                if event.symbol in positions:
                    continue
                if abs(event.rate) >= MIN_FUNDING_RATE:
                    if event.volume_24h >= MIN_VOLUME_24H:
                        opportunities.append(event)

            # Sort by rate (highest first)
            opportunities.sort(key=lambda e: abs(e.rate), reverse=True)

            # Open positions up to limit
            for event in opportunities:
                if len(positions) >= MAX_POSITIONS:
                    break

                direction = "long_spot_short_perp" if event.rate > 0 else "short_spot_long_perp"
                positions[event.symbol] = Position(
                    symbol=event.symbol,
                    entry_time=ts,
                    entry_rate=event.rate,
                    size=POSITION_SIZE,
                    direction=direction,
                    funding_collected=abs(event.rate),
                    periods_held=1
                )

                # Collect first funding immediately
                funding_usd = abs(event.rate) * POSITION_SIZE * capital
                total_funding_usd += funding_usd
                funding_events_captured += 1

                # Entry cost
                entry_cost_usd = ENTRY_COST * POSITION_SIZE * capital
                total_entry_costs_usd += entry_cost_usd
                positions_opened += 1

    # Close remaining positions
    for symbol, pos in positions.items():
        exit_cost_usd = EXIT_COST * pos.size * capital
        total_exit_costs_usd += exit_cost_usd

    # Calculate results
    total_costs_usd = total_entry_costs_usd + total_exit_costs_usd
    net_profit_usd = total_funding_usd - total_costs_usd

    days = (events[-1].timestamp - events[0].timestamp).days
    days = max(days, 1)

    # Calculate APY based on capital
    gross_return = total_funding_usd / capital
    net_return = net_profit_usd / capital

    gross_apy = (gross_return * 365 / days) * 100
    net_apy = (net_return * 365 / days) * 100

    return {
        'days': days,
        'capital': capital,
        'total_funding_usd': total_funding_usd,
        'total_costs_usd': total_costs_usd,
        'net_profit_usd': net_profit_usd,
        'positions_opened': positions_opened,
        'funding_events': funding_events_captured,
        'gross_return_pct': gross_return * 100,
        'net_return_pct': net_return * 100,
        'gross_apy': gross_apy,
        'net_apy': net_apy,
        'cost_ratio': total_costs_usd / total_funding_usd if total_funding_usd > 0 else 0,
    }


def main():
    parser = argparse.ArgumentParser(description='Analyze funding farming profitability')
    parser.add_argument('--days', type=int, default=365, help='Days of history to analyze')
    parser.add_argument('--symbols', type=int, default=15, help='Number of top symbols to analyze')
    parser.add_argument('--capital', type=float, default=100000, help='Starting capital in USD')
    args = parser.parse_args()

    print()
    print("=" * 70)
    print("  SINGLE-VENUE FUNDING FARMING PROFITABILITY ANALYSIS")
    print("=" * 70)
    print()
    print(f"  Period: Last {args.days} days")
    print(f"  Capital: ${args.capital:,.0f}")
    print(f"  Strategy: Delta-neutral (spot + perp hedge) on Binance")
    print(f"  Max positions: {MAX_POSITIONS} @ {POSITION_SIZE*100:.0f}% each")
    print(f"  Entry threshold: {MIN_FUNDING_RATE*100:.2f}% per 8h")
    print(f"  Round-trip cost: ~{ROUND_TRIP_COST*100:.2f}%")
    print()

    # Get top symbols
    print("Fetching top symbols by volume...")
    symbols = get_top_symbols(args.symbols)
    print(f"Analyzing: {', '.join(symbols)}")
    print()

    # Time range
    end_time = datetime.now(timezone.utc)
    start_time = end_time - timedelta(days=args.days)
    start_ms = int(start_time.timestamp() * 1000)
    end_ms = int(end_time.timestamp() * 1000)

    # Fetch data
    all_events: List[FundingEvent] = []

    for symbol in symbols:
        print(f"Fetching {symbol}...", end=" ", flush=True)
        try:
            rates = fetch_funding_rates(symbol, start_ms, end_ms)
            volume = fetch_24h_volume(symbol)

            for r in rates:
                event = FundingEvent(
                    timestamp=datetime.fromtimestamp(r['fundingTime'] / 1000, tz=timezone.utc),
                    symbol=symbol,
                    rate=float(r['fundingRate']),
                    volume_24h=volume
                )
                all_events.append(event)

            print(f"{len(rates)} events")
            time.sleep(0.2)
        except Exception as e:
            print(f"Error: {e}")

    print()
    print("=" * 70)
    print("  SIMULATION RESULTS")
    print("=" * 70)
    print()

    # Run simulation
    results = simulate_strategy(all_events, capital=args.capital)

    if 'error' in results:
        print(f"  Error: {results['error']}")
        return

    print(f"  Analysis Period:     {results['days']} days")
    print(f"  Starting Capital:    ${results['capital']:,.0f}")
    print(f"  Positions Opened:    {results['positions_opened']}")
    print(f"  Funding Events:      {results['funding_events']}")
    print()
    print(f"  Gross Funding:       ${results['total_funding_usd']:,.2f} ({results['gross_return_pct']:.1f}%)")
    print(f"  Trading Costs:       ${results['total_costs_usd']:,.2f}")
    print(f"  Net Profit:          ${results['net_profit_usd']:,.2f} ({results['net_return_pct']:.1f}%)")
    print()
    print(f"  ┌────────────────────────────────────────┐")
    print(f"  │  GROSS APY:  {results['gross_apy']:>6.1f}%                   │")
    print(f"  │  NET APY:    {results['net_apy']:>6.1f}%                   │")
    print(f"  └────────────────────────────────────────┘")
    print()
    print(f"  Cost as % of funding: {results['cost_ratio']*100:.1f}%")
    print()

    # Rate distribution
    print("=" * 70)
    print("  FUNDING RATE DISTRIBUTION (all symbols)")
    print("=" * 70)
    print()

    rates = [e.rate for e in all_events]
    abs_rates = [abs(r) for r in rates]

    above_01 = len([r for r in abs_rates if r >= 0.001])
    above_02 = len([r for r in abs_rates if r >= 0.002])
    above_05 = len([r for r in abs_rates if r >= 0.005])

    total = len(rates)
    print(f"  Total funding events: {total}")
    print(f"  Events >= 0.1%:       {above_01} ({above_01/total*100:.1f}%)")
    print(f"  Events >= 0.2%:       {above_02} ({above_02/total*100:.1f}%)")
    print(f"  Events >= 0.5%:       {above_05} ({above_05/total*100:.1f}%)")
    print()

    # Verdict
    print("=" * 70)
    print("  VERDICT")
    print("=" * 70)
    print()

    if results['net_apy'] > 15:
        print("  ✓ PROFITABLE - Strategy is viable")
        print(f"    Expected ~{results['net_apy']:.0f}% annual return after costs")
    elif results['net_apy'] > 5:
        print("  ~ MARGINAL - Modest returns")
        print(f"    ~{results['net_apy']:.0f}% APY may not justify operational complexity")
    else:
        print("  ✗ NOT WORTH IT")
        print(f"    Only ~{results['net_apy']:.0f}% APY after costs")
        print("    Better alternatives: staking, lending, simple holding")

    print()


if __name__ == '__main__':
    main()
