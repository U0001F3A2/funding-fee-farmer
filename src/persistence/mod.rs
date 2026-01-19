//! SQLite persistence for mock trading state.
//!
//! Persists trading state to survive restarts:
//! - Account balance and positions
//! - Funding collection history
//! - Interest payment history
//! - Trade execution history
//! - Periodic equity snapshots

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use tracing::{debug, info, warn};

/// Persisted position state.
#[derive(Debug, Clone)]
pub struct PersistedPosition {
    pub symbol: String,
    pub futures_qty: Decimal,
    pub futures_entry_price: Decimal,
    pub spot_qty: Decimal,
    pub spot_entry_price: Decimal,
    pub borrowed_amount: Decimal,
    pub opened_at: DateTime<Utc>,
    pub total_funding_received: Decimal,
    pub total_interest_paid: Decimal,
    pub funding_collections: u32,
    /// Expected funding rate at position entry (for anomaly detection)
    pub expected_funding_rate: Decimal,
}

/// Persisted trading state.
#[derive(Debug, Clone)]
pub struct PersistedState {
    pub initial_balance: Decimal,
    pub balance: Decimal,
    pub total_funding_received: Decimal,
    pub total_trading_fees: Decimal,
    pub total_borrow_interest: Decimal,
    pub order_count: u64,
    pub positions: HashMap<String, PersistedPosition>,
    pub last_saved: DateTime<Utc>,
}

/// SQLite-based persistence manager.
pub struct PersistenceManager {
    conn: Connection,
}

impl PersistenceManager {
    /// Create a new persistence manager, initializing the database if needed.
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let conn = Connection::open(db_path.as_ref())
            .with_context(|| format!("Failed to open database at {:?}", db_path.as_ref()))?;

        let manager = Self { conn };
        manager.init_schema()?;

