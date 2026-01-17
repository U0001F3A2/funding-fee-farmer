#!/usr/bin/env python3
"""
Fetch historical funding rate data from Binance API.

This script collects:
- Funding rates (every 8 hours)
- Price data (from klines)
- 24h volume
- Realistic spread/OI estimates based on market cap tier

Output: CSV file compatible with the backtesting module.

Usage:
    # Fetch all available history for major pairs
    python scripts/fetch_historical_data.py --start 2020-01-01 --end 2026-01-18 --output data/comprehensive_funding.csv --symbols BTCUSDT ETHUSDT SOLUSDT XRPUSDT BNBUSDT DOGEUSDT ADAUSDT AVAXUSDT LINKUSDT DOTUSDT MATICUSDT LTCUSDT ATOMUSDT ARBUSDT OPUSDT

    # Quick fetch with default settings
    python scripts/fetch_historical_data.py --start 2024-01-01 --end 2024-06-01 --output data/funding_data.csv
"""

import argparse
import csv
import time
import os
from datetime import datetime, timedelta, timezone
from typing import Dict, List, Optional
import requests
from dataclasses import dataclass

# Binance API endpoints
FUTURES_BASE_URL = "https://fapi.binance.com"
SPOT_BASE_URL = "https://api.binance.com"

# Rate limiting
REQUEST_DELAY = 0.15  # seconds between requests

# Historically significant pairs with long track records (avoid current pump-and-dump coins)
MAJOR_PAIRS = [
    "BTCUSDT", "ETHUSDT", "BNBUSDT", "SOLUSDT", "XRPUSDT",
    "DOGEUSDT", "ADAUSDT", "AVAXUSDT", "LINKUSDT", "DOTUSDT",
    "MATICUSDT", "LTCUSDT", "BCHUSDT", "ATOMUSDT", "ETCUSDT",
    "FILUSDT", "APTUSDT", "ARBUSDT", "OPUSDT", "NEARUSDT",
    "AAVEUSDT", "UNIUSDT", "XLMUSDT", "TRXUSDT", "ICPUSDT"
]

# Realistic spread estimates by market cap tier (based on historical order book data)
SPREAD_BY_TIER = {
    # Tier 1: Ultra liquid (BTC, ETH) - tightest spreads
    "BTCUSDT": 0.0001,   # 0.01%
    "ETHUSDT": 0.0001,   # 0.01%
    # Tier 2: Very liquid (top 10 alts)
    "BNBUSDT": 0.00012,
    "SOLUSDT": 0.00012,
    "XRPUSDT": 0.00012,
    "DOGEUSDT": 0.00015,
    "ADAUSDT": 0.00015,
    # Tier 3: Liquid (top 20)
    "AVAXUSDT": 0.00015,
    "LINKUSDT": 0.00015,
    "DOTUSDT": 0.00015,
    "MATICUSDT": 0.00015,
    "LTCUSDT": 0.00015,
    # Tier 4: Moderate liquidity
    "DEFAULT": 0.0002,   # 0.02%
}

# Realistic OI estimates by tier (in USD, based on historical averages)
OI_BY_TIER = {
    "BTCUSDT": 8_000_000_000,    # $8B
    "ETHUSDT": 4_000_000_000,    # $4B
    "BNBUSDT": 400_000_000,      # $400M
    "SOLUSDT": 1_500_000_000,    # $1.5B
    "XRPUSDT": 800_000_000,      # $800M
    "DOGEUSDT": 500_000_000,     # $500M
    "ADAUSDT": 300_000_000,      # $300M
    "AVAXUSDT": 200_000_000,     # $200M
    "LINKUSDT": 300_000_000,     # $300M
    "DOTUSDT": 150_000_000,      # $150M
    "MATICUSDT": 200_000_000,    # $200M
    "LTCUSDT": 200_000_000,      # $200M
    "BCHUSDT": 100_000_000,      # $100M
    "ATOMUSDT": 150_000_000,     # $150M
    "ETCUSDT": 150_000_000,      # $150M
    "ARBUSDT": 300_000_000,      # $300M
    "OPUSDT": 200_000_000,       # $200M
    "DEFAULT": 100_000_000,      # $100M for others
}


@dataclass
class FundingSnapshot:
    timestamp: datetime
    symbol: str
    funding_rate: float
    price: float
    volume_24h: float
    spread: float
    open_interest: float


