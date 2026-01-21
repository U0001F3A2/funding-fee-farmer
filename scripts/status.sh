#!/bin/bash
# Funding Fee Farmer - Status Check Script
# Usage: ./scripts/status.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
EC2_HOST_FILE="$PROJECT_DIR/.ec2_host"
SSH_KEY="$HOME/.ssh/fff-key.pem"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color
BOLD='\033[1m'

# Check prerequisites
if [[ ! -f "$EC2_HOST_FILE" ]]; then
    echo -e "${RED}Error: .ec2_host file not found${NC}"
    exit 1
fi

if [[ ! -f "$SSH_KEY" ]]; then
    echo -e "${RED}Error: SSH key not found at $SSH_KEY${NC}"
    exit 1
fi

EC2_HOST=$(cat "$EC2_HOST_FILE")

echo -e "${BOLD}${CYAN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}${CYAN}       FUNDING FEE FARMER - STATUS CHECK${NC}"
echo -e "${BOLD}${CYAN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}Host:${NC} $EC2_HOST"
echo -e "${BLUE}Time:${NC} $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
echo ""

# Function to run SSH command
ssh_cmd() {
    ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no -o ConnectTimeout=10 ec2-user@"$EC2_HOST" "$1" 2>/dev/null
}

# Check service status
echo -e "${BOLD}${YELLOW}[1/6] Service Status${NC}"
SERVICE_STATUS=$(ssh_cmd "sudo systemctl is-active funding-fee-farmer" || echo "inactive")
if [[ "$SERVICE_STATUS" == "active" ]]; then
    echo -e "  Status: ${GREEN}● Running${NC}"
    UPTIME=$(ssh_cmd "sudo systemctl show funding-fee-farmer --property=ActiveEnterTimestamp | cut -d= -f2")
    echo -e "  Since:  $UPTIME"
else
    echo -e "  Status: ${RED}● Stopped${NC}"
    echo -e "${RED}Service is not running. Use 'sudo systemctl start funding-fee-farmer' to start.${NC}"
    exit 1
fi
echo ""

# Get latest status report
echo -e "${BOLD}${YELLOW}[2/6] Latest Status Report${NC}"
ssh_cmd "sudo journalctl -u funding-fee-farmer --no-pager -n 500 | grep -A 25 'STATUS REPORT' | tail -28" | while read line; do
    # Colorize based on content
    if [[ "$line" == *"STATUS REPORT"* ]]; then
        echo -e "  ${BOLD}${CYAN}$line${NC}"
    elif [[ "$line" == *"Funding Received"* ]]; then
        echo -e "  ${GREEN}$line${NC}"
    elif [[ "$line" == *"Trading Fees"* ]] || [[ "$line" == *"Borrow Interest"* ]]; then
        echo -e "  ${RED}$line${NC}"
    elif [[ "$line" == *"Realized PnL"* ]]; then
        if [[ "$line" == *"-$"* ]]; then
            echo -e "  ${RED}${BOLD}$line${NC}"
        else
            echo -e "  ${GREEN}${BOLD}$line${NC}"
        fi
    else
        echo -e "  $line"
    fi
done
echo ""

# Current positions
echo -e "${BOLD}${YELLOW}[3/6] Current Positions${NC}"
ssh_cmd "sudo journalctl -u funding-fee-farmer --no-pager -n 100 | grep 'POSITIONS.*current_positions' | tail -1" | sed 's/.*\[POSITIONS\]/  /'
echo ""

# JIT Entry Status
echo -e "${BOLD}${YELLOW}[4/6] JIT Entry Activity (last 10)${NC}"
JIT_LINES=$(ssh_cmd "sudo journalctl -u funding-fee-farmer --no-pager -n 1000 | grep -E 'JIT|ready to enter' | tail -10")
if [[ -n "$JIT_LINES" ]]; then
    echo "$JIT_LINES" | while read line; do
        if [[ "$line" == *"waiting 0 min"* ]] || [[ "$line" == *"ready to enter"* ]]; then
            echo -e "  ${GREEN}$line${NC}"
        elif [[ "$line" == *"waiting"* ]]; then
            echo -e "  ${YELLOW}$line${NC}"
        else
            echo -e "  $line"
        fi
    done
else
    echo -e "  ${BLUE}No recent JIT activity${NC}"
fi
echo ""

# Protection & Warnings
echo -e "${BOLD}${YELLOW}[5/6] Position Protection (last 5)${NC}"
PROTECT_LINES=$(ssh_cmd "sudo journalctl -u funding-fee-farmer --no-pager -n 500 | grep 'PROTECT' | tail -5")
if [[ -n "$PROTECT_LINES" ]]; then
    echo "$PROTECT_LINES" | while read line; do
        echo -e "  ${CYAN}$line${NC}"
    done
else
    echo -e "  ${BLUE}No protection events${NC}"
fi
echo ""

# Recent errors/warnings
echo -e "${BOLD}${YELLOW}[6/6] Recent Warnings/Errors${NC}"
WARN_LINES=$(ssh_cmd "sudo journalctl -u funding-fee-farmer --no-pager -n 500 | grep -E 'WARN|ERROR' | grep -v 'drawdown recorded' | tail -5")
if [[ -n "$WARN_LINES" ]]; then
    echo "$WARN_LINES" | while read line; do
        if [[ "$line" == *"ERROR"* ]]; then
            echo -e "  ${RED}$line${NC}"
        else
            echo -e "  ${YELLOW}$line${NC}"
        fi
    done
else
    echo -e "  ${GREEN}No recent warnings or errors${NC}"
fi
echo ""

echo -e "${BOLD}${CYAN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}Tip: Run 'journalctl -u funding-fee-farmer -f' on EC2 for live logs${NC}"
