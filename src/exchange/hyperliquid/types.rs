//! Type definitions for Hyperliquid API responses.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Request type for Hyperliquid info endpoint.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum InfoRequest {
    /// Get metadata and asset contexts (funding rates, prices, OI).
    #[serde(rename = "metaAndAssetCtxs")]
    MetaAndAssetCtxs,

    /// Get all mid prices.
    #[serde(rename = "allMids")]
    AllMids,

    /// Get funding rate history.
    #[serde(rename = "fundingHistory")]
    FundingHistory {
        coin: String,
        #[serde(rename = "startTime")]
        start_time: i64,
        #[serde(rename = "endTime", skip_serializing_if = "Option::is_none")]
        end_time: Option<i64>,
    },
}

/// Response from metaAndAssetCtxs endpoint.
/// Returns a tuple of (Meta, Vec<AssetCtx>).
pub type MetaAndAssetCtxsResponse = (Meta, Vec<AssetCtx>);

/// Universe metadata for perpetuals.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub universe: Vec<AssetMeta>,
}

/// Metadata for a single asset in the universe.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetMeta {
    /// Asset name (e.g., "BTC", "ETH")
    pub name: String,
    /// Size decimal precision
    pub sz_decimals: u8,
    /// Maximum allowed leverage
    pub max_leverage: u8,
    /// Whether only isolated margin is allowed
    #[serde(default)]
    pub only_isolated: bool,
}

/// Real-time context for an asset (prices, funding, volume).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCtx {
    /// Current funding rate (hourly, as decimal string)
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub funding: Decimal,
    /// Open interest
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub open_interest: Decimal,
    /// Previous day price (24h ago)
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub prev_day_px: Decimal,
    /// Daily notional volume
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub day_ntl_vlm: Decimal,
    /// Premium over oracle price (can be null for inactive coins)
    #[serde(default, deserialize_with = "deserialize_decimal_str_option_null")]
    pub premium: Option<Decimal>,
    /// Oracle price
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub oracle_px: Decimal,
    /// Mark price
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub mark_px: Decimal,
    /// Mid price (between best bid and ask, can be null for inactive coins)
    #[serde(default, deserialize_with = "deserialize_decimal_str_option_null")]
    pub mid_px: Option<Decimal>,
    /// Impact prices [bid_impact, ask_impact] (can be null for inactive coins)
    #[serde(default)]
    pub impact_pxs: Option<Vec<String>>,
    /// Daily base volume (optional, not in all responses)
    #[serde(default, deserialize_with = "deserialize_decimal_str_option_null")]
    pub day_base_vlm: Option<Decimal>,
}

/// Historical funding rate record.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingHistoryRecord {
    /// Asset symbol
    pub coin: String,
    /// Funding rate at this timestamp
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub funding_rate: Decimal,
    /// Premium component
    #[serde(deserialize_with = "deserialize_decimal_str")]
    pub premium: Decimal,
    /// Timestamp in milliseconds
    pub time: i64,
}

/// Combined asset data with metadata and context.
#[derive(Debug, Clone)]
pub struct HyperliquidAsset {
    /// Asset name/symbol
    pub name: String,
    /// Size decimal precision
    pub sz_decimals: u8,
    /// Maximum leverage
    pub max_leverage: u8,
    /// Current hourly funding rate
    pub funding_rate: Decimal,
    /// Open interest in contracts
    pub open_interest: Decimal,
    /// Oracle price
    pub oracle_price: Decimal,
    /// Mark price
    pub mark_price: Decimal,
    /// 24h notional volume
    pub volume_24h: Decimal,
    /// Premium over oracle
    pub premium: Decimal,
}

/// Funding rate comparison between Hyperliquid and another venue.
#[derive(Debug, Clone)]
pub struct FundingSpread {
    /// Symbol (normalized, e.g., "BTCUSDT")
    pub symbol: String,
    /// Hyperliquid coin name (e.g., "BTC")
    pub hl_coin: String,
    /// Hyperliquid funding rate (hourly)
    pub hl_funding_hourly: Decimal,
    /// Hyperliquid funding rate (8h equivalent for comparison)
    pub hl_funding_8h: Decimal,
    /// Other venue funding rate (8h)
    pub other_funding_8h: Decimal,
    /// Spread: hl_funding_8h - other_funding_8h
    /// Positive = HL pays more to shorts (or less to longs)
    pub spread_8h: Decimal,
    /// Annualized spread percentage
    pub spread_annualized: Decimal,
    /// Recommended direction: "long_hl_short_other" or "short_hl_long_other"
    pub recommended_direction: Option<SpreadDirection>,
}

/// Direction for funding spread arbitrage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadDirection {
    /// Long on Hyperliquid, short on other venue
    LongHlShortOther,
    /// Short on Hyperliquid, long on other venue
    ShortHlLongOther,
}

// Custom deserializers for Hyperliquid's string-encoded decimals

fn deserialize_decimal_str<'de, D>(deserializer: D) -> Result<Decimal, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    s.parse::<Decimal>().map_err(serde::de::Error::custom)
}

/// Deserializer that handles both null JSON values and missing fields.
fn deserialize_decimal_str_option_null<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // First try to deserialize as Option<String> to handle null
    let opt: Option<Option<String>> = Option::deserialize(deserializer)?;
    match opt {
        Some(Some(s)) if !s.is_empty() => s
            .parse::<Decimal>()
            .map(Some)
            .map_err(serde::de::Error::custom),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_asset_ctx() {
        let json = r#"{
            "funding": "0.00001234",
            "openInterest": "1234567.89",
            "prevDayPx": "50000.0",
            "dayNtlVlm": "999999999.0",
            "premium": "0.0001",
            "oraclePx": "50000.0",
            "markPx": "50005.0",
            "midPx": "50002.5"
        }"#;

        let ctx: AssetCtx = serde_json::from_str(json).unwrap();
        assert_eq!(ctx.funding.to_string(), "0.00001234");
        assert_eq!(ctx.mark_px.to_string(), "50005.0");
    }

    #[test]
    fn test_info_request_serialization() {
        let req = InfoRequest::MetaAndAssetCtxs;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"metaAndAssetCtxs"}"#);

        let req = InfoRequest::FundingHistory {
            coin: "BTC".to_string(),
            start_time: 1234567890000,
            end_time: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""type":"fundingHistory""#));
        assert!(json.contains(r#""coin":"BTC""#));
    }
}
