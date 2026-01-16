//! Market scanner for identifying funding rate opportunities.

use crate::config::PairSelectionConfig;
use crate::exchange::{BinanceClient, FundingRate, MarginAsset, QualifiedPair, SpotSymbolInfo};
use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, info, instrument, warn};

/// Scans the market for profitable funding rate opportunities.
pub struct MarketScanner {
    config: PairSelectionConfig,
}

impl MarketScanner {
    /// Create a new market scanner with the given configuration.
    pub fn new(config: PairSelectionConfig) -> Self {
        Self { config }
    }

    /// Scan the market and return qualified pairs sorted by score.
    /// Only returns pairs that have spot margin trading enabled for hedging.
    #[instrument(skip(self, client))]
    pub async fn scan(&self, client: &BinanceClient) -> Result<Vec<QualifiedPair>> {
        // Fetch public data in parallel (required)
        let (funding_rates, tickers, book_tickers, spot_info) = tokio::try_join!(
            client.get_funding_rates(),
            client.get_24h_tickers(),
            client.get_book_tickers(),
            client.get_spot_exchange_info(),
        )?;

        // Fetch margin assets separately (requires auth, may fail in read-only mode)
        let margin_assets = match client.get_margin_all_assets().await {
            Ok(assets) => assets,
            Err(e) => {
                warn!("Failed to fetch margin assets (may need API key): {}. Using empty list.", e);
                Vec::new()
            }
        };

        info!(
            funding_count = funding_rates.len(),
            ticker_count = tickers.len(),
            spot_symbols = spot_info.len(),
            margin_assets = margin_assets.len(),
            "Fetched market data"
        );

        // Index data by symbol for efficient lookup
        let volume_map: HashMap<String, Decimal> = tickers
            .iter()
            .map(|t| (t.symbol.clone(), t.quote_volume))
            .collect();

        let spread_map: HashMap<String, Decimal> = book_tickers
            .iter()
            .filter_map(|b| {
                let mid = (b.bid_price + b.ask_price) / dec!(2);
                if mid > Decimal::ZERO {
                    Some((b.symbol.clone(), (b.ask_price - b.bid_price) / mid))
                } else {
                    None
                }
            })
            .collect();

        // Index spot symbols by symbol name for margin availability check
        let spot_margin_map: HashMap<String, &SpotSymbolInfo> = spot_info
            .iter()
            .filter(|s| s.status == "TRADING" && s.quote_asset == "USDT")
            .map(|s| (s.symbol.clone(), s))
            .collect();

        // Index margin assets by asset name for borrow rate lookup
        let margin_asset_map: HashMap<String, &MarginAsset> = margin_assets
            .iter()
            .filter(|a| a.borrowable)
            .map(|a| (a.asset.clone(), a))
            .collect();

        // Filter and score pairs
        let mut qualified: Vec<QualifiedPair> = funding_rates
            .iter()
            .filter_map(|fr| {
                self.qualify_pair(fr, &volume_map, &spread_map, &spot_margin_map, &margin_asset_map)
            })
            .collect();

        // Sort by score (descending) - pairs with higher net profitability first
        qualified.sort_by(|a, b| b.score.cmp(&a.score));

        info!(qualified_count = qualified.len(), "Pairs qualified with margin support");

        Ok(qualified)
    }

