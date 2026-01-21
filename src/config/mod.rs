//! Configuration management for the funding fee farmer.
//!
//! Loads settings from environment variables and config files.

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Main application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Binance API credentials
    #[serde(default)]
    pub binance: BinanceConfig,
    /// Capital allocation settings
    #[serde(default)]
    pub capital: CapitalConfig,
    /// Risk management parameters
    #[serde(default)]
    pub risk: RiskConfig,
    /// Pair selection criteria
    #[serde(default)]
    pub pair_selection: PairSelectionConfig,
    /// Execution parameters
    #[serde(default)]
    pub execution: ExecutionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinanceConfig {
    /// API key for authentication
    #[serde(default)]
    pub api_key: String,
    /// Secret key for signing requests
    #[serde(default)]
    pub secret_key: String,
    /// Use testnet instead of production
    #[serde(default)]
    pub testnet: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Rebalance threshold - reduce positions when current > target * (1 + threshold)
    /// Default 0.2 = 20% drift triggers reduction
    #[serde(default = "default_rebalance_threshold")]
    pub rebalance_threshold: Decimal,
    /// Allocation concentration factor (1.0-3.0)
    /// 1.0 = equal weighting across positions
    /// 2.0 = geometric weighting (50%, 25%, 12.5%, ...)
    /// 1.5 = moderate concentration (recommended, ~35%, 25%, 20%, ...)
    #[serde(default = "default_allocation_concentration")]
    pub allocation_concentration: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    // Position entry timing
    /// Minutes before funding settlement to allow new position entry (0 = anytime)
    /// JIT entry reduces borrow interest and confirms funding rate before entry
    #[serde(default = "default_entry_window_minutes")]
    pub entry_window_minutes: u32,

    // Position holding rules
    /// Minimum hours to hold a position before considering exit (to cover trading fees)
    #[serde(default = "default_min_holding_period_hours")]
    pub min_holding_period_hours: u32,
    /// Minimum yield advantage (in %) for new position to justify switching (e.g., 0.05 = 5%)
    #[serde(default = "default_min_yield_advantage")]
    pub min_yield_advantage: Decimal,

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
    /// Maximum absolute loss in USD before force exit (e.g., 10.0 = $10)
    #[serde(default = "default_max_loss_usd")]
    pub max_loss_usd: Decimal,
    /// Maximum negative APY before force exit (0.0-1.0, e.g., 0.50 = -50% APY)
    #[serde(default = "default_max_negative_apy")]
    pub max_negative_apy: Decimal,

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

    // Circuit breaker
    /// Maximum consecutive risk check cycles with ERROR/CRITICAL alerts before halting
    #[serde(default = "default_max_consecutive_risk_cycles")]
    pub max_consecutive_risk_cycles: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Maximum number of concurrent positions (concentrate capital)
    #[serde(default = "default_max_positions")]
    pub max_positions: u8,
    /// Default daily borrow rate for assets with missing margin data
    #[serde(default = "default_borrow_rate")]
    pub default_borrow_rate: Decimal,
    /// Minimum net funding rate per 8h (funding - borrow cost) to accept a pair
    /// Rejects pairs where borrowing costs would eat most/all funding income
    #[serde(default = "default_min_net_funding")]
    pub min_net_funding: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

fn default_rebalance_threshold() -> Decimal {
    Decimal::new(20, 2) // 0.20 = 20% drift triggers reduction
}

fn default_allocation_concentration() -> Decimal {
    Decimal::new(15, 1) // 1.5 = moderate concentration (~35%, 25%, 20%, 12%, 8%)
}

fn default_max_drawdown() -> Decimal {
    Decimal::new(5, 2) // 0.05
}

fn default_min_margin_ratio() -> Decimal {
    Decimal::new(3, 0) // 3.0x
}

fn default_max_single_position() -> Decimal {
    Decimal::new(35, 2) // 0.35 - allows concentrated allocation on top pair
}

fn default_min_volume() -> Decimal {
    Decimal::new(50_000_000, 0) // $50M combined spot+futures volume
}

fn default_min_funding_rate() -> Decimal {
    Decimal::new(1, 3) // 0.001 (0.1%) - higher threshold to ensure profitable entries
}

fn default_max_spread() -> Decimal {
    Decimal::new(2, 4) // 0.0002 (0.02%)
}

fn default_min_open_interest() -> Decimal {
    Decimal::new(50_000_000, 0) // $50M
}

fn default_max_positions() -> u8 {
    5 // Concentrate capital into top pairs
}

fn default_borrow_rate() -> Decimal {
    Decimal::new(1, 3) // 0.001 (0.1% daily) - conservative fallback for unknown assets
}

fn default_min_net_funding() -> Decimal {
    // With ~0.04% taker fee per side, round-trip cost is ~0.08%
    // Minimum 24h hold (3 funding cycles) to be profitable:
    // Required: 0.08% fees / 3 cycles = 0.027% per cycle minimum
    // Set to 0.03% to ensure profitability with some buffer
    Decimal::new(3, 4) // 0.0003 (0.03%) minimum net funding per 8h after borrow costs
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

// Position entry timing defaults
fn default_entry_window_minutes() -> u32 {
    30 // Enter positions within 30 minutes of funding settlement (0 = anytime)
}

// Position holding rules defaults
fn default_min_holding_period_hours() -> u32 {
    16 // Minimum 16h hold (2 funding cycles) to ensure fees are covered
}

fn default_min_yield_advantage() -> Decimal {
    Decimal::new(2, 2) // 0.02 (2%) - new position must yield 2%+ more to justify switch
}

// Position loss detection defaults
fn default_max_unprofitable_hours() -> u32 {
    12 // Close positions unprofitable for 12+ hours
}

fn default_min_expected_yield() -> Decimal {
    Decimal::new(10, 2) // 0.10 (10% APY)
}

fn default_grace_period_hours() -> u32 {
    4 // 4 hour grace period (half a funding cycle)
}

fn default_max_funding_deviation() -> Decimal {
    Decimal::new(20, 2) // 0.20 (20%)
}

fn default_max_loss_usd() -> Decimal {
    Decimal::new(10, 0) // $10 absolute loss triggers force exit
}

fn default_max_negative_apy() -> Decimal {
    Decimal::new(50, 2) // 0.50 (-50% APY triggers force exit)
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

fn default_max_consecutive_risk_cycles() -> u32 {
    3
}

impl Config {
    /// Load configuration from environment variables and config files.
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();

        let config = config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(config::Environment::default().separator("__").prefix("FFF"))
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
            self.risk.max_drawdown > Decimal::ZERO && self.risk.max_drawdown <= Decimal::ONE,
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
                rebalance_threshold: default_rebalance_threshold(),
                allocation_concentration: default_allocation_concentration(),
            },
            risk: RiskConfig {
                max_drawdown: default_max_drawdown(),
                min_margin_ratio: default_min_margin_ratio(),
                max_single_position: default_max_single_position(),
                entry_window_minutes: default_entry_window_minutes(),
                min_holding_period_hours: default_min_holding_period_hours(),
                min_yield_advantage: default_min_yield_advantage(),
                max_unprofitable_hours: default_max_unprofitable_hours(),
                min_expected_yield: default_min_expected_yield(),
                grace_period_hours: default_grace_period_hours(),
                max_funding_deviation: default_max_funding_deviation(),
                max_loss_usd: default_max_loss_usd(),
                max_negative_apy: default_max_negative_apy(),
                max_errors_per_minute: default_max_errors_per_minute(),
                max_consecutive_failures: default_max_consecutive_failures(),
                emergency_delta_drift: default_emergency_delta_drift(),
                max_consecutive_risk_cycles: default_max_consecutive_risk_cycles(),
            },
            pair_selection: PairSelectionConfig {
                min_volume_24h: default_min_volume(),
                min_funding_rate: default_min_funding_rate(),
                max_spread: default_max_spread(),
                min_open_interest: default_min_open_interest(),
                max_positions: default_max_positions(),
                default_borrow_rate: default_borrow_rate(),
                min_net_funding: default_min_net_funding(),
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

impl Default for BinanceConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            testnet: false,
        }
    }
}

impl Default for CapitalConfig {
    fn default() -> Self {
        Self {
            max_utilization: default_max_utilization(),
            reserve_buffer: default_reserve_buffer(),
            min_position_size: default_min_position_size(),
            rebalance_threshold: default_rebalance_threshold(),
            allocation_concentration: default_allocation_concentration(),
        }
    }
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_drawdown: default_max_drawdown(),
            min_margin_ratio: default_min_margin_ratio(),
            max_single_position: default_max_single_position(),
            entry_window_minutes: default_entry_window_minutes(),
            min_holding_period_hours: default_min_holding_period_hours(),
            min_yield_advantage: default_min_yield_advantage(),
            max_unprofitable_hours: default_max_unprofitable_hours(),
            min_expected_yield: default_min_expected_yield(),
            grace_period_hours: default_grace_period_hours(),
            max_funding_deviation: default_max_funding_deviation(),
            max_loss_usd: default_max_loss_usd(),
            max_negative_apy: default_max_negative_apy(),
            max_errors_per_minute: default_max_errors_per_minute(),
            max_consecutive_failures: default_max_consecutive_failures(),
            emergency_delta_drift: default_emergency_delta_drift(),
            max_consecutive_risk_cycles: default_max_consecutive_risk_cycles(),
        }
    }
}

impl Default for PairSelectionConfig {
    fn default() -> Self {
        Self {
            min_volume_24h: default_min_volume(),
            min_funding_rate: default_min_funding_rate(),
            max_spread: default_max_spread(),
            min_open_interest: default_min_open_interest(),
            max_positions: default_max_positions(),
            default_borrow_rate: default_borrow_rate(),
            min_net_funding: default_min_net_funding(),
        }
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            default_leverage: default_leverage(),
            max_leverage: default_max_leverage(),
            slippage_tolerance: default_slippage_tolerance(),
            order_timeout_secs: default_order_timeout(),
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
