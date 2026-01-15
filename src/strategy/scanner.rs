//! Market scanner for identifying funding rate opportunities.

use crate::config::PairSelectionConfig;
use crate::exchange::{BinanceClient, FundingRate, QualifiedPair};
use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, info, instrument};

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
    #[instrument(skip(self, client))]
    pub async fn scan(&self, client: &BinanceClient) -> Result<Vec<QualifiedPair>> {
        // Fetch all required data in parallel
        let (funding_rates, tickers, book_tickers) = tokio::try_join!(
            client.get_funding_rates(),
            client.get_24h_tickers(),
            client.get_book_tickers(),
        )?;

        info!(
            funding_count = funding_rates.len(),
            ticker_count = tickers.len(),
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

        // Filter and score pairs
        let mut qualified: Vec<QualifiedPair> = funding_rates
            .iter()
            .filter_map(|fr| self.qualify_pair(fr, &volume_map, &spread_map))
            .collect();

        // Sort by score (descending)
        qualified.sort_by(|a, b| b.score.cmp(&a.score));

        info!(qualified_count = qualified.len(), "Pairs qualified");

        Ok(qualified)
    }

    /// Check if a pair qualifies and calculate its score.
    fn qualify_pair(
        &self,
        funding: &FundingRate,
        volume_map: &HashMap<String, Decimal>,
        spread_map: &HashMap<String, Decimal>,
    ) -> Option<QualifiedPair> {
        let symbol = &funding.symbol;

        // Must be USDT perpetual
        if !symbol.ends_with("USDT") {
            return None;
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
        let funding_rate = funding.funding_rate.abs();
        if funding_rate < self.config.min_funding_rate {
            debug!(symbol, %funding_rate, "Funding rate below threshold");
            return None;
        }

        // Calculate score
        // Score = (Funding × 0.4) + (Volume_normalized × 0.3) + (1/Spread × 0.2) + (Stability × 0.1)
        let funding_score = funding_rate * dec!(10000); // Scale for comparison
        let volume_score = (volume / dec!(1_000_000_000)).min(dec!(1)); // Cap at 1B
        let spread_score = dec!(1) / (spread * dec!(10000) + dec!(1));

        let score = funding_score * dec!(0.4)
            + volume_score * dec!(0.3)
            + spread_score * dec!(0.2)
            + dec!(0.1); // Base stability score

        Some(QualifiedPair {
            symbol: symbol.clone(),
            funding_rate: funding.funding_rate,
            volume_24h: volume,
            spread,
            open_interest: Decimal::ZERO, // TODO: Fetch separately
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
            min_volume_24h: dec!(100_000_000),
            min_funding_rate: dec!(0.0001),
            max_spread: dec!(0.0002),
            min_open_interest: dec!(50_000_000),
        }
    }

    #[test]
    fn test_funding_time_calculation() {
        let seconds = MarketScanner::seconds_until_funding();
        assert!(seconds > 0);
        assert!(seconds <= 8 * 3600); // Max 8 hours
    }
}