def get_spread_estimate(symbol: str) -> float:
    """Get realistic spread estimate for a symbol."""
    return SPREAD_BY_TIER.get(symbol, SPREAD_BY_TIER["DEFAULT"])


def get_oi_estimate(symbol: str) -> float:
    """Get realistic OI estimate for a symbol."""
    return OI_BY_TIER.get(symbol, OI_BY_TIER["DEFAULT"])


def get_top_futures_symbols(limit: int = 20) -> List[str]:
    """Get top futures symbols by volume."""
    url = f"{FUTURES_BASE_URL}/fapi/v1/ticker/24hr"
    response = requests.get(url)
    response.raise_for_status()

    tickers = response.json()
    # Filter USDT pairs and sort by volume
    usdt_pairs = [t for t in tickers if t['symbol'].endswith('USDT')]
    sorted_pairs = sorted(usdt_pairs, key=lambda x: float(x['quoteVolume']), reverse=True)

    return [p['symbol'] for p in sorted_pairs[:limit]]


def fetch_funding_rates(symbol: str, start_time: int, end_time: int) -> List[dict]:
    """Fetch historical funding rates for a symbol."""
    url = f"{FUTURES_BASE_URL}/fapi/v1/fundingRate"
    all_rates = []
    current_start = start_time

    while current_start < end_time:
        params = {
            'symbol': symbol,
            'startTime': current_start,
            'endTime': end_time,
            'limit': 1000
        }

        response = requests.get(url, params=params)
        if response.status_code == 429:
            print(f"Rate limited, waiting...")
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

    return all_rates


def fetch_klines(symbol: str, start_time: int, end_time: int, interval: str = '8h') -> List[dict]:
    """Fetch historical klines (candlestick) data."""
    url = f"{FUTURES_BASE_URL}/fapi/v1/klines"
    all_klines = []
    current_start = start_time

    while current_start < end_time:
        params = {
            'symbol': symbol,
            'interval': interval,
            'startTime': current_start,
            'endTime': end_time,
            'limit': 1500
        }

        response = requests.get(url, params=params)
        if response.status_code == 429:
            print(f"Rate limited, waiting...")
            time.sleep(60)
            continue
        response.raise_for_status()

        data = response.json()
        if not data:
            break

        all_klines.extend(data)

        # Move to next batch
        last_time = data[-1][0]
        if last_time <= current_start:
            break
        current_start = last_time + 1

        time.sleep(REQUEST_DELAY)

    return all_klines


def fetch_open_interest_history(symbol: str, start_time: int, end_time: int) -> List[dict]:
    """Fetch historical open interest data."""
    url = f"{FUTURES_BASE_URL}/futures/data/openInterestHist"
    all_oi = []
    current_start = start_time

    while current_start < end_time:
        params = {
            'symbol': symbol,
            'period': '4h',  # 5m, 15m, 30m, 1h, 2h, 4h, 6h, 12h, 1d
            'startTime': current_start,
            'endTime': end_time,
            'limit': 500
        }

        response = requests.get(url, params=params)
        if response.status_code == 429:
            print(f"Rate limited, waiting...")
            time.sleep(60)
            continue

        if response.status_code != 200:
            print(f"Warning: Could not fetch OI for {symbol}: {response.status_code}")
            break

        data = response.json()
        if not data:
            break

        all_oi.extend(data)

        # Move to next batch
        last_time = data[-1]['timestamp']
        if last_time <= current_start:
            break
        current_start = last_time + 1

        time.sleep(REQUEST_DELAY)

    return all_oi


def align_to_funding_time(timestamp_ms: int) -> int:
    """Align timestamp to nearest funding time (00:00, 08:00, 16:00 UTC)."""
    dt = datetime.fromtimestamp(timestamp_ms / 1000, tz=timezone.utc)
    hour = dt.hour

    # Find nearest funding hour
    funding_hours = [0, 8, 16]
    nearest_hour = min(funding_hours, key=lambda h: abs(h - hour) if abs(h - hour) <= 4 else 24)

    aligned = dt.replace(hour=nearest_hour, minute=0, second=0, microsecond=0)
    return int(aligned.timestamp() * 1000)


