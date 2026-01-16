#!/usr/bin/env python3
"""
Fetch historical funding rate data from Binance API.

This script collects:
- Funding rates (every 8 hours)
- Price data (from klines)
- 24h volume
- Open interest

Output: CSV file compatible with the backtesting module.

Usage:
    python scripts/fetch_historical_data.py --start 2024-01-01 --end 2024-06-01 --output data/funding_data.csv
"""

import argparse
import csv
import time
from datetime import datetime, timedelta
from typing import Dict, List, Optional
import requests
from dataclasses import dataclass

# Binance API endpoints
FUTURES_BASE_URL = "https://fapi.binance.com"
SPOT_BASE_URL = "https://api.binance.com"

# Rate limiting
REQUEST_DELAY = 0.1  # seconds between requests


@dataclass
class FundingSnapshot:
    timestamp: datetime
    symbol: str
    funding_rate: float
    price: float
    volume_24h: float
    spread: float
    open_interest: float


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
    dt = datetime.utcfromtimestamp(timestamp_ms / 1000)
    hour = dt.hour

    # Find nearest funding hour
    funding_hours = [0, 8, 16]
    nearest_hour = min(funding_hours, key=lambda h: abs(h - hour) if abs(h - hour) <= 4 else 24)

    aligned = dt.replace(hour=nearest_hour, minute=0, second=0, microsecond=0)
    return int(aligned.timestamp() * 1000)


def collect_symbol_data(
    symbol: str,
    start_time: int,
    end_time: int
) -> Dict[int, FundingSnapshot]:
    """Collect all data for a single symbol."""
    print(f"  Fetching funding rates for {symbol}...")
    funding_rates = fetch_funding_rates(symbol, start_time, end_time)

    print(f"  Fetching price data for {symbol}...")
    klines = fetch_klines(symbol, start_time, end_time)

    print(f"  Fetching open interest for {symbol}...")
    open_interest = fetch_open_interest_history(symbol, start_time, end_time)

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

    # Merge data at funding times
    snapshots = {}

    for funding_time, funding_rate in funding_by_time.items():
        # Find nearest kline
        nearest_kline_time = min(kline_by_time.keys(), key=lambda t: abs(t - funding_time), default=None)
        if nearest_kline_time is None:
            continue

        kline = kline_by_time[nearest_kline_time]

        # Find nearest OI
        nearest_oi_time = min(oi_by_time.keys(), key=lambda t: abs(t - funding_time), default=None)
        oi_value = oi_by_time.get(nearest_oi_time, 0) if nearest_oi_time else 0

        # Calculate spread estimate (use high-low as proxy)
        price = kline['close']
        spread = (kline['high'] - kline['low']) / price if price > 0 else 0.001
        spread = min(spread, 0.01)  # Cap at 1%

        snapshot = FundingSnapshot(
            timestamp=datetime.utcfromtimestamp(funding_time / 1000),
            symbol=symbol,
            funding_rate=funding_rate,
            price=price,
            volume_24h=kline['quote_volume'] * 3,  # Approximate 24h from 8h
            spread=spread,
            open_interest=oi_value
        )

        snapshots[funding_time] = snapshot

    return snapshots


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
    parser.add_argument('--symbols', nargs='+', help='Specific symbols to fetch (default: top 20 by volume)')
    parser.add_argument('--top', type=int, default=20, help='Number of top symbols to fetch')

    args = parser.parse_args()

    # Parse dates
    start_date = datetime.strptime(args.start, '%Y-%m-%d')
    end_date = datetime.strptime(args.end, '%Y-%m-%d')

    start_time = int(start_date.timestamp() * 1000)
    end_time = int(end_date.timestamp() * 1000)

    print(f"Fetching data from {args.start} to {args.end}")

    # Get symbols
    if args.symbols:
        symbols = args.symbols
    else:
        print(f"Fetching top {args.top} symbols by volume...")
        symbols = get_top_futures_symbols(args.top)

    print(f"Symbols: {', '.join(symbols)}")

    # Collect data for each symbol
    all_snapshots = []

    for i, symbol in enumerate(symbols):
        print(f"\n[{i+1}/{len(symbols)}] Processing {symbol}...")
        try:
            snapshots = collect_symbol_data(symbol, start_time, end_time)
            all_snapshots.extend(snapshots.values())
            print(f"  Collected {len(snapshots)} snapshots")
        except Exception as e:
            print(f"  Error: {e}")
            continue

        # Rate limiting between symbols
        time.sleep(1)

    print(f"\nTotal snapshots collected: {len(all_snapshots)}")

    # Write to CSV
    write_csv(all_snapshots, args.output)
    print(f"Data written to: {args.output}")


if __name__ == '__main__':
    main()
