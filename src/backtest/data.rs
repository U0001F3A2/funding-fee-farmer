//! Historical data loading for backtesting.
//!
//! Provides CSV import and live data collection capabilities.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A snapshot of market data at a specific point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub timestamp: DateTime<Utc>,
    pub symbols: Vec<SymbolData>,
}

impl MarketSnapshot {
    /// Create an empty snapshot at the given timestamp.
    pub fn new(timestamp: DateTime<Utc>) -> Self {
        Self {
            timestamp,
            symbols: Vec::new(),
        }
    }

    /// Get funding rates as a HashMap for MockBinanceClient.
    pub fn funding_rates(&self) -> HashMap<String, Decimal> {
        self.symbols
            .iter()
            .map(|s| (s.symbol.clone(), s.funding_rate))
            .collect()
    }

    /// Get prices as a HashMap for MockBinanceClient.
    pub fn prices(&self) -> HashMap<String, Decimal> {
        self.symbols
            .iter()
            .map(|s| (s.symbol.clone(), s.price))
            .collect()
    }

    /// Get symbol data by symbol name.
    pub fn get_symbol(&self, symbol: &str) -> Option<&SymbolData> {
        self.symbols.iter().find(|s| s.symbol == symbol)
    }
}

/// Market data for a single trading pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolData {
    pub symbol: String,
    pub funding_rate: Decimal,
    pub price: Decimal,
    pub volume_24h: Decimal,
    pub spread: Decimal,
    pub open_interest: Decimal,
}

impl SymbolData {
    /// Calculate bid price assuming symmetric spread.
    pub fn bid_price(&self) -> Decimal {
        self.price * (Decimal::ONE - self.spread / Decimal::TWO)
    }

    /// Calculate ask price assuming symmetric spread.
    pub fn ask_price(&self) -> Decimal {
        self.price * (Decimal::ONE + self.spread / Decimal::TWO)
    }
}

/// Trait for loading historical market data.
pub trait DataLoader: Send + Sync {
    /// Load all snapshots in the given time range.
    fn load_snapshots(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<MarketSnapshot>>;

    /// Get the available date range in the data.
    fn available_range(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)>;

    /// Get all available symbols.
    fn available_symbols(&self) -> Vec<String>;
}

/// CSV data loader for historical backtesting.
///
/// Expected CSV format:
/// ```csv
/// timestamp,symbol,funding_rate,price,volume_24h,spread,open_interest
/// 2024-01-01T00:00:00Z,BTCUSDT,0.0001,42000.50,1500000000,0.0001,800000000
/// ```
#[derive(Clone)]
pub struct CsvDataLoader {
    /// Loaded snapshots indexed by timestamp
    snapshots: Vec<MarketSnapshot>,
    /// All available symbols
    symbols: Vec<String>,
}

impl CsvDataLoader {
    /// Load data from a CSV file.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read CSV file: {}", path.display()))?;

        Self::from_csv_content(&content)
    }

    /// Load data from CSV content string.
    pub fn from_csv_content(content: &str) -> Result<Self> {
        let mut rows: Vec<CsvRow> = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            // Skip header
            if line_num == 0 && line.starts_with("timestamp") {
                continue;
            }

            if line.trim().is_empty() {
                continue;
            }

            let row = CsvRow::parse(line)
                .with_context(|| format!("Failed to parse line {}: {}", line_num + 1, line))?;
            rows.push(row);
        }

        if rows.is_empty() {
            anyhow::bail!("CSV file contains no data rows");
        }

        // Group by timestamp
        let mut by_timestamp: HashMap<DateTime<Utc>, Vec<SymbolData>> = HashMap::new();
        let mut all_symbols: std::collections::HashSet<String> = std::collections::HashSet::new();

        for row in rows {
            all_symbols.insert(row.symbol.clone());
            by_timestamp
                .entry(row.timestamp)
                .or_default()
                .push(SymbolData {
                    symbol: row.symbol,
                    funding_rate: row.funding_rate,
                    price: row.price,
                    volume_24h: row.volume_24h,
                    spread: row.spread,
                    open_interest: row.open_interest,
                });
        }

        // Convert to sorted snapshots
        let mut snapshots: Vec<MarketSnapshot> = by_timestamp
            .into_iter()
            .map(|(timestamp, symbols)| MarketSnapshot { timestamp, symbols })
            .collect();

        snapshots.sort_by_key(|s| s.timestamp);

        let mut symbols: Vec<String> = all_symbols.into_iter().collect();
        symbols.sort();

        Ok(Self { snapshots, symbols })
    }

    /// Create a loader from in-memory snapshots.
    pub fn from_snapshots(snapshots: Vec<MarketSnapshot>) -> Self {
        let mut symbols: std::collections::HashSet<String> = std::collections::HashSet::new();
        for snapshot in &snapshots {
            for sym in &snapshot.symbols {
                symbols.insert(sym.symbol.clone());
            }
        }

        let mut symbols: Vec<String> = symbols.into_iter().collect();
        symbols.sort();

        Self { snapshots, symbols }
    }

    /// Get total number of snapshots.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Check if the loader has no data.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}