def collect_symbol_data(
    symbol: str,
    start_time: int,
    end_time: int,
    use_realistic_estimates: bool = True
) -> Dict[int, FundingSnapshot]:
    """Collect all data for a single symbol."""
    print(f"  Fetching funding rates for {symbol}...")
    funding_rates = fetch_funding_rates(symbol, start_time, end_time)
    print(f"    Found {len(funding_rates)} funding rate records")

    print(f"  Fetching price data for {symbol}...")
    klines = fetch_klines(symbol, start_time, end_time)
    print(f"    Found {len(klines)} kline records")

    # Skip OI fetching - API is unreliable, use estimates instead
    open_interest = []
    if not use_realistic_estimates:
        print(f"  Fetching open interest for {symbol}...")
        open_interest = fetch_open_interest_history(symbol, start_time, end_time)
        print(f"    Found {len(open_interest)} OI records")

    # Index data by timestamp
    funding_by_time = {r['fundingTime']: float(r['fundingRate']) for r in funding_rates}

    kline_by_time = {}
    for k in klines:
        ts = k[0]
        kline_by_time[ts] = {
            'open': float(k[1]),
            'high': float(k[2]),
            'low': float(k[3]),
            'close': float(k[4]),
            'volume': float(k[5]),
            'quote_volume': float(k[7])
        }

    oi_by_time = {}
    for o in open_interest:
        ts = o['timestamp']
        oi_by_time[ts] = float(o['sumOpenInterestValue'])

    # Get realistic estimates for this symbol
    spread_estimate = get_spread_estimate(symbol)
    oi_estimate = get_oi_estimate(symbol)

    # Merge data at funding times
    snapshots = {}

    for funding_time, funding_rate in funding_by_time.items():
        # Find nearest kline
        if not kline_by_time:
            continue
        nearest_kline_time = min(kline_by_time.keys(), key=lambda t: abs(t - funding_time), default=None)
        if nearest_kline_time is None:
            continue

        kline = kline_by_time[nearest_kline_time]

        price = kline['close']

        if use_realistic_estimates:
            # Use realistic spread/OI estimates based on market cap tier
            spread = spread_estimate
            oi_value = oi_estimate
        else:
            # Find nearest OI from API data
            if oi_by_time:
                nearest_oi_time = min(oi_by_time.keys(), key=lambda t: abs(t - funding_time), default=None)
                oi_value = oi_by_time.get(nearest_oi_time, oi_estimate) if nearest_oi_time else oi_estimate
            else:
                oi_value = oi_estimate

            # Calculate spread from high-low (less accurate)
            spread = (kline['high'] - kline['low']) / price if price > 0 else spread_estimate
            spread = min(spread, 0.01)  # Cap at 1%

        snapshot = FundingSnapshot(
            timestamp=datetime.fromtimestamp(funding_time / 1000, tz=timezone.utc),
            symbol=symbol,
            funding_rate=funding_rate,
            price=price,
            volume_24h=kline['quote_volume'] * 3,  # Approximate 24h from 8h
            spread=spread,
            open_interest=oi_value
        )

        snapshots[funding_time] = snapshot

    return snapshots


def append_to_csv(snapshots: List[FundingSnapshot], output_path: str, write_header: bool = False):
    """Append snapshots to CSV file (for incremental saves)."""
    sorted_snapshots = sorted(snapshots, key=lambda s: (s.timestamp, s.symbol))

    mode = 'w' if write_header else 'a'
    with open(output_path, mode, newline='') as f:
        writer = csv.writer(f)
        if write_header:
            writer.writerow(['timestamp', 'symbol', 'funding_rate', 'price', 'volume_24h', 'spread', 'open_interest'])

        for s in sorted_snapshots:
            writer.writerow([
                s.timestamp.strftime('%Y-%m-%dT%H:%M:%SZ'),
                s.symbol,
                f"{s.funding_rate:.8f}",
                f"{s.price:.2f}",
                f"{s.volume_24h:.0f}",
                f"{s.spread:.6f}",
                f"{s.open_interest:.0f}"
            ])


def write_csv(snapshots: List[FundingSnapshot], output_path: str):
    """Write snapshots to CSV file."""
    # Sort by timestamp then symbol
    sorted_snapshots = sorted(snapshots, key=lambda s: (s.timestamp, s.symbol))

    with open(output_path, 'w', newline='') as f:
        writer = csv.writer(f)
        writer.writerow(['timestamp', 'symbol', 'funding_rate', 'price', 'volume_24h', 'spread', 'open_interest'])

        for s in sorted_snapshots:
            writer.writerow([
                s.timestamp.strftime('%Y-%m-%dT%H:%M:%SZ'),
                s.symbol,
                f"{s.funding_rate:.8f}",
                f"{s.price:.2f}",
                f"{s.volume_24h:.0f}",
                f"{s.spread:.6f}",
                f"{s.open_interest:.0f}"
            ])


