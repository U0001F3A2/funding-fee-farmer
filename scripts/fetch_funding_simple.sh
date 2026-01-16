#!/bin/bash
# Simple script to fetch recent funding rate data from Binance
#
# Usage:
#   ./scripts/fetch_funding_simple.sh [OUTPUT_FILE] [NUM_SYMBOLS]
#
# Example:
#   ./scripts/fetch_funding_simple.sh data/funding_data.csv 10

set -e

OUTPUT_FILE="${1:-data/funding_data.csv}"
NUM_SYMBOLS="${2:-20}"

FUTURES_URL="https://fapi.binance.com"

echo "Fetching top $NUM_SYMBOLS futures symbols by volume..."

# Get top symbols by volume
SYMBOLS=$(curl -s "${FUTURES_URL}/fapi/v1/ticker/24hr" | \
    jq -r '[.[] | select(.symbol | endswith("USDT"))] | sort_by(-(.quoteVolume | tonumber)) | .[0:'$NUM_SYMBOLS'] | .[].symbol' | \
    tr '\n' ' ')

echo "Symbols: $SYMBOLS"

# Create output directory if needed
mkdir -p "$(dirname "$OUTPUT_FILE")"

# Write CSV header
echo "timestamp,symbol,funding_rate,price,volume_24h,spread,open_interest" > "$OUTPUT_FILE"

# Fetch data for each symbol
for SYMBOL in $SYMBOLS; do
    echo "Fetching data for $SYMBOL..."

    # Get current funding rate
    FUNDING=$(curl -s "${FUTURES_URL}/fapi/v1/premiumIndex?symbol=$SYMBOL")
    FUNDING_RATE=$(echo "$FUNDING" | jq -r '.lastFundingRate')
    MARK_PRICE=$(echo "$FUNDING" | jq -r '.markPrice')

    # Get 24h ticker data
    TICKER=$(curl -s "${FUTURES_URL}/fapi/v1/ticker/24hr?symbol=$SYMBOL")
    VOLUME=$(echo "$TICKER" | jq -r '.quoteVolume')
    HIGH=$(echo "$TICKER" | jq -r '.highPrice')
    LOW=$(echo "$TICKER" | jq -r '.lowPrice')
    PRICE=$(echo "$TICKER" | jq -r '.lastPrice')

    # Calculate spread estimate (high-low as percentage of price)
    SPREAD=$(echo "scale=8; ($HIGH - $LOW) / $PRICE" | bc 2>/dev/null || echo "0.001")

    # Get open interest
    OI=$(curl -s "${FUTURES_URL}/fapi/v1/openInterest?symbol=$SYMBOL" | jq -r '.openInterest')
    OI_VALUE=$(echo "scale=0; $OI * $PRICE" | bc 2>/dev/null || echo "0")

    # Current timestamp
    TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

    # Write row
    echo "$TIMESTAMP,$SYMBOL,$FUNDING_RATE,$PRICE,$VOLUME,$SPREAD,$OI_VALUE" >> "$OUTPUT_FILE"

    # Rate limiting
    sleep 0.2
done

echo ""
echo "Data written to: $OUTPUT_FILE"
echo "Rows: $(wc -l < "$OUTPUT_FILE")"
