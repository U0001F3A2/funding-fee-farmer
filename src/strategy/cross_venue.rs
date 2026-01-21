//! Cross-venue funding rate comparison scanner.
//!
//! Compares funding rates between Hyperliquid and Binance to identify
//! perp-perp arbitrage opportunities.

use anyhow::Result;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::{debug, info, instrument};

use crate::exchange::hyperliquid::{FundingSpread, HyperliquidClient};
use crate::exchange::BinanceClient;

/// Scanner for cross-venue funding rate arbitrage opportunities.
#[derive(Debug, Clone)]
pub struct CrossVenueScanner {
    /// Minimum spread (8h) to include in results
    min_spread_8h: Decimal,
}

impl CrossVenueScanner {
    /// Create a new cross-venue scanner.
    pub fn new(min_spread_8h: Decimal) -> Self {
        Self { min_spread_8h }
    }

    /// Scan for cross-venue funding rate arbitrage opportunities.
    ///
    /// Fetches funding rates from both Hyperliquid and Binance in parallel,
    /// then calculates spreads for matching symbols.
    ///
    /// Returns spreads sorted by absolute spread (highest first).
    #[instrument(skip(self, binance, hyperliquid), name = "cross_venue_scan")]
    pub async fn scan(
        &self,
        binance: &BinanceClient,
        hyperliquid: &HyperliquidClient,
    ) -> Result<Vec<FundingSpread>> {
        // Fetch data from both venues in parallel
        let (binance_rates, hl_assets) = tokio::try_join!(
            binance.get_funding_rates(),
            hyperliquid.get_assets(),
        )?;

        debug!(
            "Fetched {} Binance rates, {} Hyperliquid assets",
            binance_rates.len(),
            hl_assets.len()
        );

        // Convert Binance rates to HashMap<symbol, 8h_rate>
        let binance_8h: HashMap<String, Decimal> = binance_rates
            .into_iter()
            .map(|fr| (fr.symbol, fr.funding_rate))
            .collect();

        // Calculate spreads using existing HyperliquidClient method
        let spreads = HyperliquidClient::calculate_funding_spreads(
            &hl_assets,
            &binance_8h,
            self.min_spread_8h,
        );

        info!(
            "Found {} cross-venue spreads above {:.4}%",
            spreads.len(),
            self.min_spread_8h * Decimal::from(100)
        );

        Ok(spreads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires network access
    async fn test_cross_venue_scan() {
        let binance = BinanceClient::new(&Default::default()).unwrap();
        let hyperliquid = HyperliquidClient::new().unwrap();
        let scanner = CrossVenueScanner::new(Decimal::new(1, 4)); // 0.0001 = 0.01% minimum spread

        let spreads = scanner.scan(&binance, &hyperliquid).await.unwrap();

        println!("Found {} cross-venue spreads", spreads.len());
        for spread in spreads.iter().take(10) {
            println!(
                "{}: HL {:.4}% vs BN {:.4}% = spread {:.4}% ({:.1}% APY)",
                spread.hl_coin,
                spread.hl_funding_8h * Decimal::from(100),
                spread.other_funding_8h * Decimal::from(100),
                spread.spread_8h * Decimal::from(100),
                spread.spread_annualized * Decimal::from(100),
            );
        }
    }
}
