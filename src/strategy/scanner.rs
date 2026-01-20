//! Market scanner for identifying funding rate opportunities.

use crate::config::PairSelectionConfig;
use crate::exchange::{BinanceClient, FundingRate, MarginAsset, QualifiedPair, SpotSymbolInfo};
use anyhow::Result;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{info, instrument, trace, warn};

/// Reasons for rejecting a pair during qualification.
#[derive(Debug, Clone, Copy)]
enum RejectReason {
    NotUsdt,
    NoMargin,
    NotBorrowable, // Can't short spot for negative funding
    LowVolume,
    WideSpread,
    LowFunding,
    LowNetFunding, // Net funding (after borrow costs) too low
    MissingData,
}

/// Details about a near-miss opportunity for diagnostic logging.
#[derive(Debug, Clone)]
struct NearMissOpportunity {
    symbol: String,
    funding_rate: Decimal,
    rejection_reason: String,
    actual_value: String,
    threshold: String,
    /// Proximity score: higher = closer to qualifying (0-100)
    proximity: u8,
}

/// Scans the market for profitable funding rate opportunities.
pub struct MarketScanner {
    config: PairSelectionConfig,
}

/// Calculate a proximity score (0-100) for how close a value is to reaching a threshold.
/// Higher score = closer to qualifying.
fn calculate_percentage_proximity(actual: Decimal, threshold: Decimal) -> u8 {
    if threshold.is_zero() {
        return 100;
    }
    let ratio = actual / threshold;
    // Convert to 0-100 scale, capped at 100
    (ratio * dec!(100)).to_u8().unwrap_or(100).min(100)
}

/// Calculate proximity for inverse thresholds (where lower is better, e.g., spread).
fn calculate_inverse_proximity(actual: Decimal, threshold: Decimal) -> u8 {
    if actual.is_zero() {
        return 100;
    }
    let ratio = threshold / actual;
    (ratio * dec!(100)).to_u8().unwrap_or(0).min(100)
}

/// Calculate proximity score based on funding rate significance.
fn calculate_proximity_score(funding_rate: Decimal, min_funding_rate: Decimal) -> u8 {
    if min_funding_rate.is_zero() {
        return 100;
    }
    // Higher funding rates get higher proximity scores
    let ratio = funding_rate / min_funding_rate;
    // Scale: 1x threshold = 50, 2x = 75, 4x = 100
    let score = dec!(50) + (ratio - dec!(1)) * dec!(25);
    score.to_u8().unwrap_or(50).clamp(0, 100)
}

