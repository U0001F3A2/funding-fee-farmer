//! Maximum Drawdown (MDD) tracking and alerts.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use tracing::{info, warn};

/// A single equity snapshot for tracking.
#[derive(Debug, Clone)]
pub struct EquitySnapshot {
    pub timestamp: DateTime<Utc>,
    pub equity: Decimal,
}

/// Tracks maximum drawdown and equity curve.
pub struct DrawdownTracker {
    /// Maximum allowed drawdown (e.g., 0.05 for 5%)
    max_drawdown: Decimal,
    /// Peak equity value observed
    peak_equity: Decimal,
    /// Current drawdown from peak
    current_drawdown: Decimal,
    /// Maximum drawdown observed this session
    session_mdd: Decimal,
    /// Historical equity snapshots (rolling window)
    history: VecDeque<EquitySnapshot>,
    /// Maximum history size
    max_history: usize,
}

impl DrawdownTracker {
    /// Create a new drawdown tracker.
    pub fn new(max_drawdown: Decimal, initial_equity: Decimal) -> Self {
        let mut history = VecDeque::new();
        history.push_back(EquitySnapshot {
            timestamp: Utc::now(),
            equity: initial_equity,
        });

        Self {
            max_drawdown,
            peak_equity: initial_equity,
            current_drawdown: Decimal::ZERO,
            session_mdd: Decimal::ZERO,
            history,
            max_history: 1000,
        }
    }

    /// Update with new equity value.
    ///
    /// Returns true if drawdown exceeds maximum allowed.
    pub fn update(&mut self, equity: Decimal) -> bool {
        // Update peak
        if equity > self.peak_equity {
            self.peak_equity = equity;
            self.current_drawdown = Decimal::ZERO;
        } else {
            // Calculate drawdown from peak
            self.current_drawdown = (self.peak_equity - equity) / self.peak_equity;

            // Update session MDD if this is the worst
            if self.current_drawdown > self.session_mdd {
                self.session_mdd = self.current_drawdown;
                warn!(
                    mdd = %self.session_mdd,
                    peak = %self.peak_equity,
                    current = %equity,
                    "New maximum drawdown recorded"
                );
            }
        }

        // Record snapshot
        self.history.push_back(EquitySnapshot {
            timestamp: Utc::now(),
            equity,
        });

        // Trim history
        while self.history.len() > self.max_history {
            self.history.pop_front();
        }

        // Return true if we've exceeded max drawdown
        self.current_drawdown >= self.max_drawdown
    }

    /// Get current drawdown as percentage (0.0-1.0).
    pub fn current_drawdown(&self) -> Decimal {
        self.current_drawdown
    }

    /// Get session maximum drawdown as percentage (0.0-1.0).
    pub fn session_mdd(&self) -> Decimal {
        self.session_mdd
    }

    /// Get peak equity value.
    pub fn peak_equity(&self) -> Decimal {
        self.peak_equity
    }

    /// Check if we're approaching the max drawdown threshold.
    ///
    /// Returns (is_warning, distance_to_max)
    pub fn warning_check(&self) -> (bool, Decimal) {
        let distance = self.max_drawdown - self.current_drawdown;
        let warning_threshold = self.max_drawdown * dec!(0.2); // 20% buffer

        (distance <= warning_threshold, distance)
    }

    /// Calculate Calmar ratio (annual return / max drawdown).
    ///
    /// This requires enough history to estimate annual return.
    pub fn calmar_ratio(&self, annual_return: Decimal) -> Option<Decimal> {
        if self.session_mdd == Decimal::ZERO {
            return None;
        }
        Some(annual_return / self.session_mdd)
    }

    /// Get equity statistics.
    pub fn statistics(&self) -> DrawdownStats {
        let equities: Vec<Decimal> = self.history.iter().map(|s| s.equity).collect();

        let min_equity = equities.iter().copied().min().unwrap_or(Decimal::ZERO);
        let max_equity = equities.iter().copied().max().unwrap_or(Decimal::ZERO);
        let current_equity = equities.last().copied().unwrap_or(Decimal::ZERO);

        let total_return = if let Some(first) = equities.first() {
            if *first > Decimal::ZERO {
                (current_equity - *first) / *first
            } else {
                Decimal::ZERO
            }
        } else {
            Decimal::ZERO
        };

        DrawdownStats {
            peak_equity: self.peak_equity,
            current_equity,
            min_equity,
            max_equity,
            current_drawdown: self.current_drawdown,
            session_mdd: self.session_mdd,
            total_return,
            snapshots: self.history.len(),
        }
    }

    /// Reset the tracker (e.g., for a new trading session).
    pub fn reset(&mut self, initial_equity: Decimal) {
        self.peak_equity = initial_equity;
        self.current_drawdown = Decimal::ZERO;
        self.session_mdd = Decimal::ZERO;
        self.history.clear();
        self.history.push_back(EquitySnapshot {
            timestamp: Utc::now(),
            equity: initial_equity,
        });

        info!(%initial_equity, "Drawdown tracker reset");
    }
}

/// Statistics from the drawdown tracker.
#[derive(Debug, Clone)]
pub struct DrawdownStats {
    pub peak_equity: Decimal,
    pub current_equity: Decimal,
    pub min_equity: Decimal,
    pub max_equity: Decimal,
    pub current_drawdown: Decimal,
    pub session_mdd: Decimal,
    pub total_return: Decimal,
    pub snapshots: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drawdown_tracking() {
        let mut tracker = DrawdownTracker::new(dec!(0.05), dec!(10000));

        // Equity goes up
        assert!(!tracker.update(dec!(10500)));
        assert_eq!(tracker.peak_equity(), dec!(10500));
        assert_eq!(tracker.current_drawdown(), Decimal::ZERO);

        // Equity drops
        assert!(!tracker.update(dec!(10000)));
        // Drawdown = (10500 - 10000) / 10500 ≈ 4.76%
        assert!(tracker.current_drawdown() > dec!(0.04));
        assert!(tracker.current_drawdown() < dec!(0.05));

        // Further drop exceeds max drawdown
        assert!(tracker.update(dec!(9900))); // Returns true - exceeded 5%
    }

    #[test]
    fn test_warning_check() {
        let mut tracker = DrawdownTracker::new(dec!(0.05), dec!(10000));

        // At 4% drawdown, should warn (20% buffer = 1% remaining)
        // Warning threshold = 5% * 0.2 = 1%
        // Distance to max = 5% - 4% = 1%
        tracker.update(dec!(9600)); // 4% DD

        let (is_warning, distance) = tracker.warning_check();
        assert!(is_warning);
        assert!(distance <= dec!(0.01)); // Exactly at warning threshold
    }
}