impl DataLoader for CsvDataLoader {
    fn load_snapshots(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<MarketSnapshot>> {
        let filtered: Vec<MarketSnapshot> = self
            .snapshots
            .iter()
            .filter(|s| s.timestamp >= start && s.timestamp <= end)
            .cloned()
            .collect();

        Ok(filtered)
    }

    fn available_range(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        if self.snapshots.is_empty() {
            return None;
        }

        let start = self.snapshots.first().unwrap().timestamp;
        let end = self.snapshots.last().unwrap().timestamp;
        Some((start, end))
    }

    fn available_symbols(&self) -> Vec<String> {
        self.symbols.clone()
    }
}

/// Internal struct for parsing CSV rows.
#[derive(Debug)]
struct CsvRow {
    timestamp: DateTime<Utc>,
    symbol: String,
    funding_rate: Decimal,
    price: Decimal,
    volume_24h: Decimal,
    spread: Decimal,
    open_interest: Decimal,
}

impl CsvRow {
    fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 7 {
            anyhow::bail!(
                "Expected 7 columns (timestamp,symbol,funding_rate,price,volume_24h,spread,open_interest), got {}",
                parts.len()
            );
        }

        Ok(Self {
            timestamp: parts[0]
                .trim()
                .parse()
                .with_context(|| format!("Invalid timestamp: {}", parts[0]))?,
            symbol: parts[1].trim().to_string(),
            funding_rate: parts[2]
                .trim()
                .parse()
                .with_context(|| format!("Invalid funding_rate: {}", parts[2]))?,
            price: parts[3]
                .trim()
                .parse()
                .with_context(|| format!("Invalid price: {}", parts[3]))?,
            volume_24h: parts[4]
                .trim()
                .parse()
                .with_context(|| format!("Invalid volume_24h: {}", parts[4]))?,
            spread: parts[5]
                .trim()
                .parse()
                .with_context(|| format!("Invalid spread: {}", parts[5]))?,
            open_interest: parts[6]
                .trim()
                .parse()
                .with_context(|| format!("Invalid open_interest: {}", parts[6]))?,
        })
    }
}

/// Live data collector for gathering data from the real Binance API.
///
/// Stores snapshots to SQLite for future backtesting.
pub struct LiveDataCollector {
    persistence_path: String,
    collection_interval_secs: u64,
}

impl LiveDataCollector {
    /// Create a new live data collector.
    pub fn new(persistence_path: &str, collection_interval_secs: u64) -> Self {
        Self {
            persistence_path: persistence_path.to_string(),
            collection_interval_secs,
        }
    }

    /// Get the persistence path.
    pub fn persistence_path(&self) -> &str {
        &self.persistence_path
    }

    /// Get the collection interval.
    pub fn collection_interval_secs(&self) -> u64 {
        self.collection_interval_secs
    }

    // NOTE: The actual collection loop will be implemented in a separate
    // background task that uses BinanceClient to fetch live data.
    // This struct just holds the configuration.
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};
    use rust_decimal_macros::dec;

    #[test]
    fn test_csv_parsing() {
        let csv = r#"timestamp,symbol,funding_rate,price,volume_24h,spread,open_interest
2024-01-01T00:00:00Z,BTCUSDT,0.0001,42000.50,1500000000,0.0001,800000000
2024-01-01T00:00:00Z,ETHUSDT,0.00015,2300.25,800000000,0.00012,400000000
2024-01-01T08:00:00Z,BTCUSDT,0.00012,42100.00,1600000000,0.0001,850000000
"#;

        let loader = CsvDataLoader::from_csv_content(csv).unwrap();

        assert_eq!(loader.len(), 2); // 2 timestamps
        assert_eq!(loader.available_symbols(), vec!["BTCUSDT", "ETHUSDT"]);

        let range = loader.available_range().unwrap();
        assert_eq!(range.0, Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(range.1, Utc.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).unwrap());
    }

    #[test]
    fn test_market_snapshot_helpers() {
        let snapshot = MarketSnapshot {
            timestamp: Utc::now(),
            symbols: vec![
                SymbolData {
                    symbol: "BTCUSDT".to_string(),
                    funding_rate: dec!(0.0001),
                    price: dec!(42000),
                    volume_24h: dec!(1000000000),
                    spread: dec!(0.0002),
                    open_interest: dec!(500000000),
                },
                SymbolData {
                    symbol: "ETHUSDT".to_string(),
                    funding_rate: dec!(0.00015),
                    price: dec!(2300),
                    volume_24h: dec!(500000000),
                    spread: dec!(0.00015),
                    open_interest: dec!(200000000),
                },
            ],
        };

        let funding_rates = snapshot.funding_rates();
        assert_eq!(funding_rates.get("BTCUSDT"), Some(&dec!(0.0001)));
        assert_eq!(funding_rates.get("ETHUSDT"), Some(&dec!(0.00015)));

        let prices = snapshot.prices();
        assert_eq!(prices.get("BTCUSDT"), Some(&dec!(42000)));

        let btc = snapshot.get_symbol("BTCUSDT").unwrap();
        assert_eq!(btc.bid_price(), dec!(42000) * dec!(0.9999));
        assert_eq!(btc.ask_price(), dec!(42000) * dec!(1.0001));
    }

    #[test]
    fn test_filter_by_date_range() {
        let csv = r#"timestamp,symbol,funding_rate,price,volume_24h,spread,open_interest
2024-01-01T00:00:00Z,BTCUSDT,0.0001,42000,1500000000,0.0001,800000000
2024-01-02T00:00:00Z,BTCUSDT,0.0001,42500,1500000000,0.0001,800000000
2024-01-03T00:00:00Z,BTCUSDT,0.0001,43000,1500000000,0.0001,800000000
"#;

        let loader = CsvDataLoader::from_csv_content(csv).unwrap();

        let start = Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2024, 1, 2, 12, 0, 0).unwrap();

        let filtered = loader.load_snapshots(start, end).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].timestamp.day(), 2);
    }
}
