//! Configuration management for the funding fee farmer.
//!
//! Loads settings from environment variables and config files.

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;

/// Main application configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Binance API credentials
    pub binance: BinanceConfig,
    /// Capital allocation settings
    pub capital: CapitalConfig,
    /// Risk management parameters
    pub risk: RiskConfig,
    /// Pair selection criteria
    pub pair_selection: PairSelectionConfig,
    /// Execution parameters
    pub execution: ExecutionConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BinanceConfig {
    /// API key for authentication
    pub api_key: String,
    /// Secret key for signing requests
    pub secret_key: String,
    /// Use testnet instead of production
    #[serde(default)]
    pub testnet: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CapitalConfig {
    /// Maximum percentage of capital to deploy (0.0-1.0)
    #[serde(default = "default_max_utilization")]
    pub max_utilization: Decimal,
    /// Reserve buffer for margin safety (0.0-1.0)
    #[serde(default = "default_reserve_buffer")]
    pub reserve_buffer: Decimal,
    /// Minimum position size in USDT
    #[serde(default = "default_min_position_size")]
    pub min_position_size: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    /// Maximum allowable drawdown (0.0-1.0)
    #[serde(default = "default_max_drawdown")]
    pub max_drawdown: Decimal,
    /// Minimum margin ratio to maintain
    #[serde(default = "default_min_margin_ratio")]
    pub min_margin_ratio: Decimal,
    /// Maximum allocation to a single position (0.0-1.0)
    #[serde(default = "default_max_single_position")]
    pub max_single_position: Decimal,

    // Position loss detection
    /// Maximum hours to keep an unprofitable position
    #[serde(default = "default_max_unprofitable_hours")]
    pub max_unprofitable_hours: u32,
    /// Minimum expected annualized yield (0.0-1.0, e.g., 0.10 = 10%)
    #[serde(default = "default_min_expected_yield")]
    pub min_expected_yield: Decimal,
    /// Grace period hours before profit checking starts
    #[serde(default = "default_grace_period_hours")]
    pub grace_period_hours: u32,
    /// Maximum allowed funding deviation (0.0-1.0)
    #[serde(default = "default_max_funding_deviation")]
    pub max_funding_deviation: Decimal,

    // Malfunction detection
    /// Maximum API errors per minute before alert
    #[serde(default = "default_max_errors_per_minute")]
    pub max_errors_per_minute: u32,
    /// Maximum consecutive order failures before alert
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: u32,
    /// Delta drift percentage that triggers emergency (0.0-1.0)
    #[serde(default = "default_emergency_delta_drift")]
    pub emergency_delta_drift: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PairSelectionConfig {
    /// Minimum 24h trading volume in USDT
    #[serde(default = "default_min_volume")]
    pub min_volume_24h: Decimal,
    /// Minimum absolute funding rate
    #[serde(default = "default_min_funding_rate")]
    pub min_funding_rate: Decimal,
    /// Maximum bid-ask spread
    #[serde(default = "default_max_spread")]
    pub max_spread: Decimal,
    /// Minimum open interest in USDT
    #[serde(default = "default_min_open_interest")]
    pub min_open_interest: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionConfig {
    /// Default leverage for positions
    #[serde(default = "default_leverage")]
    pub default_leverage: u8,
    /// Maximum leverage allowed
    #[serde(default = "default_max_leverage")]
    pub max_leverage: u8,
    /// Maximum slippage tolerance (0.0-1.0)
    #[serde(default = "default_slippage_tolerance")]
    pub slippage_tolerance: Decimal,
    /// Order timeout in seconds
    #[serde(default = "default_order_timeout")]
    pub order_timeout_secs: u64,
}

// Default value functions
fn default_max_utilization() -> Decimal {
    Decimal::new(85, 2) // 0.85
}

fn default_reserve_buffer() -> Decimal {
    Decimal::new(10, 2) // 0.10
}

fn default_min_position_size() -> Decimal {
    Decimal::new(1000, 0) // 1000 USDT
}

fn default_max_drawdown() -> Decimal {
    Decimal::new(5, 2) // 0.05
}

fn default_min_margin_ratio() -> Decimal {
    Decimal::new(3, 0) // 3.0x
}

fn default_max_single_position() -> Decimal {
    Decimal::new(30, 2) // 0.30
}

fn default_min_volume() -> Decimal {
    Decimal::new(100_000_000, 0) // $100M
}

fn default_min_funding_rate() -> Decimal {
    Decimal::new(1, 4) // 0.0001 (0.01%)
}

fn default_max_spread() -> Decimal {
    Decimal::new(2, 4) // 0.0002 (0.02%)
}

fn default_min_open_interest() -> Decimal {
    Decimal::new(50_000_000, 0) // $50M
}

fn default_leverage() -> u8 {
    5
}

fn default_max_leverage() -> u8 {
    10
}

fn default_slippage_tolerance() -> Decimal {
    Decimal::new(5, 4) // 0.0005 (0.05%)
}

fn default_order_timeout() -> u64 {
    30
}

// Position loss detection defaults
fn default_max_unprofitable_hours() -> u32 {
    48
}

fn default_min_expected_yield() -> Decimal {
    Decimal::new(10, 2) // 0.10 (10% APY)
}

fn default_grace_period_hours() -> u32 {
    8
}

fn default_max_funding_deviation() -> Decimal {
    Decimal::new(20, 2) // 0.20 (20%)
}

// Malfunction detection defaults
fn default_max_errors_per_minute() -> u32 {
    10
}

fn default_max_consecutive_failures() -> u32 {
    3
}

fn default_emergency_delta_drift() -> Decimal {
    Decimal::new(10, 2) // 0.10 (10%)
}

impl Config {
    /// Load configuration from environment variables and config files.
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();

        let config = config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(
                config::Environment::default()
                    .separator("__")
                    .prefix("FFF"),
            )
            .build()
            .context("Failed to build configuration")?;

        config
            .try_deserialize()
            .context("Failed to deserialize configuration")
    }

    /// Validate configuration values.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.capital.max_utilization > Decimal::ZERO
                && self.capital.max_utilization <= Decimal::ONE,
            "max_utilization must be between 0 and 1"
        );

        anyhow::ensure!(
            self.risk.max_drawdown > Decimal::ZERO
                && self.risk.max_drawdown <= Decimal::ONE,
            "max_drawdown must be between 0 and 1"
        );

        anyhow::ensure!(
            self.execution.default_leverage >= 1
                && self.execution.default_leverage <= self.execution.max_leverage,
            "default_leverage must be >= 1 and <= max_leverage"
        );

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            binance: BinanceConfig {
                api_key: String::new(),
                secret_key: String::new(),
                testnet: true,
            },
            capital: CapitalConfig {
                max_utilization: default_max_utilization(),
                reserve_buffer: default_reserve_buffer(),
                min_position_size: default_min_position_size(),
            },
            risk: RiskConfig {
                max_drawdown: default_max_drawdown(),
                min_margin_ratio: default_min_margin_ratio(),
                max_single_position: default_max_single_position(),
                max_unprofitable_hours: default_max_unprofitable_hours(),
                min_expected_yield: default_min_expected_yield(),
                grace_period_hours: default_grace_period_hours(),
                max_funding_deviation: default_max_funding_deviation(),
                max_errors_per_minute: default_max_errors_per_minute(),
                max_consecutive_failures: default_max_consecutive_failures(),
                emergency_delta_drift: default_emergency_delta_drift(),
            },
            pair_selection: PairSelectionConfig {
                min_volume_24h: default_min_volume(),
                min_funding_rate: default_min_funding_rate(),
                max_spread: default_max_spread(),
                min_open_interest: default_min_open_interest(),
            },
            execution: ExecutionConfig {
                default_leverage: default_leverage(),
                max_leverage: default_max_leverage(),
                slippage_tolerance: default_slippage_tolerance(),
                order_timeout_secs: default_order_timeout(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_valid() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }
}
