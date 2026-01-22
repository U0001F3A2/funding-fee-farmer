# Funding Fee Farmer

High-performance delta-neutral funding fee farming on Binance Futures. -- pretty much not profitable, so use at your own risk.

## Overview

This system captures funding rate payments in perpetual futures by maintaining market-neutral positions. When funding rates are positive, shorts pay longs; when negative, longs pay shorts. By holding offsetting positions (spot + futures), we capture funding payments while eliminating directional risk.

## Key Features

- **Maximum Capital Utilization**: Target >80% capital deployment via cross-margin and multi-position allocation
- **Risk Management**: <5% maximum drawdown with automated liquidation prevention
- **Pair Selection**: Volume >$100M, spread <0.02%, funding rate >0.01%
- **Low Latency**: Rust-based execution for sub-millisecond order placement

## Quick Start

```bash
# Clone and build
git clone https://github.com/yourusername/funding-fee-farmer
cd funding-fee-farmer
cargo build --release

# Configure (copy and edit)
cp .env.example .env
# Edit .env with your Binance API keys

# Run (start with testnet!)
cargo run --release
```

## Configuration

See `.env.example` for all configuration options. Key parameters:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `MAX_UTILIZATION` | 0.85 | Maximum capital deployment |
| `MAX_DRAWDOWN` | 0.05 | Stop trading if exceeded |
| `MIN_VOLUME_24H` | $100M | Minimum pair liquidity |
| `DEFAULT_LEVERAGE` | 5x | Position leverage |

## Architecture

```
src/
├── config/     # Configuration management
├── exchange/   # Binance API (REST + WebSocket)
├── strategy/   # Scanner, allocator, executor
├── risk/       # Margin monitor, liquidation guard, MDD tracker
└── utils/      # Decimal arithmetic utilities
```

## Documentation

- [Design Document](docs/DESIGN.md) - Detailed strategy and architecture
- [CLAUDE.md](CLAUDE.md) - Development guide

## Safety

- **Start with testnet** - Set `TESTNET=true` until comfortable
- **No withdrawal permissions** - API keys should only have trading rights
- **IP whitelist** - Restrict API access to your IPs
- **Small positions first** - Test with minimum sizes before scaling

## License

MIT

## Disclaimer

This software is for educational purposes. Trading cryptocurrency derivatives involves substantial risk of loss. Past performance does not guarantee future results.
