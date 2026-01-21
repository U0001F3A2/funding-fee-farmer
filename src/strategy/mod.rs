//! Trading strategy implementation.
//!
//! Contains the core logic for:
//! - Market scanning and opportunity detection
//! - Capital allocation across positions
//! - Order execution and position management
//! - Hedge rebalancing to maintain delta neutrality

mod allocator;
mod executor;
mod rebalancer;
mod scanner;

pub use allocator::{CapitalAllocator, PositionAllocation, PositionReduction};
pub use executor::{EntryResult, MarginContext, OrderExecutor};
pub use rebalancer::{HedgeRebalancer, RebalanceAction, RebalanceConfig, RebalanceResult};
pub use scanner::MarketScanner;
