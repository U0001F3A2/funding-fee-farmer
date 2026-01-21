# CLAUDE.md - Project Intelligence

## Project Overview

**Funding Fee Farmer** is a high-performance Rust application for automated delta-neutral funding fee farming on Binance Futures. The system captures perpetual futures funding rate payments while maintaining market-neutral exposure.

## Quick Start

```bash
# Build
cargo build --release

# Run (requires .env configuration)
cargo run --release

# Run tests
cargo test

# Check formatting and lints
cargo fmt --check && cargo clippy
```

## Project Structure

```
funding-fee-farmer/
├── src/
│   ├── main.rs              # Application entry point
│   ├── lib.rs               # Library exports
│   ├── config/              # Configuration management
│   │   └── mod.rs
│   ├── exchange/            # Binance API integration
│   │   ├── mod.rs
│   │   ├── client.rs        # REST API client
│   │   ├── websocket.rs     # WebSocket streams
│   │   └── types.rs         # API data types
│   ├── strategy/            # Trading strategy logic
│   │   ├── mod.rs
│   │   ├── scanner.rs       # Market opportunity scanner
│   │   ├── allocator.rs     # Capital allocation
│   │   └── executor.rs      # Order execution
│   ├── risk/                # Risk management
│   │   ├── mod.rs
│   │   ├── margin.rs        # Margin monitoring
│   │   ├── liquidation.rs   # Liquidation prevention
│   │   └── mdd.rs           # Maximum drawdown tracking
│   └── utils/               # Shared utilities
│       ├── mod.rs
│       └── decimal.rs       # Precise decimal arithmetic
├── docs/
│   └── DESIGN.md            # Detailed design document
├── tests/                   # Integration tests
├── Cargo.toml
├── .env.example             # Environment template
└── CLAUDE.md                # This file
```

## Core Concepts

### Delta-Neutral Funding Farming

The strategy profits from funding rate payments without directional exposure:

- **Positive Funding**: Long spot + Short perpetual (shorts receive payment)
- **Negative Funding**: Short spot (margin) + Long perpetual (longs receive payment)

### Key Metrics

| Metric | Target | Description |
|--------|--------|-------------|
| Capital Utilization | >80% | Percentage of capital actively deployed |
| Maximum Drawdown | <5% | Largest peak-to-trough decline |
| Margin Ratio | >300% | Maintenance margin safety buffer |

### Pair Selection Criteria

Pairs must meet ALL conditions:
- 24h volume > $100M (liquidity)
- Funding rate > 0.01% or < -0.01% (profitability)
- Bid-ask spread < 0.02% (execution cost)
- Open interest > $50M (market depth)

## Architecture Decisions

### Why Rust?
- **Latency**: Sub-millisecond order execution critical for funding capture
- **Safety**: Memory safety for financial applications
- **Async**: Tokio runtime for concurrent WebSocket streams
- **Performance**: No GC pauses during critical operations

### Key Dependencies

```toml
tokio = "1.x"           # Async runtime
reqwest = "0.11"        # HTTP client
tokio-tungstenite = "0.x"  # WebSocket
serde = "1.x"           # Serialization
rust_decimal = "1.x"    # Precise decimal math
tracing = "0.1"         # Structured logging
```

## Configuration

### Environment Variables (.env)

```env
# Binance API Credentials
BINANCE_API_KEY=your_api_key
BINANCE_SECRET_KEY=your_secret_key

# Optional: Testnet
BINANCE_TESTNET=false

# Risk Parameters
MAX_CAPITAL_UTILIZATION=0.85
MAX_DRAWDOWN=0.05
MIN_MARGIN_RATIO=3.0
```

### Config File (config.toml)

See `docs/DESIGN.md` for full configuration reference.

## Development Guidelines

### Code Style

- Follow Rust standard formatting (`cargo fmt`)
- All public APIs must have documentation
- Use `Result<T, Error>` for fallible operations
- Prefer `rust_decimal::Decimal` over `f64` for financial math

### Error Handling

```rust
// Use thiserror for error types
#[derive(Debug, thiserror::Error)]
pub enum TradingError {
    #[error("Insufficient margin: {available} < {required}")]
    InsufficientMargin { available: Decimal, required: Decimal },

    #[error("API error: {0}")]
    ApiError(#[from] reqwest::Error),
}
```

### Testing

- Unit tests: `cargo test`
- Integration tests require testnet credentials
- Mock exchange responses for unit tests

## Common Tasks

### Adding a New Pair Filter

1. Edit `src/strategy/scanner.rs`
2. Add filter predicate to `PairScanner::filter_pairs()`
3. Update config schema if configurable
4. Add unit test

### Modifying Risk Parameters

1. Update `src/config/mod.rs` struct
2. Update `.env.example` if environment-based
3. Update `docs/DESIGN.md` documentation
4. Ensure `RiskManager` respects new parameter

### Debugging Position Issues

```bash
# Enable debug logging
RUST_LOG=debug cargo run

# Trace specific module
RUST_LOG=funding_fee_farmer::strategy=trace cargo run
```

## API Reference (Binance)

### Important Endpoints

| Endpoint | Purpose |
|----------|---------|
| `GET /fapi/v1/fundingRate` | Current funding rates |
| `GET /fapi/v1/ticker/24hr` | 24h volume data |
| `POST /fapi/v1/order` | Place futures order |
| `GET /fapi/v2/account` | Account/position info |

### Rate Limits

- REST API: 1200 requests/min (weight-based)
- WebSocket: 5 messages/sec outbound
- Order placement: 10 orders/sec

## Troubleshooting

### "Insufficient margin" errors
- Check margin ratio (target >300%)
- Reduce position size
- Add funds or reduce leverage

### WebSocket disconnections
- Implement exponential backoff reconnection
- Check network stability
- Verify API key permissions

### Missed funding payments
- Ensure positions opened BEFORE funding snapshot (within 15s of funding time)
- Verify both spot and futures positions are active

## Security Notes

- Never commit `.env` or API keys
- Use IP whitelisting on Binance API
- Enable withdrawal whitelist
- Start with testnet for development
- Use minimal permissions (no withdrawal rights)

## Related Documentation

- [Design Document](docs/DESIGN.md) - Detailed strategy and architecture
- [Binance Futures API](https://binance-docs.github.io/apidocs/futures/en/)
- [Binance Spot API](https://binance-docs.github.io/apidocs/spot/en/)
