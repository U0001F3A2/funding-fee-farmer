//! Trading strategy implementation.
//!
//! Contains the core logic for:
//! - Market scanning and opportunity detection
//! - Capital allocation across positions
//! - Order execution and position management

mod allocator;
mod executor;
mod scanner;

pub use allocator::CapitalAllocator;
pub use executor::OrderExecutor;
pub use scanner::MarketScanner;
