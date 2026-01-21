//! Hyperliquid REST API client.
//!
//! Provides read-only access to Hyperliquid perpetuals market data:
//! - Funding rates (hourly)
//! - Asset prices (mark, oracle, mid)
//! - Open interest and volume
//! - Historical funding data

use anyhow::{Context, Result};
use reqwest::Client;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, instrument};

use super::types::*;

/// Base URL for Hyperliquid mainnet API.
const MAINNET_API_URL: &str = "https://api.hyperliquid.xyz";

/// Hyperliquid API client for fetching market data.
#[derive(Debug, Clone)]
pub struct HyperliquidClient {
    client: Client,
    base_url: String,
}

impl HyperliquidClient {
    /// Create a new Hyperliquid client for mainnet.
    pub fn new() -> Result<Self> {
        Self::with_base_url(MAINNET_API_URL)
    }

    /// Create a new Hyperliquid client with a custom base URL.
    pub fn with_base_url(base_url: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.to_string(),
        })
    }

    /// Get metadata and asset contexts for all perpetuals.
    /// Returns funding rates, prices, open interest, and volume.
    #[instrument(skip(self), name = "hl_meta_and_asset_ctxs")]
    pub async fn get_meta_and_asset_ctxs(&self) -> Result<(Meta, Vec<AssetCtx>)> {
        let url = format!("{}/info", self.base_url);
        let request = InfoRequest::MetaAndAssetCtxs;

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send metaAndAssetCtxs request")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Hyperliquid API error {}: {}", status, body);
        }

        let data: MetaAndAssetCtxsResponse = response
            .json()
            .await
            .context("Failed to parse metaAndAssetCtxs response")?;

        debug!(
            "Fetched {} assets from Hyperliquid",
            data.0.universe.len()
        );

        Ok(data)
    }

    /// Get all assets with their current market data.
    /// Combines metadata with asset contexts into a convenient format.
    #[instrument(skip(self), name = "hl_get_assets")]
    pub async fn get_assets(&self) -> Result<Vec<HyperliquidAsset>> {
        let (meta, ctxs) = self.get_meta_and_asset_ctxs().await?;

        if meta.universe.len() != ctxs.len() {
            anyhow::bail!(
                "Mismatch between universe ({}) and contexts ({})",
                meta.universe.len(),
                ctxs.len()
            );
        }

        let assets: Vec<HyperliquidAsset> = meta
            .universe
            .into_iter()
            .zip(ctxs.into_iter())
            .map(|(m, c)| HyperliquidAsset {
                name: m.name,
                sz_decimals: m.sz_decimals,
                max_leverage: m.max_leverage,
                funding_rate: c.funding,
                open_interest: c.open_interest,
                oracle_price: c.oracle_px,
                mark_price: c.mark_px,
                volume_24h: c.day_ntl_vlm,
                premium: c.premium.unwrap_or_default(), // Default to 0 for inactive coins
            })
            .collect();

        info!("Fetched {} Hyperliquid perpetual assets", assets.len());
        Ok(assets)
    }

    /// Get funding rates for all assets.
    /// Returns a map of coin name -> hourly funding rate.
    #[instrument(skip(self), name = "hl_get_funding_rates")]
    pub async fn get_funding_rates(&self) -> Result<HashMap<String, Decimal>> {
        let assets = self.get_assets().await?;

        let rates: HashMap<String, Decimal> = assets
            .into_iter()
            .map(|a| (a.name, a.funding_rate))
            .collect();

        debug!("Fetched {} funding rates from Hyperliquid", rates.len());
        Ok(rates)
    }

    /// Get funding history for a specific coin.
    #[instrument(skip(self), name = "hl_get_funding_history")]
    pub async fn get_funding_history(
        &self,
        coin: &str,
        start_time: i64,
        end_time: Option<i64>,
    ) -> Result<Vec<FundingHistoryRecord>> {
        let url = format!("{}/info", self.base_url);
        let request = InfoRequest::FundingHistory {
            coin: coin.to_string(),
            start_time,
            end_time,
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send fundingHistory request")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Hyperliquid API error {}: {}", status, body);
        }

        let records: Vec<FundingHistoryRecord> = response
            .json()
            .await
            .context("Failed to parse fundingHistory response")?;

        debug!(
            "Fetched {} funding history records for {}",
            records.len(),
            coin
        );
        Ok(records)
    }

    /// Calculate funding spread between Hyperliquid and another venue.
    ///
    /// # Arguments
    /// * `hl_assets` - Hyperliquid asset data
    /// * `other_funding` - Map of symbol (e.g., "BTCUSDT") -> 8h funding rate from other venue
    /// * `min_spread_8h` - Minimum absolute spread to consider (e.g., 0.0001 = 0.01%)
    ///
    /// # Returns
    /// Vector of funding spreads sorted by absolute spread (highest first).
    pub fn calculate_funding_spreads(
        hl_assets: &[HyperliquidAsset],
        other_funding: &HashMap<String, Decimal>,
        min_spread_8h: Decimal,
    ) -> Vec<FundingSpread> {
        let mut spreads = Vec::new();

        for asset in hl_assets {
            // Convert HL coin name to Binance-style symbol (e.g., "BTC" -> "BTCUSDT")
            let binance_symbol = format!("{}USDT", asset.name);

            if let Some(&other_rate) = other_funding.get(&binance_symbol) {
                // HL funding is hourly, convert to 8h equivalent
                let hl_8h = asset.funding_rate * dec!(8);

                // Spread = HL rate - Other rate
                // Positive spread: HL pays more to shorts (or charges more to longs)
                // Negative spread: Other venue pays more to shorts
                let spread = hl_8h - other_rate;
                let abs_spread = spread.abs();

                if abs_spread >= min_spread_8h {
                    // Annualize: 8h rate * 3 periods/day * 365 days
                    let spread_annualized = spread * dec!(3) * dec!(365);

                    // Determine recommended direction
                    let direction = if spread > Decimal::ZERO {
                        // HL has higher funding -> short HL, long other
                        Some(SpreadDirection::ShortHlLongOther)
                    } else if spread < Decimal::ZERO {
                        // Other has higher funding -> long HL, short other
                        Some(SpreadDirection::LongHlShortOther)
                    } else {
                        None
                    };

                    spreads.push(FundingSpread {
                        symbol: binance_symbol,
                        hl_coin: asset.name.clone(),
                        hl_funding_hourly: asset.funding_rate,
                        hl_funding_8h: hl_8h,
                        other_funding_8h: other_rate,
                        spread_8h: spread,
                        spread_annualized,
                        recommended_direction: direction,
                    });
                }
            }
        }

        // Sort by absolute spread (highest first)
        spreads.sort_by(|a, b| b.spread_8h.abs().cmp(&a.spread_8h.abs()));

        spreads
    }
}