/// Get fallback borrow rate for an asset when margin data is unavailable.
///
/// Rates are based on typical borrow rates observed on Binance:
/// - Tier 1 (BTC, ETH): Lower rates due to high liquidity
/// - Tier 2 (Major alts): Medium rates
/// - Tier 3 (Other): Use config default (conservative)
fn get_fallback_borrow_rate(asset: &str, config_default: Decimal) -> Decimal {
    match asset.to_uppercase().as_str() {
        // Tier 1: Major crypto - typically lowest rates (0.02-0.05% daily)
        "BTC" | "ETH" => dec!(0.0003), // 0.03% daily
        // Tier 2: Large caps - moderate rates (0.05-0.1% daily)
        "BNB" | "SOL" | "XRP" | "ADA" | "DOGE" | "AVAX" | "DOT" | "LINK" | "MATIC" => {
            dec!(0.0007)
        } // 0.07% daily
        // Tier 3: Stablecoins - very low rates
        "USDT" | "USDC" | "BUSD" | "DAI" | "TUSD" => dec!(0.0001), // 0.01% daily
        // Tier 4: All others - use conservative config default
        _ => config_default,
    }
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
        let (funding_rates, futures_tickers, book_tickers, spot_info, spot_tickers) = tokio::try_join!(
            client.get_funding_rates(),
            client.get_24h_tickers(),
            client.get_book_tickers(),
            client.get_spot_exchange_info(),
            client.get_spot_24h_tickers(),
        )?;

        // Fetch margin assets separately (requires auth, may fail in read-only mode)
        let margin_assets = match client.get_margin_all_assets().await {
            Ok(assets) => assets,
            Err(e) => {
                warn!(
                    "Failed to fetch margin assets (may need API key): {}. Using empty list.",
                    e
                );
                Vec::new()
            }
        };

        info!(
            funding_count = funding_rates.len(),
            futures_ticker_count = futures_tickers.len(),
            spot_ticker_count = spot_tickers.len(),
            spot_symbols = spot_info.len(),
            margin_assets = margin_assets.len(),
            "Fetched market data"
        );

        // Build combined volume map (futures + spot volume for better liquidity assessment)
        // Start with futures volume
        let mut volume_map: HashMap<String, Decimal> = futures_tickers
            .iter()
            .map(|t| (t.symbol.clone(), t.quote_volume))
            .collect();

        // Add spot volume to the same symbols
        for spot_ticker in &spot_tickers {
            if let Some(futures_vol) = volume_map.get_mut(&spot_ticker.symbol) {
                *futures_vol += spot_ticker.quote_volume;
            }
        }

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

        // Track rejection reasons for summary logging
        let mut rejected_no_usdt = 0usize;
        let mut rejected_no_margin = 0usize;
        let mut rejected_not_borrowable = 0usize;
        let mut rejected_low_volume = 0usize;
        let mut rejected_wide_spread = 0usize;
        let mut rejected_low_funding = 0usize;
        let mut rejected_low_net_funding = 0usize;
        let mut rejected_missing_data = 0usize;

        // Track near-miss opportunities for diagnostic logging
        let mut near_misses: Vec<NearMissOpportunity> = Vec::new();

        // Filter and score pairs
        let mut qualified: Vec<QualifiedPair> = funding_rates
            .iter()
            .filter_map(|fr| {
                match self.qualify_pair_with_details(
                    fr,
                    &volume_map,
                    &spread_map,
                    &spot_margin_map,
                    &margin_asset_map,
                ) {
                    Ok(pair) => Some(pair),
                    Err((reason, near_miss)) => {
                        match reason {
                            RejectReason::NotUsdt => rejected_no_usdt += 1,
                            RejectReason::NoMargin => rejected_no_margin += 1,
                            RejectReason::NotBorrowable => rejected_not_borrowable += 1,
                            RejectReason::LowVolume => rejected_low_volume += 1,
                            RejectReason::WideSpread => rejected_wide_spread += 1,
                            RejectReason::LowFunding => rejected_low_funding += 1,
                            RejectReason::LowNetFunding => rejected_low_net_funding += 1,
                            RejectReason::MissingData => rejected_missing_data += 1,
                        }
                        // Collect near-misses (only for pairs that got past initial filters)
                        if let Some(nm) = near_miss {
                            near_misses.push(nm);
                        }
                        None
                    }
                }
            })
            .collect();

        // Sort by score (descending) - pairs with higher net profitability first
        qualified.sort_by(|a, b| b.score.cmp(&a.score));

        let total_scanned = funding_rates.len();
        info!(
            total_scanned,
            qualified = qualified.len(),
            rejected_no_usdt,
            rejected_no_margin,
            rejected_not_borrowable,
            rejected_low_volume,
            rejected_wide_spread,
            rejected_low_funding,
            rejected_low_net_funding,
            rejected_missing_data,
            "Market scan complete"
        );

        // Log near-miss opportunities when few pairs qualify (for diagnostic visibility)
        if qualified.len() < 3 && !near_misses.is_empty() {
            // Sort near-misses by proximity (highest = closest to qualifying)
            near_misses.sort_by(|a, b| b.proximity.cmp(&a.proximity));

            // Take top 5 near-misses
            let top_near_misses: Vec<_> = near_misses.into_iter().take(5).collect();

            info!("📊 Top near-miss opportunities (closest to qualifying):");
            for nm in &top_near_misses {
                info!(
                    "   {} | funding={:.4}% | rejected: {} (actual={}, threshold={})",
                    nm.symbol,
                    nm.funding_rate * dec!(100),
                    nm.rejection_reason,
                    nm.actual_value,
                    nm.threshold
                );
            }
        }

        Ok(qualified)
    }

    /// Check if a pair qualifies with detailed rejection info for near-miss tracking.
    fn qualify_pair_with_details(
        &self,
        funding: &FundingRate,
        volume_map: &HashMap<String, Decimal>,
        spread_map: &HashMap<String, Decimal>,
        spot_margin_map: &HashMap<String, &SpotSymbolInfo>,
        margin_asset_map: &HashMap<String, &MarginAsset>,
    ) -> Result<QualifiedPair, (RejectReason, Option<NearMissOpportunity>)> {
        let symbol = &funding.symbol;

        // Must be USDT perpetual - early filter, not a near-miss
        if !symbol.ends_with("USDT") {
            return Err((RejectReason::NotUsdt, None));
        }

        // Derive spot symbol (same as futures for USDT pairs)
        let spot_symbol = symbol.clone();

        // Extract base asset (e.g., "BTC" from "BTCUSDT")
        let base_asset = symbol
            .strip_suffix("USDT")
            .ok_or((RejectReason::NotUsdt, None))?
            .to_string();

        // Check if spot margin trading is available
        let spot_info = spot_margin_map.get(&spot_symbol);
        let margin_available = spot_info
            .map(|s| s.is_margin_trading_allowed)
            .unwrap_or(false);

        if !margin_available {
            trace!(symbol, "No spot margin trading available - cannot hedge");
            // This is an infrastructure limitation, not a near-miss
            return Err((RejectReason::NoMargin, None));
        }

        // Check if base asset is borrowable (needed for shorting spot)
        let margin_asset = margin_asset_map.get(&base_asset);
        let borrow_rate = margin_asset.and_then(|a| a.margin_interest_rate);

        // For negative funding rates, we need to short spot (borrow base asset)
        if funding.funding_rate < Decimal::ZERO {
            if margin_asset.is_none() {
                trace!(
                    symbol,
                    base_asset,
                    funding_rate = %funding.funding_rate,
                    "Rejecting: negative funding requires borrowing, but asset not in margin system"
                );
                // Track as near-miss if funding rate is significant
                return Err((
                    RejectReason::NotBorrowable,
                    Some(NearMissOpportunity {
                        symbol: symbol.clone(),
                        funding_rate: funding.funding_rate,
                        rejection_reason: "not_borrowable".to_string(),
                        actual_value: format!("funding={:.4}%", funding.funding_rate.abs() * dec!(100)),
                        threshold: "requires margin borrowing".to_string(),
                        proximity: calculate_proximity_score(funding.funding_rate.abs(), self.config.min_funding_rate),
                    }),
                ));
            }
        }

        // Get volume
        let volume = match volume_map.get(symbol) {
            Some(&v) => v,
            None => return Err((RejectReason::MissingData, None)),
        };

        if volume < self.config.min_volume_24h {
            trace!(symbol, %volume, "Volume below threshold");
            let proximity = calculate_percentage_proximity(volume, self.config.min_volume_24h);
            return Err((
                RejectReason::LowVolume,
                Some(NearMissOpportunity {
                    symbol: symbol.clone(),
                    funding_rate: funding.funding_rate,
                    rejection_reason: "low_volume".to_string(),
                    actual_value: format!("${:.0}M", volume / dec!(1_000_000)),
                    threshold: format!("${:.0}M", self.config.min_volume_24h / dec!(1_000_000)),
                    proximity,
                }),
            ));
        }

        // Get spread
        let spread = match spread_map.get(symbol) {
            Some(&s) => s,
            None => return Err((RejectReason::MissingData, None)),
        };

        if spread > self.config.max_spread {
            trace!(symbol, %spread, "Spread above threshold");
            let proximity = calculate_inverse_proximity(spread, self.config.max_spread);
            return Err((
                RejectReason::WideSpread,
                Some(NearMissOpportunity {
                    symbol: symbol.clone(),
                    funding_rate: funding.funding_rate,
                    rejection_reason: "wide_spread".to_string(),
                    actual_value: format!("{:.4}%", spread * dec!(100)),
                    threshold: format!("{:.4}%", self.config.max_spread * dec!(100)),
                    proximity,
                }),
            ));
        }

        // Check funding rate magnitude
        let funding_rate_abs = funding.funding_rate.abs();
        if funding_rate_abs < self.config.min_funding_rate {
            trace!(symbol, %funding_rate_abs, "Funding rate below threshold");
            let proximity = calculate_percentage_proximity(funding_rate_abs, self.config.min_funding_rate);
            return Err((
                RejectReason::LowFunding,
                Some(NearMissOpportunity {
                    symbol: symbol.clone(),
                    funding_rate: funding.funding_rate,
                    rejection_reason: "low_funding".to_string(),
                    actual_value: format!("{:.4}%", funding_rate_abs * dec!(100)),
                    threshold: format!("{:.4}%", self.config.min_funding_rate * dec!(100)),
                    proximity,
                }),
            ));
        }

        // Calculate net profitability considering borrow costs
        let borrow_cost_per_8h = if funding.funding_rate < Decimal::ZERO {
            let daily_rate = borrow_rate.unwrap_or_else(|| {
                let fallback =
                    get_fallback_borrow_rate(&base_asset, self.config.default_borrow_rate);
                trace!(
                    symbol,
                    %base_asset,
                    %fallback,
                    "Using fallback borrow rate (margin data unavailable)"
                );
                fallback
            });
            daily_rate / dec!(3)
        } else {
            Decimal::ZERO
        };

        let net_funding = funding_rate_abs - borrow_cost_per_8h;

        // CRITICAL: Reject pairs where net funding (after borrow costs) is too low
        if net_funding < self.config.min_net_funding {
            warn!(
                symbol,
                %net_funding,
                %funding_rate_abs,
                %borrow_cost_per_8h,
                min_required = %self.config.min_net_funding,
                "Rejecting: net funding too low after borrow costs"
            );
            let proximity = calculate_percentage_proximity(net_funding.max(Decimal::ZERO), self.config.min_net_funding);
            return Err((
                RejectReason::LowNetFunding,
                Some(NearMissOpportunity {
                    symbol: symbol.clone(),
                    funding_rate: funding.funding_rate,
                    rejection_reason: "low_net_funding".to_string(),
                    actual_value: format!("{:.4}% (funding) - {:.4}% (borrow) = {:.4}%",
                        funding_rate_abs * dec!(100),
                        borrow_cost_per_8h * dec!(100),
                        net_funding * dec!(100)),
                    threshold: format!("{:.4}%", self.config.min_net_funding * dec!(100)),
                    proximity,
                }),
            ));
        }

        // Calculate score - prioritize net profitability
        let funding_score = net_funding * dec!(10000);
        let volume_score = (volume / dec!(1_000_000_000)).min(dec!(1));
        let spread_score = dec!(1) / (spread * dec!(10000) + dec!(1));
        let margin_safety = if margin_asset.is_some() {
            dec!(1)
        } else {
            dec!(0.5)
        };

        let score = funding_score * dec!(0.5)
            + volume_score * dec!(0.25)
            + spread_score * dec!(0.2)
            + margin_safety * dec!(0.05);

        trace!(
            symbol,
            %funding.funding_rate,
            %net_funding,
            %borrow_cost_per_8h,
            %score,
            "Pair qualified"
        );

        Ok(QualifiedPair {
            symbol: symbol.clone(),
            spot_symbol,
            base_asset,
            funding_rate: funding.funding_rate,
            next_funding_time: funding.funding_time,
            volume_24h: volume,
            spread,
            open_interest: Decimal::ZERO,
            margin_available,
            borrow_rate,
            score,
        })
    }

    /// Check if a pair qualifies and calculate its score (wrapper for tests).
    /// A pair must have:
    /// 1. USDT perpetual futures available
    /// 2. Spot margin trading enabled for hedging
    /// 3. Base asset borrowable (for shorting spot if needed)
    /// 4. Sufficient volume, tight spread, and meaningful funding rate
    #[cfg(test)]
    fn qualify_pair(
        &self,
        funding: &FundingRate,
        volume_map: &HashMap<String, Decimal>,
        spread_map: &HashMap<String, Decimal>,
        spot_margin_map: &HashMap<String, &SpotSymbolInfo>,
        margin_asset_map: &HashMap<String, &MarginAsset>,
    ) -> Option<QualifiedPair> {
        self.qualify_pair_with_details(
            funding,
            volume_map,
            spread_map,
            spot_margin_map,
            margin_asset_map,
        )
        .ok()
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
    use crate::exchange::{FundingRate, MarginAsset, SpotSymbolInfo};

    // =========================================================================
    // Test Helpers
    // =========================================================================

    fn test_config() -> PairSelectionConfig {
        PairSelectionConfig {
            min_volume_24h: dec!(50_000_000),
            min_funding_rate: dec!(0.0001),
            max_spread: dec!(0.0002),
            min_open_interest: dec!(50_000_000),
            max_positions: 5,
            default_borrow_rate: dec!(0.001), // 0.1% daily fallback
            min_net_funding: dec!(0.0001),    // 0.01% minimum net funding per 8h
        }
    }

    fn make_funding_rate(symbol: &str, rate: Decimal) -> FundingRate {
        FundingRate {
            symbol: symbol.to_string(),
            funding_rate: rate,
            funding_time: 0,
            mark_price: Some(dec!(50000)),
        }
    }

    fn make_spot_info(symbol: &str, margin_allowed: bool) -> SpotSymbolInfo {
        SpotSymbolInfo {
            symbol: symbol.to_string(),
            base_asset: symbol.strip_suffix("USDT").unwrap_or("BTC").to_string(),
            quote_asset: "USDT".to_string(),
            status: "TRADING".to_string(),
            is_margin_trading_allowed: margin_allowed,
        }
    }

    fn make_margin_asset(asset: &str, daily_rate: Decimal) -> MarginAsset {
        MarginAsset {
            asset: asset.to_string(),
            borrowable: true,
            collateral: true,
            margin_interest_rate: Some(daily_rate),
        }
    }

    fn setup_test_data() -> (
        HashMap<String, Decimal>,        // volume_map
        HashMap<String, Decimal>,        // spread_map
        HashMap<String, SpotSymbolInfo>, // spot_margin_map (owned)
        HashMap<String, MarginAsset>,    // margin_asset_map (owned)
    ) {
        let mut volume_map = HashMap::new();
        volume_map.insert("BTCUSDT".to_string(), dec!(1_000_000_000));
        volume_map.insert("ETHUSDT".to_string(), dec!(500_000_000));
        volume_map.insert("LOWVOLUSDT".to_string(), dec!(10_000_000)); // Below threshold

        let mut spread_map = HashMap::new();
        spread_map.insert("BTCUSDT".to_string(), dec!(0.00005)); // Very tight
        spread_map.insert("ETHUSDT".to_string(), dec!(0.0001)); // Acceptable
        spread_map.insert("WIDESPREADUSDT".to_string(), dec!(0.001)); // Too wide

        let mut spot_map = HashMap::new();
        spot_map.insert("BTCUSDT".to_string(), make_spot_info("BTCUSDT", true));
        spot_map.insert("ETHUSDT".to_string(), make_spot_info("ETHUSDT", true));
        spot_map.insert(
            "NOMARGINUSDT".to_string(),
            make_spot_info("NOMARGINUSDT", false),
        );

        let mut margin_map = HashMap::new();
        margin_map.insert("BTC".to_string(), make_margin_asset("BTC", dec!(0.001)));
        margin_map.insert("ETH".to_string(), make_margin_asset("ETH", dec!(0.002)));

        (volume_map, spread_map, spot_map, margin_map)
    }

    // =========================================================================
    // Basic Tests
    // =========================================================================

    #[test]
    fn test_funding_time_calculation() {
        let seconds = MarketScanner::seconds_until_funding();
        assert!(seconds > 0);
        assert!(seconds <= 8 * 3600); // Max 8 hours
    }

    #[test]
    fn test_next_funding_time_is_future() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let next_ms = MarketScanner::next_funding_time();
        assert!(next_ms > now_ms);
    }

    // =========================================================================
    // Volume Filter Tests
    // =========================================================================

    #[test]
    fn test_min_volume_filter_rejects_low_volume() {
        let scanner = MarketScanner::new(test_config());
        let (mut volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        // Set volume below threshold
        volume_map.insert("BTCUSDT".to_string(), dec!(10_000_000));

        let funding = make_funding_rate("BTCUSDT", dec!(0.001));

        // Convert to reference maps
        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_none(),
            "Should reject pair with volume below threshold"
        );
    }

    #[test]
    fn test_min_volume_filter_accepts_high_volume() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        let funding = make_funding_rate("BTCUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_some(),
            "Should accept pair with sufficient volume"
        );
    }

    // =========================================================================
    // Funding Rate Filter Tests
    // =========================================================================

    #[test]
    fn test_min_funding_rate_filter_rejects_low_rate() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        // Very small funding rate
        let funding = make_funding_rate("BTCUSDT", dec!(0.00001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_none(),
            "Should reject pair with funding rate below threshold"
        );
    }

    #[test]
    fn test_min_funding_rate_accepts_negative_rate() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        // Negative but large magnitude
        let funding = make_funding_rate("BTCUSDT", dec!(-0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_some(),
            "Should accept negative funding rate with sufficient magnitude"
        );
    }

    // =========================================================================
    // Spread Filter Tests
    // =========================================================================

    #[test]
    fn test_max_spread_filter_rejects_wide_spread() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, mut spread_map, spot_map, margin_map) = setup_test_data();

        // Wide spread
        spread_map.insert("BTCUSDT".to_string(), dec!(0.001));

        let funding = make_funding_rate("BTCUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_none(),
            "Should reject pair with spread above threshold"
        );
    }

    #[test]
    fn test_spread_calculation_accuracy() {
        // Test spread = (ask - bid) / mid
        let bid = dec!(49990);
        let ask = dec!(50010);
        let mid = (bid + ask) / dec!(2); // 50000
        let spread = (ask - bid) / mid; // 20 / 50000 = 0.0004

        assert_eq!(spread, dec!(0.0004));
    }

    // =========================================================================
    // Borrow Cost Tests
    // =========================================================================

    #[test]
    fn test_borrow_cost_calculation_for_negative_funding() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        // Negative funding rate means we need to short spot (borrow)
        let funding = make_funding_rate("BTCUSDT", dec!(-0.005));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);

        // BTC daily borrow rate = 0.001
        // 8-hour rate = 0.001 / 3 = 0.000333...
        // Net = 0.005 - 0.000333... = ~0.00467
        assert!(result.is_some());
        let pair = result.unwrap();
        assert_eq!(pair.funding_rate, dec!(-0.005));
        assert_eq!(pair.borrow_rate, Some(dec!(0.001)));
    }

    #[test]
    fn test_low_net_funding_rejected() {
        // Create config with high min_net_funding to force rejection
        let config = PairSelectionConfig {
            min_volume_24h: dec!(50_000_000),
            min_funding_rate: dec!(0.0001),
            max_spread: dec!(0.0002),
            min_open_interest: dec!(50_000_000),
            max_positions: 5,
            default_borrow_rate: dec!(0.01), // 1% daily - very high
            min_net_funding: dec!(0.005),    // Require 0.5% net funding
        };
        let scanner = MarketScanner::new(config);
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        // Funding rate 0.001 (0.1%) but borrow cost ~0.33% per 8h
        // Net funding would be negative, should be rejected
        let funding = make_funding_rate("BTCUSDT", dec!(-0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);

        // Should be rejected due to low net funding
        assert!(result.is_none(), "Expected rejection due to low net funding after borrow costs");
    }

    #[test]
    fn test_no_borrow_cost_for_positive_funding() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        // Positive funding = short perp, long spot (no borrow needed)
        let funding = make_funding_rate("BTCUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);

        // Should qualify - no borrow cost subtracted
        assert!(result.is_some());
    }

    // =========================================================================
    // Scoring Tests
    // =========================================================================

    #[test]
    fn test_score_weighting_formula() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        let funding = make_funding_rate("BTCUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        let pair = result.unwrap();

        // Verify score is reasonable
        assert!(pair.score > Decimal::ZERO);

        // Score formula:
        // funding_score = 0.001 * 10000 * 0.5 = 5
        // volume_score = min(1B/1B, 1) * 0.25 = 0.25
        // spread_score = 1/(0.00005*10000+1) * 0.2 = 1/1.5 * 0.2 = ~0.133
        // margin_safety = 1 * 0.05 = 0.05
        // Total ~= 5.43
        assert!(pair.score > dec!(5));
    }

    #[test]
    fn test_ranking_by_net_yield() {
        let scanner = MarketScanner::new(test_config());
        let (mut volume_map, mut spread_map, mut spot_map, margin_map) = setup_test_data();

        // Add test data for SOLUSDT
        volume_map.insert("SOLUSDT".to_string(), dec!(500_000_000));
        spread_map.insert("SOLUSDT".to_string(), dec!(0.0001));
        spot_map.insert("SOLUSDT".to_string(), make_spot_info("SOLUSDT", true));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        // Create funding rates with different magnitudes
        let btc_funding = make_funding_rate("BTCUSDT", dec!(0.002)); // Higher
        let eth_funding = make_funding_rate("ETHUSDT", dec!(0.001)); // Lower

        let btc_pair = scanner.qualify_pair(
            &btc_funding,
            &volume_map,
            &spread_map,
            &spot_ref,
            &margin_ref,
        );
        let eth_pair = scanner.qualify_pair(
            &eth_funding,
            &volume_map,
            &spread_map,
            &spot_ref,
            &margin_ref,
        );

        assert!(btc_pair.is_some());
        assert!(eth_pair.is_some());

        // Higher funding rate should have higher score
        assert!(btc_pair.unwrap().score > eth_pair.unwrap().score);
    }

    // =========================================================================
    // Margin Availability Tests
    // =========================================================================

    #[test]
    fn test_rejects_pair_without_margin_trading() {
        let scanner = MarketScanner::new(test_config());
        let (mut volume_map, mut spread_map, spot_map, margin_map) = setup_test_data();

        // Add data for NOMARGIN pair
        volume_map.insert("NOMARGINUSDT".to_string(), dec!(100_000_000));
        spread_map.insert("NOMARGINUSDT".to_string(), dec!(0.0001));

        let funding = make_funding_rate("NOMARGINUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_none(),
            "Should reject pair without margin trading"
        );
    }

    // =========================================================================
    // Symbol Validation Tests
    // =========================================================================

    #[test]
    fn test_rejects_non_usdt_pair() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        let funding = make_funding_rate("BTCBUSD", dec!(0.001)); // Not USDT

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(result.is_none(), "Should reject non-USDT pairs");
    }

    #[test]
    fn test_extracts_base_asset_correctly() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        let funding = make_funding_rate("BTCUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        let pair = result.unwrap();

        assert_eq!(pair.base_asset, "BTC");
        assert_eq!(pair.spot_symbol, "BTCUSDT");
    }

    // =========================================================================
    // Edge Case Tests
    // =========================================================================

    #[test]
    fn test_missing_volume_data() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        // Missing volume data for NEWUSDT
        let funding = make_funding_rate("NEWUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_none(),
            "Should reject pair with missing volume data"
        );
    }

    #[test]
    fn test_missing_spread_data() {
        let scanner = MarketScanner::new(test_config());
        let (mut volume_map, spread_map, mut spot_map, margin_map) = setup_test_data();

        // Add volume and spot but no spread
        volume_map.insert("NEWUSDT".to_string(), dec!(100_000_000));
        spot_map.insert("NEWUSDT".to_string(), make_spot_info("NEWUSDT", true));

        let funding = make_funding_rate("NEWUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        assert!(
            result.is_none(),
            "Should reject pair with missing spread data"
        );
    }

    #[test]
    fn test_qualified_pair_fields_populated() {
        let scanner = MarketScanner::new(test_config());
        let (volume_map, spread_map, spot_map, margin_map) = setup_test_data();

        let funding = make_funding_rate("BTCUSDT", dec!(0.001));

        let spot_ref: HashMap<String, &SpotSymbolInfo> =
            spot_map.iter().map(|(k, v)| (k.clone(), v)).collect();
        let margin_ref: HashMap<String, &MarginAsset> =
            margin_map.iter().map(|(k, v)| (k.clone(), v)).collect();

        let result =
            scanner.qualify_pair(&funding, &volume_map, &spread_map, &spot_ref, &margin_ref);
        let pair = result.unwrap();

        assert_eq!(pair.symbol, "BTCUSDT");
        assert_eq!(pair.spot_symbol, "BTCUSDT");
        assert_eq!(pair.base_asset, "BTC");
        assert_eq!(pair.funding_rate, dec!(0.001));
        assert_eq!(pair.volume_24h, dec!(1_000_000_000));
        assert_eq!(pair.spread, dec!(0.00005));
        assert!(pair.margin_available);
        assert!(pair.borrow_rate.is_some());
        assert!(pair.score > Decimal::ZERO);
    }

    // =========================================================================
    // Fallback Borrow Rate Tests
    // =========================================================================

    #[test]
    fn test_fallback_borrow_rate_tier1_btc() {
        let config_default = dec!(0.001);
        let rate = super::get_fallback_borrow_rate("BTC", config_default);
        assert_eq!(
            rate,
            dec!(0.0003),
            "BTC should use tier 1 rate (0.03% daily)"
        );
    }

    #[test]
    fn test_fallback_borrow_rate_tier1_eth() {
        let config_default = dec!(0.001);
        let rate = super::get_fallback_borrow_rate("ETH", config_default);
        assert_eq!(
            rate,
            dec!(0.0003),
            "ETH should use tier 1 rate (0.03% daily)"
        );
    }

    #[test]
    fn test_fallback_borrow_rate_tier2_sol() {
        let config_default = dec!(0.001);
        let rate = super::get_fallback_borrow_rate("SOL", config_default);
        assert_eq!(
            rate,
            dec!(0.0007),
            "SOL should use tier 2 rate (0.07% daily)"
        );
    }

    #[test]
    fn test_fallback_borrow_rate_tier2_bnb() {
        let config_default = dec!(0.001);
        let rate = super::get_fallback_borrow_rate("BNB", config_default);
        assert_eq!(
            rate,
            dec!(0.0007),
            "BNB should use tier 2 rate (0.07% daily)"
        );
    }

    #[test]
    fn test_fallback_borrow_rate_stablecoins() {
        let config_default = dec!(0.001);
        assert_eq!(
            super::get_fallback_borrow_rate("USDT", config_default),
            dec!(0.0001),
            "USDT should use stablecoin rate (0.01% daily)"
        );
        assert_eq!(
            super::get_fallback_borrow_rate("USDC", config_default),
            dec!(0.0001),
            "USDC should use stablecoin rate"
        );
    }

    #[test]
    fn test_fallback_borrow_rate_unknown_asset() {
        let config_default = dec!(0.0015);
        let rate = super::get_fallback_borrow_rate("OBSCURECOIN", config_default);
        assert_eq!(
            rate, config_default,
            "Unknown asset should use config default"
        );
    }

    #[test]
    fn test_fallback_borrow_rate_case_insensitive() {
        let config_default = dec!(0.001);
        assert_eq!(
            super::get_fallback_borrow_rate("btc", config_default),
            super::get_fallback_borrow_rate("BTC", config_default),
            "Asset lookup should be case insensitive"
        );
    }
}