        info!("Persistence manager initialized at {:?}", db_path.as_ref());
        Ok(manager)
    }

    /// Initialize database schema.
    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            -- Trading state (singleton row)
            CREATE TABLE IF NOT EXISTS trading_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                initial_balance TEXT NOT NULL,
                balance TEXT NOT NULL,
                total_funding_received TEXT NOT NULL,
                total_trading_fees TEXT NOT NULL,
                total_borrow_interest TEXT NOT NULL,
                order_count INTEGER NOT NULL,
                last_saved TEXT NOT NULL
            );

            -- Positions
            CREATE TABLE IF NOT EXISTS positions (
                symbol TEXT PRIMARY KEY,
                futures_qty TEXT NOT NULL,
                futures_entry_price TEXT NOT NULL,
                spot_qty TEXT NOT NULL,
                spot_entry_price TEXT NOT NULL,
                borrowed_amount TEXT NOT NULL,
                opened_at TEXT NOT NULL,
                total_funding_received TEXT NOT NULL,
                total_interest_paid TEXT NOT NULL,
                funding_collections INTEGER NOT NULL,
                expected_funding_rate TEXT NOT NULL DEFAULT '0'
            );

            -- Funding events history
            CREATE TABLE IF NOT EXISTS funding_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                symbol TEXT NOT NULL,
                amount TEXT NOT NULL,
                position_value TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_funding_timestamp ON funding_events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_funding_symbol ON funding_events(symbol);

            -- Interest events history
            CREATE TABLE IF NOT EXISTS interest_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                symbol TEXT NOT NULL,
                amount TEXT NOT NULL,
                borrowed_amount TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_interest_timestamp ON interest_events(timestamp);

            -- Trade history
            CREATE TABLE IF NOT EXISTS trades (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                symbol TEXT NOT NULL,
                side TEXT NOT NULL,
                order_type TEXT NOT NULL,
                quantity TEXT NOT NULL,
                price TEXT NOT NULL,
                fee TEXT NOT NULL,
                is_futures INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_trades_timestamp ON trades(timestamp);
            CREATE INDEX IF NOT EXISTS idx_trades_symbol ON trades(symbol);

            -- Equity snapshots (hourly)
            CREATE TABLE IF NOT EXISTS equity_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                balance TEXT NOT NULL,
                unrealized_pnl TEXT NOT NULL,
                total_equity TEXT NOT NULL,
                realized_pnl TEXT NOT NULL,
                position_count INTEGER NOT NULL,
                max_drawdown TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_timestamp ON equity_snapshots(timestamp);
            "#,
        )?;

        // Migration: Add expected_funding_rate column if it doesn't exist (for existing DBs)
        let _ = self.conn.execute(
            "ALTER TABLE positions ADD COLUMN expected_funding_rate TEXT NOT NULL DEFAULT '0'",
            [],
        ); // Ignore error if column already exists

        debug!("Database schema initialized");
        Ok(())
    }

    /// Save the complete trading state.
    pub fn save_state(&self, state: &PersistedState) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        // Upsert trading state
        tx.execute(
            r#"
            INSERT INTO trading_state (id, initial_balance, balance, total_funding_received,
                                       total_trading_fees, total_borrow_interest, order_count, last_saved)
            VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                initial_balance = ?1,
                balance = ?2,
                total_funding_received = ?3,
                total_trading_fees = ?4,
                total_borrow_interest = ?5,
                order_count = ?6,
                last_saved = ?7
            "#,
            params![
                state.initial_balance.to_string(),
                state.balance.to_string(),
                state.total_funding_received.to_string(),
                state.total_trading_fees.to_string(),
                state.total_borrow_interest.to_string(),
                state.order_count,
                state.last_saved.to_rfc3339(),
            ],
        )?;

        // Clear and reinsert positions
        tx.execute("DELETE FROM positions", [])?;

        for pos in state.positions.values() {
            tx.execute(
                r#"
                INSERT INTO positions (symbol, futures_qty, futures_entry_price, spot_qty,
                                       spot_entry_price, borrowed_amount, opened_at,
                                       total_funding_received, total_interest_paid, funding_collections,
                                       expected_funding_rate)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
                params![
                    pos.symbol,
                    pos.futures_qty.to_string(),
                    pos.futures_entry_price.to_string(),
                    pos.spot_qty.to_string(),
                    pos.spot_entry_price.to_string(),
                    pos.borrowed_amount.to_string(),
                    pos.opened_at.to_rfc3339(),
                    pos.total_funding_received.to_string(),
                    pos.total_interest_paid.to_string(),
                    pos.funding_collections,
                    pos.expected_funding_rate.to_string(),
                ],
            )?;
        }

        tx.commit()?;

        debug!(
            balance = %state.balance,
            positions = state.positions.len(),
            "State saved to database"
        );
        Ok(())
    }

    /// Load the trading state from database.
    pub fn load_state(&self) -> Result<Option<PersistedState>> {
        // Load trading state
        let state_row: Option<(String, String, String, String, String, u64, String)> = self
            .conn
            .query_row(
                r#"
                SELECT initial_balance, balance, total_funding_received, total_trading_fees,
                       total_borrow_interest, order_count, last_saved
                FROM trading_state WHERE id = 1
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;

        let Some((initial_balance, balance, funding, fees, interest, order_count, last_saved)) =
            state_row
        else {
            return Ok(None);
        };

        // Load positions
        let mut stmt = self.conn.prepare(
            r#"
            SELECT symbol, futures_qty, futures_entry_price, spot_qty, spot_entry_price,
                   borrowed_amount, opened_at, total_funding_received, total_interest_paid,
                   funding_collections, expected_funding_rate
            FROM positions
            "#,
        )?;

        let positions: HashMap<String, PersistedPosition> = stmt
            .query_map([], |row| {
                let symbol: String = row.get(0)?;
                Ok((
                    symbol.clone(),
                    PersistedPosition {
                        symbol,
                        futures_qty: Decimal::from_str(&row.get::<_, String>(1)?)
                            .unwrap_or_default(),
                        futures_entry_price: Decimal::from_str(&row.get::<_, String>(2)?)
                            .unwrap_or_default(),
                        spot_qty: Decimal::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
                        spot_entry_price: Decimal::from_str(&row.get::<_, String>(4)?)
                            .unwrap_or_default(),
                        borrowed_amount: Decimal::from_str(&row.get::<_, String>(5)?)
                            .unwrap_or_default(),
                        opened_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now()),
                        total_funding_received: Decimal::from_str(&row.get::<_, String>(7)?)
                            .unwrap_or_default(),
                        total_interest_paid: Decimal::from_str(&row.get::<_, String>(8)?)
                            .unwrap_or_default(),
                        funding_collections: row.get(9)?,
                        expected_funding_rate: Decimal::from_str(&row.get::<_, String>(10)?)
                            .unwrap_or_default(),
                    },
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let state = PersistedState {
            initial_balance: Decimal::from_str(&initial_balance).unwrap_or_default(),
            balance: Decimal::from_str(&balance).unwrap_or_default(),
            total_funding_received: Decimal::from_str(&funding).unwrap_or_default(),
            total_trading_fees: Decimal::from_str(&fees).unwrap_or_default(),
            total_borrow_interest: Decimal::from_str(&interest).unwrap_or_default(),
            order_count,
            positions,
            last_saved: DateTime::parse_from_rfc3339(&last_saved)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        };

        info!(
            balance = %state.balance,
            positions = state.positions.len(),
            last_saved = %state.last_saved,
            "Loaded state from database"
        );

        Ok(Some(state))
    }

    /// Record a funding event.
    pub fn record_funding_event(
        &self,
        symbol: &str,
        amount: Decimal,
        position_value: Option<Decimal>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO funding_events (timestamp, symbol, amount, position_value)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![
                Utc::now().to_rfc3339(),
                symbol,
                amount.to_string(),
                position_value.map(|v| v.to_string()),
            ],
        )?;
        Ok(())
    }

    /// Record an interest event.
    pub fn record_interest_event(
        &self,
        symbol: &str,
        amount: Decimal,
        borrowed_amount: Option<Decimal>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO interest_events (timestamp, symbol, amount, borrowed_amount)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![
                Utc::now().to_rfc3339(),
                symbol,
                amount.to_string(),
                borrowed_amount.map(|v| v.to_string()),
            ],
        )?;
        Ok(())
    }

    /// Record a trade.
    pub fn record_trade(
        &self,
        symbol: &str,
        side: &str,
        order_type: &str,
        quantity: Decimal,
        price: Decimal,
        fee: Decimal,
        is_futures: bool,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO trades (timestamp, symbol, side, order_type, quantity, price, fee, is_futures)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                Utc::now().to_rfc3339(),
                symbol,
                side,
                order_type,
                quantity.to_string(),
                price.to_string(),
                fee.to_string(),
                is_futures as i32,
            ],
        )?;
        Ok(())
    }

    /// Record an equity snapshot.
    pub fn record_snapshot(
        &self,
        balance: Decimal,
        unrealized_pnl: Decimal,
        total_equity: Decimal,
        realized_pnl: Decimal,
        position_count: usize,
        max_drawdown: Decimal,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO equity_snapshots (timestamp, balance, unrealized_pnl, total_equity,
                                          realized_pnl, position_count, max_drawdown)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                Utc::now().to_rfc3339(),
                balance.to_string(),
                unrealized_pnl.to_string(),
                total_equity.to_string(),
                realized_pnl.to_string(),
                position_count,
                max_drawdown.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Get total funding received by symbol.
    pub fn get_funding_stats(&self) -> Result<HashMap<String, Decimal>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT symbol, SUM(CAST(amount AS REAL)) as total
            FROM funding_events
            GROUP BY symbol
            "#,
        )?;

        let stats: HashMap<String, Decimal> = stmt
            .query_map([], |row| {
                let symbol: String = row.get(0)?;
                let total: f64 = row.get(1)?;
                Ok((symbol, Decimal::from_f64_retain(total).unwrap_or_default()))
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(stats)
    }

    /// Get recent equity snapshots for performance analysis.
    pub fn get_recent_snapshots(&self, limit: usize) -> Result<Vec<(DateTime<Utc>, Decimal)>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT timestamp, total_equity
            FROM equity_snapshots
            ORDER BY timestamp DESC
            LIMIT ?1
            "#,
        )?;

        let snapshots: Vec<(DateTime<Utc>, Decimal)> = stmt
            .query_map([limit], |row| {
                let ts: String = row.get(0)?;
                let equity: String = row.get(1)?;
                Ok((
                    DateTime::parse_from_rfc3339(&ts)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    Decimal::from_str(&equity).unwrap_or_default(),
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(snapshots)
    }

    /// Check if we have any saved state.
    pub fn has_state(&self) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM trading_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Clear all data (for testing or reset).
    pub fn clear_all(&self) -> Result<()> {
        warn!("Clearing all persistence data");
        self.conn.execute_batch(
            r#"
            DELETE FROM trading_state;
            DELETE FROM positions;
            DELETE FROM funding_events;
            DELETE FROM interest_events;
            DELETE FROM trades;
            DELETE FROM equity_snapshots;
            "#,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_save_and_load_state() {
        let manager = PersistenceManager::new(":memory:").unwrap();

        let mut positions = HashMap::new();
        positions.insert(
            "BTCUSDT".to_string(),
            PersistedPosition {
                symbol: "BTCUSDT".to_string(),
                futures_qty: dec!(-0.1),
                futures_entry_price: dec!(50000),
                spot_qty: dec!(0.1),
                spot_entry_price: dec!(50000),
                borrowed_amount: Decimal::ZERO,
                opened_at: Utc::now(),
                total_funding_received: dec!(10),
                total_interest_paid: dec!(1),
                funding_collections: 2,
                expected_funding_rate: dec!(0.0001), // 0.01% expected funding rate
            },
        );

        let state = PersistedState {
            initial_balance: dec!(10000),
            balance: dec!(10009),
            total_funding_received: dec!(10),
            total_trading_fees: dec!(1),
            total_borrow_interest: Decimal::ZERO,
            order_count: 4,
            positions,
            last_saved: Utc::now(),
        };

        manager.save_state(&state).unwrap();

        let loaded = manager.load_state().unwrap().unwrap();
        assert_eq!(loaded.balance, dec!(10009));
        assert_eq!(loaded.positions.len(), 1);
        assert_eq!(loaded.positions["BTCUSDT"].futures_qty, dec!(-0.1));
    }

    #[test]
    fn test_funding_events() {
        let manager = PersistenceManager::new(":memory:").unwrap();

        manager
            .record_funding_event("BTCUSDT", dec!(5.5), Some(dec!(50000)))
            .unwrap();
        manager
            .record_funding_event("BTCUSDT", dec!(4.5), Some(dec!(50000)))
            .unwrap();
        manager
            .record_funding_event("ETHUSDT", dec!(3.0), Some(dec!(3000)))
            .unwrap();

        let stats = manager.get_funding_stats().unwrap();
        assert_eq!(stats.len(), 2);
    }
}
