//! # Funding Fee Farmer
//!
//! A high-performance Rust application for delta-neutral funding fee farming
//! on Binance Futures.
//!
//! ## Architecture
//!
//! - `config`: Configuration management and validation
//! - `exchange`: Binance API client (REST + WebSocket)
//! - `strategy`: Trading logic, opportunity scanning, and execution
//! - `risk`: Position monitoring, margin management, and MDD tracking
//! - `persistence`: SQLite-based state persistence for mock trading
//! - `backtest`: Historical backtesting and parameter optimization
//! - `utils`: Shared utilities and decimal arithmetic

pub mod backtest;
pub mod config;
pub mod exchange;
pub mod persistence;
pub mod risk;
pub mod strategy;
pub mod utils;

pub use config::Config;
