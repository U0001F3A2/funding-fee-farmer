//! Venue-agnostic traits for funding data providers.
//!
//! Provides a common interface for extracting funding rate and market data
//! from any perpetuals venue (Binance, Hyperliquid, etc.) for:
//! - Cross-venue arbitrage detection
//! - Unified opportunity scanning
//! - Multi-venue strategy execution

use async_trait::async_trait;
use rust_decimal::Decimal;
use std::fmt;

/// Normalized asset data from any venue.
///
/// Contains the minimum data needed to evaluate funding opportunities
/// across different exchanges with different APIs and data formats.
#[derive(Debug, Clone)]
pub struct VenueAsset {
    /// Symbol in venue's native format (e.g., "BTC" for HL, "BTCUSDT" for Binance)
    pub symbol: String,
    /// Normalized symbol for cross-venue matching (e.g., "BTC")
    pub base_asset: String,
    /// Funding rate normalized to 8-hour equivalent
    pub funding_rate_8h: Decimal,
    /// 24-hour trading volume in USD
    pub volume_24h_usd: Decimal,
    /// Bid-ask spread as a decimal (e.g., 0.0001 = 0.01%)
    pub spread: Option<Decimal>,
    /// Mark price in USD
    pub mark_price: Decimal,
    /// Open interest in USD (if available)
    pub open_interest_usd: Option<Decimal>,
}

impl VenueAsset {
    /// Create a new VenueAsset with required fields.
    pub fn new(
        symbol: String,
        base_asset: String,
        funding_rate_8h: Decimal,
        volume_24h_usd: Decimal,
        mark_price: Decimal,
    ) -> Self {
        Self {
            symbol,
            base_asset,
            funding_rate_8h,
            volume_24h_usd,
            spread: None,
            mark_price,
            open_interest_usd: None,
        }
    }

    /// Set the bid-ask spread.
    pub fn with_spread(mut self, spread: Decimal) -> Self {
        self.spread = Some(spread);
        self
    }

    /// Set the open interest.
    pub fn with_open_interest(mut self, oi: Decimal) -> Self {
        self.open_interest_usd = Some(oi);
        self
    }
}

/// Venue identifier for multi-venue operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Venue {
    Binance,
    Hyperliquid,
    // Future venues can be added here
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Venue::Binance => write!(f, "Binance"),
            Venue::Hyperliquid => write!(f, "Hyperliquid"),
        }
    }
}

impl Venue {
    /// Short code for display (2-3 chars).
    pub fn short_code(&self) -> &'static str {
        match self {
            Venue::Binance => "BN",
            Venue::Hyperliquid => "HL",
        }
    }
}

/// Trait for venues that provide funding rate data.
///
/// Implement this trait to add support for new perpetuals exchanges.
/// The trait normalizes data to a common format for cross-venue comparison.
#[async_trait]
pub trait FundingDataProvider: Send + Sync {
    /// Returns the venue identifier.
    fn venue(&self) -> Venue;

    /// Fetch all perpetual assets with their current funding rates.
    ///
    /// Returns normalized `VenueAsset` data with:
    /// - Funding rates converted to 8h equivalent
    /// - Volume in USD
    /// - Base asset extracted for cross-venue matching
    async fn get_venue_assets(&self) -> anyhow::Result<Vec<VenueAsset>>;

    /// Get the funding period in hours for this venue.
    ///
    /// - Binance: 8 hours
    /// - Hyperliquid: 1 hour
    fn funding_period_hours(&self) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_venue_asset_builder() {
        let asset = VenueAsset::new(
            "BTCUSDT".to_string(),
            "BTC".to_string(),
            dec!(0.0001),
            dec!(100_000_000),
            dec!(50000),
        )
        .with_spread(dec!(0.0001))
        .with_open_interest(dec!(500_000_000));

        assert_eq!(asset.base_asset, "BTC");
        assert_eq!(asset.spread, Some(dec!(0.0001)));
        assert_eq!(asset.open_interest_usd, Some(dec!(500_000_000)));
    }

    #[test]
    fn test_venue_display() {
        assert_eq!(Venue::Binance.to_string(), "Binance");
        assert_eq!(Venue::Hyperliquid.short_code(), "HL");
    }
}
