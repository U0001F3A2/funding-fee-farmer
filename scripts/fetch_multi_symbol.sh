#!/bin/bash
# Fetch historical funding data for multiple symbols and combine into one CSV
#
# Usage:
#   ./scripts/fetch_multi_symbol.sh START_DATE END_DATE OUTPUT_FILE [SYMBOLS...]
#
# Example (fetch top 5 by volume):
#   ./scripts/fetch_multi_symbol.sh 2024-01-01 2024-06-01 data/funding_data.csv
#
# Example (specific symbols):
#   ./scripts/fetch_multi_symbol.sh 2024-01-01 2024-06-01 data/funding_data.csv BTCUSDT ETHUSDT SOLUSDT

set -e

START_DATE="${1:?Usage: $0 START_DATE END_DATE OUTPUT_FILE [SYMBOLS...]}"
END_DATE="${2:?Usage: $0 START_DATE END_DATE OUTPUT_FILE [SYMBOLS...]}"
OUTPUT_FILE="${3:?Usage: $0 START_DATE END_DATE OUTPUT_FILE [SYMBOLS...]}"
shift 3

FUTURES_URL="https://fapi.binance.com"
SCRIPT_DIR="$(dirname "$0")"

# Get symbols (from args or top by volume)
if [ $# -gt 0 ]; then
    SYMBOLS=("$@")
else
    echo "Fetching top 10 futures symbols by volume..."
    SYMBOLS=($(curl -s "${FUTURES_URL}/fapi/v1/ticker/24hr" | \
        jq -r '[.[] | select(.symbol | endswith("USDT"))] | sort_by(-(.quoteVolume | tonumber)) | .[0:10] | .[].symbol'))
fi

echo "Symbols to fetch: ${SYMBOLS[*]}"
echo "Date range: $START_DATE to $END_DATE"
echo ""

# Create temp directory for individual symbol files
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

# Fetch each symbol
for SYMBOL in "${SYMBOLS[@]}"; do
    echo "========================================"
    echo "Fetching $SYMBOL..."
    echo "========================================"

    SYMBOL_FILE="$TEMP_DIR/${SYMBOL}.csv"

    # Use Python script if available (more robust), otherwise use bash script
    if command -v python3 &> /dev/null && [ -f "$SCRIPT_DIR/fetch_historical_data.py" ]; then
        python3 "$SCRIPT_DIR/fetch_historical_data.py" \
            --start "$START_DATE" \
            --end "$END_DATE" \
            --symbols "$SYMBOL" \
            --output "$SYMBOL_FILE"
    else
        "$SCRIPT_DIR/fetch_historical_funding.sh" "$SYMBOL" "$START_DATE" "$END_DATE" "$SYMBOL_FILE"
    fi

    echo ""
    sleep 2  # Rate limiting between symbols
done

# Combine all CSVs
echo "Combining data..."
mkdir -p "$(dirname "$OUTPUT_FILE")"

# Write header
echo "timestamp,symbol,funding_rate,price,volume_24h,spread,open_interest" > "$OUTPUT_FILE"

# Append data from each file (skip headers)
for SYMBOL in "${SYMBOLS[@]}"; do
    SYMBOL_FILE="$TEMP_DIR/${SYMBOL}.csv"
    if [ -f "$SYMBOL_FILE" ]; then
        tail -n +2 "$SYMBOL_FILE" >> "$OUTPUT_FILE"
    fi
done

# Sort by timestamp and symbol
SORTED_FILE="$TEMP_DIR/sorted.csv"
head -1 "$OUTPUT_FILE" > "$SORTED_FILE"
tail -n +2 "$OUTPUT_FILE" | sort -t',' -k1,1 -k2,2 >> "$SORTED_FILE"
mv "$SORTED_FILE" "$OUTPUT_FILE"

TOTAL_ROWS=$(tail -n +2 "$OUTPUT_FILE" | wc -l)
echo ""
echo "========================================"
echo "Complete!"
echo "Output: $OUTPUT_FILE"
echo "Total rows: $TOTAL_ROWS"
echo "========================================"