def main():
    parser = argparse.ArgumentParser(description='Fetch historical funding rate data from Binance')
    parser.add_argument('--start', required=True, help='Start date (YYYY-MM-DD)')
    parser.add_argument('--end', required=True, help='End date (YYYY-MM-DD)')
    parser.add_argument('--output', default='data/funding_data.csv', help='Output CSV path')
    parser.add_argument('--symbols', nargs='+', help='Specific symbols to fetch (default: major pairs)')
    parser.add_argument('--top', type=int, default=0, help='Use top N symbols by current volume (0=use MAJOR_PAIRS)')
    parser.add_argument('--realistic', action='store_true', default=True,
                        help='Use realistic spread/OI estimates (default: True)')
    parser.add_argument('--incremental', action='store_true',
                        help='Save after each symbol (prevents data loss)')

    args = parser.parse_args()

    # Parse dates
    start_date = datetime.strptime(args.start, '%Y-%m-%d')
    end_date = datetime.strptime(args.end, '%Y-%m-%d')

    start_time = int(start_date.timestamp() * 1000)
    end_time = int(end_date.timestamp() * 1000)

    days = (end_date - start_date).days
    print(f"╔══════════════════════════════════════════════════════════════╗")
    print(f"║  COMPREHENSIVE HISTORICAL DATA FETCH                         ║")
    print(f"╠══════════════════════════════════════════════════════════════╣")
    print(f"║  Period: {args.start} to {args.end} ({days} days)            ")
    print(f"║  Expected funding events per symbol: ~{days * 3}              ")
    print(f"╚══════════════════════════════════════════════════════════════╝")

    # Get symbols
    if args.symbols:
        symbols = args.symbols
    elif args.top > 0:
        print(f"\nFetching top {args.top} symbols by current volume...")
        symbols = get_top_futures_symbols(args.top)
    else:
        print(f"\nUsing {len(MAJOR_PAIRS)} major pairs with long track records...")
        symbols = MAJOR_PAIRS

    print(f"Symbols: {', '.join(symbols)}")
    print(f"Output: {args.output}")
    print(f"Realistic estimates: {args.realistic}")
    print()

    # Collect data for each symbol
    all_snapshots = []
    total_collected = 0
    first_symbol = True

    for i, symbol in enumerate(symbols):
        print(f"\n{'='*60}")
        print(f"[{i+1}/{len(symbols)}] Processing {symbol}...")
        print(f"{'='*60}")

        try:
            snapshots = collect_symbol_data(
                symbol, start_time, end_time,
                use_realistic_estimates=args.realistic
            )
            snapshot_list = list(snapshots.values())
            all_snapshots.extend(snapshot_list)
            total_collected += len(snapshot_list)

            print(f"  ✓ Collected {len(snapshot_list)} snapshots (total: {total_collected})")

            # Incremental save if requested
            if args.incremental:
                append_to_csv(snapshot_list, args.output, write_header=first_symbol)
                first_symbol = False
                print(f"  ✓ Saved incrementally to {args.output}")

        except Exception as e:
            print(f"  ✗ Error: {e}")
            import traceback
            traceback.print_exc()
            continue

        # Rate limiting between symbols
        time.sleep(1)

    print(f"\n{'='*60}")
    print(f"COLLECTION COMPLETE")
    print(f"{'='*60}")
    print(f"Total snapshots collected: {total_collected}")

    # Write final CSV (unless incremental mode)
    if not args.incremental:
        write_csv(all_snapshots, args.output)
        print(f"Data written to: {args.output}")
    else:
        print(f"Data already saved incrementally to: {args.output}")

    # Print statistics
    if all_snapshots:
        rates = [s.funding_rate for s in all_snapshots]
        high_rates = [r for r in rates if abs(r) >= 0.001]
        extreme_rates = [r for r in rates if abs(r) >= 0.005]

        print(f"\n{'='*60}")
        print(f"DATA STATISTICS")
        print(f"{'='*60}")
        print(f"Funding rate range: {min(rates)*100:.4f}% to {max(rates)*100:.4f}%")
        print(f"Records >= 0.1%:    {len(high_rates)} ({len(high_rates)/len(rates)*100:.1f}%)")
        print(f"Records >= 0.5%:    {len(extreme_rates)} ({len(extreme_rates)/len(rates)*100:.1f}%)")


if __name__ == '__main__':
    main()
