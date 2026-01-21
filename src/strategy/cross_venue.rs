//! Cross-venue funding rate comparison scanner.
//!
//! Compares funding rates between any two venues to identify
//! perp-perp arbitrage opportunities.

use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, info, instrument};

use crate::exchange::{FundingDataProvider, Venue, VenueAsset};

/// Configuration for cross-venue scanning filters.
#[derive(Debug, Clone)]
pub struct CrossVenueConfig {
    /// Minimum spread (8h) to include in results
    pub min_spread_8h: Decimal,
    /// Minimum 24h volume in USD (applied to both venues)
    pub min_volume_24h: Decimal,
    /// Maximum bid-ask spread (if available)
    pub max_bid_ask_spread: Decimal,
    /// Minimum absolute funding rate on either venue
    pub min_funding_rate: Decimal,
}

impl Default for CrossVenueConfig {
    fn default() -> Self {
        Self {
            min_spread_8h: dec!(0.001),      // 0.1% minimum spread
            min_volume_24h: dec!(10_000_000), // $10M minimum volume
            max_bid_ask_spread: dec!(0.001),  // 0.1% max spread
            min_funding_rate: dec!(0.0001),   // 0.01% minimum funding
        }
    }
}

/// Cross-venue funding spread opportunity.
#[derive(Debug, Clone)]
pub struct CrossVenueOpportunity {
    /// Base asset (e.g., "BTC")
    pub base_asset: String,
    /// Venue A data
    pub venue_a: Venue,
    pub venue_a_symbol: String,
    pub venue_a_funding_8h: Decimal,
    pub venue_a_volume: Decimal,
    /// Venue B data
    pub venue_b: Venue,
    pub venue_b_symbol: String,
    pub venue_b_funding_8h: Decimal,
    pub venue_b_volume: Decimal,
    /// Calculated metrics
    pub spread_8h: Decimal,
    pub spread_annualized: Decimal,
    /// Direction: true = long venue_a / short venue_b
    pub long_venue_a: bool,
}

impl CrossVenueOpportunity {
    /// Get the venue to go long on.
    pub fn long_venue(&self) -> Venue {
        if self.long_venue_a {
            self.venue_a
        } else {
            self.venue_b
        }
    }

    /// Get the venue to go short on.
    pub fn short_venue(&self) -> Venue {
        if self.long_venue_a {
            self.venue_b
        } else {
            self.venue_a
        }
    }
}

/// Scanner for cross-venue funding rate arbitrage opportunities.
#[derive(Debug, Clone)]
pub struct CrossVenueScanner {
    config: CrossVenueConfig,
}

impl CrossVenueScanner {
    /// Create a new cross-venue scanner with default config.
    pub fn new(min_spread_8h: Decimal) -> Self {
        Self {
            config: CrossVenueConfig {
                min_spread_8h,
                ..Default::default()
            },
        }
    }

    /// Create a scanner with full configuration.
    pub fn with_config(config: CrossVenueConfig) -> Self {
        Self { config }
    }

