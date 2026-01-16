#!/bin/bash
# Fetch historical funding rate data from Binance for backtesting
#
# Usage:
#   ./scripts/fetch_historical_funding.sh SYMBOL START_DATE END_DATE [OUTPUT_FILE]
#
# Example:
#   ./scripts/fetch_historical_funding.sh BTCUSDT 2024-01-01 2024-06-01 data/btc_funding.csv
#   ./scripts/fetch_historical_funding.sh ETHUSDT 2024-01-01 2024-06-01 data/eth_funding.csv
#
# Then combine CSVs:
#   cat data/btc_funding.csv > data/combined.csv
#   tail -n +2 data/eth_funding.csv >> data/combined.csv

set -e

SYMBOL="${1:?Usage: $0 SYMBOL START_DATE END_DATE [OUTPUT_FILE]}"
START_DATE="${2:?Usage: $0 SYMBOL START_DATE END_DATE [OUTPUT_FILE]}"
END_DATE="${3:?Usage: $0 SYMBOL START_DATE END_DATE [OUTPUT_FILE]}"
OUTPUT_FILE="${4:-data/${SYMBOL}_funding.csv}"

FUTURES_URL="https://fapi.binance.com"

# Convert dates to milliseconds
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS
    START_MS=$(date -j -f "%Y-%m-%d" "$START_DATE" "+%s000" 2>/dev/null || echo "")
    END_MS=$(date -j -f "%Y-%m-%d" "$END_DATE" "+%s000" 2>/dev/null || echo "")
else
    # Linux
    START_MS=$(date -d "$START_DATE" "+%s000" 2>/dev/null || echo "")
    END_MS=$(date -d "$END_DATE" "+%s000" 2>/dev/null || echo "")
fi

if [ -z "$START_MS" ] || [ -z "$END_MS" ]; then
    echo "Error: Could not parse dates. Use format YYYY-MM-DD"
    exit 1
fi

echo "Fetching funding rates for $SYMBOL from $START_DATE to $END_DATE..."
echo "Start timestamp: $START_MS"
echo "End timestamp: $END_MS"

# Create output directory
mkdir -p "$(dirname "$OUTPUT_FILE")"

# Write CSV header
echo "timestamp,symbol,funding_rate,price,volume_24h,spread,open_interest" > "$OUTPUT_FILE"

# Fetch funding rates in batches
CURRENT_START=$START_MS
TOTAL_ROWS=0

while [ "$CURRENT_START" -lt "$END_MS" ]; do
    echo "  Fetching batch starting at $(date -d @$((CURRENT_START/1000)) 2>/dev/null || date -r $((CURRENT_START/1000)))..."

    # Fetch funding rates
    FUNDING_DATA=$(curl -s "${FUTURES_URL}/fapi/v1/fundingRate?symbol=${SYMBOL}&startTime=${CURRENT_START}&endTime=${END_MS}&limit=1000")

    if [ -z "$FUNDING_DATA" ] || [ "$FUNDING_DATA" = "[]" ]; then
        echo "  No more data"
        break
    fi

    # Parse each funding entry
    LAST_TIME=$CURRENT_START
    BATCH_COUNT=0

    echo "$FUNDING_DATA" | jq -c '.[]' | while read -r entry; do
        FUNDING_TIME=$(echo "$entry" | jq -r '.fundingTime')
        FUNDING_RATE=$(echo "$entry" | jq -r '.fundingRate')

        # Convert timestamp to ISO format
        if [[ "$OSTYPE" == "darwin"* ]]; then
            TIMESTAMP=$(date -r $((FUNDING_TIME/1000)) -u +"%Y-%m-%dT%H:%M:%SZ")
        else
            TIMESTAMP=$(date -d @$((FUNDING_TIME/1000)) -u +"%Y-%m-%dT%H:%M:%SZ")
        fi

        # Fetch kline data for this timestamp to get price/volume
        # Use 8h kline that contains this funding time
        KLINE_START=$((FUNDING_TIME - 28800000))  # 8 hours before
        KLINE=$(curl -s "${FUTURES_URL}/fapi/v1/klines?symbol=${SYMBOL}&interval=8h&startTime=${KLINE_START}&limit=1" 2>/dev/null || echo "[]")

        if [ "$KLINE" != "[]" ] && [ -n "$KLINE" ]; then
            PRICE=$(echo "$KLINE" | jq -r '.[0][4]' 2>/dev/null || echo "0")
            HIGH=$(echo "$KLINE" | jq -r '.[0][2]' 2>/dev/null || echo "0")
            LOW=$(echo "$KLINE" | jq -r '.[0][3]' 2>/dev/null || echo "0")
            VOLUME=$(echo "$KLINE" | jq -r '.[0][7]' 2>/dev/null || echo "0")
            VOLUME_24H=$(echo "scale=0; $VOLUME * 3" | bc 2>/dev/null || echo "$VOLUME")

            # Calculate spread
            if [ "$PRICE" != "0" ] && [ -n "$PRICE" ]; then
                SPREAD=$(echo "scale=8; ($HIGH - $LOW) / $PRICE" | bc 2>/dev/null || echo "0.001")
            else
                SPREAD="0.001"
            fi
        else
            PRICE="0"
            VOLUME_24H="0"
            SPREAD="0.001"
        fi

        # Estimate open interest (would need separate API call per timestamp)
        OI="0"

        echo "$TIMESTAMP,$SYMBOL,$FUNDING_RATE,$PRICE,$VOLUME_24H,$SPREAD,$OI" >> "$OUTPUT_FILE"

        # Rate limiting
        sleep 0.15
    done

    # Get last funding time from batch
    LAST_FUNDING_TIME=$(echo "$FUNDING_DATA" | jq -r '.[-1].fundingTime')
    if [ "$LAST_FUNDING_TIME" = "null" ] || [ "$LAST_FUNDING_TIME" -le "$CURRENT_START" ]; then
        break
    fi
    CURRENT_START=$((LAST_FUNDING_TIME + 1))

    # Rate limiting between batches
    sleep 1
done

TOTAL_ROWS=$(tail -n +2 "$OUTPUT_FILE" | wc -l)
echo ""
echo "Done! Fetched $TOTAL_ROWS funding rate entries"
echo "Output: $OUTPUT_FILE"