impl Default for HyperliquidClient {
    fn default() -> Self {
        Self::new().expect("Failed to create default HyperliquidClient")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_funding_spreads() {
        let hl_assets = vec![
            HyperliquidAsset {
                name: "BTC".to_string(),
                sz_decimals: 4,
                max_leverage: 50,
                funding_rate: dec!(0.0001),  // 0.01% hourly = 0.08% per 8h
                open_interest: dec!(1000000),
                oracle_price: dec!(50000),
                mark_price: dec!(50010),
                volume_24h: dec!(100000000),
                premium: dec!(0.0002),
            },
            HyperliquidAsset {
                name: "ETH".to_string(),
                sz_decimals: 3,
                max_leverage: 50,
                funding_rate: dec!(-0.0005), // -0.05% hourly = -0.4% per 8h
                open_interest: dec!(500000),
                oracle_price: dec!(3000),
                mark_price: dec!(2995),
                volume_24h: dec!(50000000),
                premium: dec!(-0.0017),
            },
        ];

        let mut other_funding = HashMap::new();
        other_funding.insert("BTCUSDT".to_string(), dec!(0.0005)); // 0.05% per 8h
        other_funding.insert("ETHUSDT".to_string(), dec!(-0.001)); // -0.1% per 8h

        let spreads = HyperliquidClient::calculate_funding_spreads(
            &hl_assets,
            &other_funding,
            dec!(0.0001), // 0.01% minimum
        );

        assert_eq!(spreads.len(), 2);

        // ETH should have larger spread (0.4% vs 0.1% = 0.3% spread)
        assert_eq!(spreads[0].hl_coin, "ETH");
        // ETH: HL -0.4%, Other -0.1%, spread = -0.4% - (-0.1%) = -0.3%
        assert_eq!(spreads[0].spread_8h, dec!(-0.003)); // -0.3%
        assert_eq!(
            spreads[0].recommended_direction,
            Some(SpreadDirection::LongHlShortOther)
        );

        // BTC: HL 0.08%, Other 0.05%, spread = 0.08% - 0.05% = 0.03%
        assert_eq!(spreads[1].hl_coin, "BTC");
        assert_eq!(spreads[1].spread_8h, dec!(0.0003)); // 0.03%
    }

    #[tokio::test]
    #[ignore] // Requires network access
    async fn test_live_fetch() {
        let client = HyperliquidClient::new().unwrap();
        let assets = client.get_assets().await.unwrap();

        assert!(!assets.is_empty());
        println!("Fetched {} assets", assets.len());

        // Check BTC exists
        let btc = assets.iter().find(|a| a.name == "BTC");
        assert!(btc.is_some());

        let btc = btc.unwrap();
        println!("BTC funding rate (hourly): {}%", btc.funding_rate * dec!(100));
        println!("BTC mark price: ${}", btc.mark_price);
    }
}