    /// Check if a pair qualifies and calculate its score.
    /// A pair must have:
    /// 1. USDT perpetual futures available
    /// 2. Spot margin trading enabled for hedging
    /// 3. Base asset borrowable (for shorting spot if needed)
    /// 4. Sufficient volume, tight spread, and meaningful funding rate
    fn qualify_pair(
        &self,
        funding: &FundingRate,
        volume_map: &HashMap<String, Decimal>,
        spread_map: &HashMap<String, Decimal>,
        spot_margin_map: &HashMap<String, &SpotSymbolInfo>,
        margin_asset_map: &HashMap<String, &MarginAsset>,
    ) -> Option<QualifiedPair> {
        let symbol = &funding.symbol;

        // Must be USDT perpetual
        if !symbol.ends_with("USDT") {
            return None;
        }

        // Derive spot symbol (same as futures for USDT pairs)
        let spot_symbol = symbol.clone();

        // Extract base asset (e.g., "BTC" from "BTCUSDT")
        let base_asset = symbol.strip_suffix("USDT")?.to_string();

        // Check if spot margin trading is available
        let spot_info = spot_margin_map.get(&spot_symbol);
        let margin_available = spot_info
            .map(|s| s.is_margin_trading_allowed)
            .unwrap_or(false);

        if !margin_available {
            debug!(symbol, "No spot margin trading available - cannot hedge");
            return None;
        }

        // Check if base asset is borrowable (needed for shorting spot)
        let margin_asset = margin_asset_map.get(&base_asset);
        let borrow_rate = margin_asset.and_then(|a| a.margin_interest_rate);

        if margin_asset.is_none() {
            if self.config.require_both_directions {
                debug!(symbol, base_asset, "Base asset not borrowable - skipping (require_both_directions=true)");
                return None;
            } else {
                warn!(symbol, base_asset, "Base asset not borrowable - can only go long spot (for negative funding)");
            }
        }

        // Get volume
        let volume = *volume_map.get(symbol)?;
        if volume < self.config.min_volume_24h {
            debug!(symbol, %volume, "Volume below threshold");
            return None;
        }

        // Get spread
        let spread = *spread_map.get(symbol)?;
        if spread > self.config.max_spread {
            debug!(symbol, %spread, "Spread above threshold");
            return None;
        }

        // Check funding rate magnitude
        let funding_rate_abs = funding.funding_rate.abs();
        if funding_rate_abs < self.config.min_funding_rate {
            debug!(symbol, %funding_rate_abs, "Funding rate below threshold");
            return None;
        }

        // Calculate net profitability considering borrow costs
        // If funding > 0: Short perp, long spot (no borrow needed)
        // If funding < 0: Long perp, short spot (need to borrow base asset)
        let borrow_cost_per_8h = if funding.funding_rate < Decimal::ZERO {
            // Need to short spot, calculate borrow cost
            // Daily rate / 3 = 8-hour rate (funding settlement period)
            borrow_rate.unwrap_or(dec!(0.001)) / dec!(3)
        } else {
            Decimal::ZERO
        };

        let net_funding = funding_rate_abs - borrow_cost_per_8h;

        // Calculate score - prioritize net profitability
        // Score = (Net Funding × 0.5) + (Volume_normalized × 0.25) + (1/Spread × 0.2) + (Margin Safety × 0.05)
        let funding_score = net_funding * dec!(10000); // Scale for comparison
        let volume_score = (volume / dec!(1_000_000_000)).min(dec!(1)); // Cap at 1B
        let spread_score = dec!(1) / (spread * dec!(10000) + dec!(1));
        let margin_safety = if margin_asset.is_some() { dec!(1) } else { dec!(0.5) };

        let score = funding_score * dec!(0.5)
            + volume_score * dec!(0.25)
            + spread_score * dec!(0.2)
            + margin_safety * dec!(0.05);

        debug!(
            symbol,
            %funding.funding_rate,
            %net_funding,
            %borrow_cost_per_8h,
            %score,
            "Pair qualified"
        );

        Some(QualifiedPair {
            symbol: symbol.clone(),
            spot_symbol,
            base_asset,
            funding_rate: funding.funding_rate,
            volume_24h: volume,
            spread,
            open_interest: Decimal::ZERO, // TODO: Fetch separately if needed
            margin_available,
            borrow_rate,
            score,
        })
    }

    /// Get the next funding time for a symbol (in milliseconds since epoch).
    pub fn next_funding_time() -> i64 {
        use chrono::{Timelike, Utc};

        let now = Utc::now();
        let hour = now.hour();

        // Funding times: 00:00, 08:00, 16:00 UTC
        let next_funding_hour = match hour {
            0..=7 => 8,
            8..=15 => 16,
            _ => 24, // Next day 00:00
        };

        let next_funding = if next_funding_hour == 24 {
            now.date_naive()
                .succ_opt()
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        } else {
            now.date_naive()
                .and_hms_opt(next_funding_hour, 0, 0)
                .unwrap()
        };

        next_funding.and_utc().timestamp_millis()
    }

    /// Time until next funding in seconds.
    pub fn seconds_until_funding() -> i64 {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let next_ms = Self::next_funding_time();
        (next_ms - now_ms) / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PairSelectionConfig {
        PairSelectionConfig {
            min_volume_24h: dec!(20_000_000),
            min_funding_rate: dec!(0.00005),
            max_spread: dec!(0.0005),
            min_open_interest: dec!(50_000_000),
            require_both_directions: false,
        }
    }

    #[test]
    fn test_funding_time_calculation() {
        let seconds = MarketScanner::seconds_until_funding();
        assert!(seconds > 0);
        assert!(seconds <= 8 * 3600); // Max 8 hours
    }
}
