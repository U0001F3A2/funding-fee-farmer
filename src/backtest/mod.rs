//! Backtesting module for optimizing trading strategy parameters.
//!
//! This module provides:
//! - Historical data loading (CSV import + live collection)
//! - Time-based simulation engine
//! - Parameter sweep for optimization
//! - Performance metrics calculation
//!
//! # Example
//!
//! ```rust,ignore
//! use funding_fee_farmer::backtest::{BacktestEngine, CsvDataLoader, BacktestConfig};
//!
//! let loader = CsvDataLoader::new("data/funding_rates.csv")?;
//! let config = BacktestConfig::default();
//! let mut engine = BacktestEngine::new(loader, config);
//!
//! let result = engine.run(start, end).await?;
//! println!("Return: {:.2}%", result.metrics.total_return_pct);
//! ```

mod data;
mod engine;
mod metrics;
mod runner;

pub use data::{
    CsvDataLoader, DataLoader, LiveDataCollector, MarketSnapshot, SymbolData,
};
pub use engine::{BacktestEngine, BacktestResult, StepResult};
pub use metrics::{BacktestMetrics, EquityPoint};
pub use runner::{ParameterSpace, SweepResults, SweepRunner};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Configuration for a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// Initial capital for the backtest
    pub initial_balance: Decimal,

    /// Time step for simulation (in minutes)
    /// Smaller = more accurate but slower
    pub time_step_minutes: u32,

    /// Whether to record every equity point (can use lots of memory)
    pub record_equity_curve: bool,

    /// Whether to record individual trades
    pub record_trades: bool,

    /// Path to output results (optional)
    pub output_path: Option<String>,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            initial_balance: Decimal::new(10000, 0), // $10,000
            time_step_minutes: 60,                   // 1 hour steps
            record_equity_curve: true,
            record_trades: true,
            output_path: None,
        }
    }
}

/// Funding times for Binance perpetual futures (UTC hours).
pub const FUNDING_HOURS: [u32; 3] = [0, 8, 16];

/// Check if a timestamp is a funding time.
pub fn is_funding_time(timestamp: &DateTime<Utc>) -> bool {
    use chrono::Timelike;
    FUNDING_HOURS.contains(&timestamp.hour()) && timestamp.minute() == 0
}

/// Calculate the next funding time from a given timestamp.
pub fn next_funding_time(from: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::{Duration, Timelike};

    let hour = from.hour();
    let minute = from.minute();

    // Find next funding hour
    let next_hour = FUNDING_HOURS
        .iter()
        .find(|&&h| h > hour || (h == hour && minute == 0))
        .copied();

    match next_hour {
        Some(h) if h == hour && minute == 0 => from, // Already at funding time
        Some(h) => from
            .date_naive()
            .and_hms_opt(h, 0, 0)
            .unwrap()
            .and_utc(),
        None => {
            // Next day at 00:00
            (from + Duration::days(1))
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Timelike};

    #[test]
    fn test_is_funding_time() {
        let funding = Utc.with_ymd_and_hms(2024, 1, 15, 8, 0, 0).unwrap();
        assert!(is_funding_time(&funding));

        let not_funding = Utc.with_ymd_and_hms(2024, 1, 15, 8, 1, 0).unwrap();
        assert!(!is_funding_time(&not_funding));

        let not_funding_hour = Utc.with_ymd_and_hms(2024, 1, 15, 10, 0, 0).unwrap();
        assert!(!is_funding_time(&not_funding_hour));
    }

    #[test]
    fn test_next_funding_time() {
        // Before first funding
        let t1 = Utc.with_ymd_and_hms(2024, 1, 15, 5, 30, 0).unwrap();
        let next1 = next_funding_time(t1);
        assert_eq!(next1.hour(), 8);
        assert_eq!(next1.minute(), 0);

        // Between 08:00 and 16:00
        let t2 = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let next2 = next_funding_time(t2);
        assert_eq!(next2.hour(), 16);

        // After 16:00, should be next day 00:00
        let t3 = Utc.with_ymd_and_hms(2024, 1, 15, 20, 0, 0).unwrap();
        let next3 = next_funding_time(t3);
        assert_eq!(next3.day(), 16);
        assert_eq!(next3.hour(), 0);
    }
}