    /// Scan for cross-venue funding rate arbitrage opportunities.
    ///
    /// Fetches funding rates from both venues in parallel,
    /// applies filters, and returns opportunities sorted by spread.
    #[instrument(skip(self, venue_a, venue_b), name = "cross_venue_scan")]
    pub async fn scan<A, B>(
        &self,
        venue_a: &A,
        venue_b: &B,
    ) -> Result<Vec<CrossVenueOpportunity>>
    where
        A: FundingDataProvider,
        B: FundingDataProvider,
    {
        // Fetch data from both venues in parallel
        let (assets_a, assets_b) = tokio::try_join!(
            venue_a.get_venue_assets(),
            venue_b.get_venue_assets(),
        )?;

        debug!(
            "{} assets: {}, {} assets: {}",
            venue_a.venue(),
            assets_a.len(),
            venue_b.venue(),
            assets_b.len()
        );

        // Build lookup map for venue B by base asset
        let venue_b_map: HashMap<String, VenueAsset> = assets_b
            .into_iter()
            .map(|a| (a.base_asset.clone(), a))
            .collect();

        let mut opportunities = Vec::new();

        for asset_a in assets_a {
            // Find matching asset in venue B
            let Some(asset_b) = venue_b_map.get(&asset_a.base_asset) else {
                continue;
            };

            // Apply volume filter (both venues must meet threshold)
            if asset_a.volume_24h_usd < self.config.min_volume_24h
                || asset_b.volume_24h_usd < self.config.min_volume_24h
            {
                continue;
            }

            // Apply bid-ask spread filter (if available)
            if let Some(spread_a) = asset_a.spread {
                if spread_a > self.config.max_bid_ask_spread {
                    continue;
                }
            }
            if let Some(spread_b) = asset_b.spread {
                if spread_b > self.config.max_bid_ask_spread {
                    continue;
                }
            }

            // Apply minimum funding rate filter
            let max_funding = asset_a.funding_rate_8h.abs().max(asset_b.funding_rate_8h.abs());
            if max_funding < self.config.min_funding_rate {
                continue;
            }

            // Calculate spread
            let spread = asset_a.funding_rate_8h - asset_b.funding_rate_8h;
            let abs_spread = spread.abs();

            if abs_spread < self.config.min_spread_8h {
                continue;
            }

            // Annualize: 8h rate * 3 periods/day * 365 days
            let spread_annualized = spread.abs() * dec!(3) * dec!(365);

            // Determine direction: go long on lower funding, short on higher
            let long_venue_a = asset_a.funding_rate_8h < asset_b.funding_rate_8h;

            opportunities.push(CrossVenueOpportunity {
                base_asset: asset_a.base_asset,
                venue_a: venue_a.venue(),
                venue_a_symbol: asset_a.symbol,
                venue_a_funding_8h: asset_a.funding_rate_8h,
                venue_a_volume: asset_a.volume_24h_usd,
                venue_b: venue_b.venue(),
                venue_b_symbol: asset_b.symbol.clone(),
                venue_b_funding_8h: asset_b.funding_rate_8h,
                venue_b_volume: asset_b.volume_24h_usd,
                spread_8h: spread,
                spread_annualized,
                long_venue_a,
            });
        }

        // Sort by absolute spread (highest first)
        opportunities.sort_by(|a, b| b.spread_8h.abs().cmp(&a.spread_8h.abs()));

        info!(
            "Found {} cross-venue opportunities ({} vs {}) above {:.2}% spread",
            opportunities.len(),
            venue_a.venue(),
            venue_b.venue(),
            self.config.min_spread_8h * Decimal::from(100)
        );

        Ok(opportunities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires network access
    async fn test_cross_venue_scan() {
        use crate::exchange::{BinanceClient, HyperliquidClient};

        let binance = BinanceClient::new(&Default::default()).unwrap();
        let hyperliquid = HyperliquidClient::new().unwrap();

        let config = CrossVenueConfig {
            min_spread_8h: dec!(0.0001), // 0.01% minimum spread for testing
            min_volume_24h: dec!(10_000_000),
            ..Default::default()
        };
        let scanner = CrossVenueScanner::with_config(config);

        let opportunities = scanner.scan(&hyperliquid, &binance).await.unwrap();

        println!("Found {} cross-venue opportunities", opportunities.len());
        for opp in opportunities.iter().take(10) {
            println!(
                "{}: {} {:.4}% vs {} {:.4}% = spread {:.4}% ({:.1}% APY) -> Long {} / Short {}",
                opp.base_asset,
                opp.venue_a.short_code(),
                opp.venue_a_funding_8h * Decimal::from(100),
                opp.venue_b.short_code(),
                opp.venue_b_funding_8h * Decimal::from(100),
                opp.spread_8h * Decimal::from(100),
                opp.spread_annualized * Decimal::from(100),
                opp.long_venue().short_code(),
                opp.short_venue().short_code(),
            );
        }
    }
}
