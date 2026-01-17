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

    // =========================================================================
    // Basic Drawdown Tracking Tests
    // =========================================================================

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

    #[test]
    fn test_initial_state() {
        let tracker = DrawdownTracker::new(dec!(0.05), dec!(10000));

        assert_eq!(tracker.peak_equity(), dec!(10000));
        assert_eq!(tracker.current_drawdown(), Decimal::ZERO);
        assert_eq!(tracker.session_mdd(), Decimal::ZERO);
    }

    #[test]
    fn test_new_high_resets_drawdown() {
        let mut tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // Drop to create drawdown
        tracker.update(dec!(9500)); // 5% drawdown
        assert!(tracker.current_drawdown() > Decimal::ZERO);

        // New high resets current drawdown (but not session MDD)
        tracker.update(dec!(10500));
        assert_eq!(tracker.current_drawdown(), Decimal::ZERO);
        assert_eq!(tracker.peak_equity(), dec!(10500));

        // Session MDD should still reflect the 5% drawdown
        assert_eq!(tracker.session_mdd(), dec!(0.05));
    }

    #[test]
    fn test_session_mdd_tracks_worst() {
        let mut tracker = DrawdownTracker::new(dec!(0.20), dec!(10000));

        // First drawdown: 3%
        tracker.update(dec!(9700));
        assert_eq!(tracker.session_mdd(), dec!(0.03));

        // Recover
        tracker.update(dec!(10000));

        // Second drawdown: 5% (worse than first)
        tracker.update(dec!(9500));
        assert_eq!(tracker.session_mdd(), dec!(0.05));

        // Recover again
        tracker.update(dec!(10200));

        // Third drawdown: 2% (not worse than session MDD)
        tracker.update(dec!(9996)); // ~2% from 10200
        assert_eq!(tracker.session_mdd(), dec!(0.05)); // Still 5%
    }

    // =========================================================================
    // Warning Check Tests
    // =========================================================================

    #[test]
    fn test_no_warning_when_safe() {
        let tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // At 0% drawdown, no warning
        let (is_warning, distance) = tracker.warning_check();
        assert!(!is_warning);
        assert_eq!(distance, dec!(0.10)); // Full 10% buffer
    }

    #[test]
    fn test_warning_at_threshold() {
        let mut tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // Warning threshold = 10% * 0.2 = 2%
        // So warning triggers when drawdown >= 8%
        tracker.update(dec!(9200)); // 8% drawdown

        let (is_warning, distance) = tracker.warning_check();
        assert!(is_warning);
        assert_eq!(distance, dec!(0.02)); // 2% remaining
    }

    #[test]
    fn test_warning_past_threshold() {
        let mut tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // Drawdown past warning threshold
        tracker.update(dec!(9100)); // 9% drawdown

        let (is_warning, distance) = tracker.warning_check();
        assert!(is_warning);
        assert_eq!(distance, dec!(0.01)); // Only 1% remaining
    }

    // =========================================================================
    // Calmar Ratio Tests
    // =========================================================================

    #[test]
    fn test_calmar_ratio_calculation() {
        let mut tracker = DrawdownTracker::new(dec!(0.20), dec!(10000));

        // Create a 10% session MDD
        tracker.update(dec!(9000));
        assert_eq!(tracker.session_mdd(), dec!(0.10));

        // Calmar = annual_return / session_mdd
        let calmar = tracker.calmar_ratio(dec!(0.20)); // 20% annual return
        assert_eq!(calmar, Some(dec!(2))); // 20% / 10% = 2.0
    }

    #[test]
    fn test_calmar_ratio_zero_mdd() {
        let tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // No drawdown = None (avoid divide by zero)
        let calmar = tracker.calmar_ratio(dec!(0.20));
        assert_eq!(calmar, None);
    }

    #[test]
    fn test_calmar_ratio_high_return() {
        let mut tracker = DrawdownTracker::new(dec!(0.20), dec!(10000));

        // Create 5% MDD
        tracker.update(dec!(9500));

        // Very high return = high Calmar
        let calmar = tracker.calmar_ratio(dec!(1.0)); // 100% annual return
        assert_eq!(calmar, Some(dec!(20))); // 100% / 5% = 20
    }

    // =========================================================================
    // Statistics Tests
    // =========================================================================

    #[test]
    fn test_statistics_initial() {
        let tracker = DrawdownTracker::new(dec!(0.05), dec!(10000));

        let stats = tracker.statistics();

        assert_eq!(stats.peak_equity, dec!(10000));
        assert_eq!(stats.current_equity, dec!(10000));
        assert_eq!(stats.min_equity, dec!(10000));
        assert_eq!(stats.max_equity, dec!(10000));
        assert_eq!(stats.current_drawdown, Decimal::ZERO);
        assert_eq!(stats.session_mdd, Decimal::ZERO);
        assert_eq!(stats.total_return, Decimal::ZERO);
        assert_eq!(stats.snapshots, 1);
    }

    #[test]
    fn test_statistics_after_updates() {
        let mut tracker = DrawdownTracker::new(dec!(0.20), dec!(10000));

        tracker.update(dec!(11000)); // New high
        tracker.update(dec!(10500)); // Drop
        tracker.update(dec!(10000)); // Drop more

        let stats = tracker.statistics();

        assert_eq!(stats.peak_equity, dec!(11000));
        assert_eq!(stats.current_equity, dec!(10000));
        assert_eq!(stats.min_equity, dec!(10000));
        assert_eq!(stats.max_equity, dec!(11000));
        assert_eq!(stats.snapshots, 4); // Initial + 3 updates

        // Total return = (current - first) / first = (10000 - 10000) / 10000 = 0
        assert_eq!(stats.total_return, Decimal::ZERO);
    }

    #[test]
    fn test_statistics_total_return() {
        let mut tracker = DrawdownTracker::new(dec!(0.20), dec!(10000));

        // End higher than start
        tracker.update(dec!(11000));
        tracker.update(dec!(12000));

        let stats = tracker.statistics();

        // Total return = (12000 - 10000) / 10000 = 20%
        assert_eq!(stats.total_return, dec!(0.2));
    }

    #[test]
    fn test_statistics_negative_return() {
        let mut tracker = DrawdownTracker::new(dec!(0.30), dec!(10000));

        // End lower than start
        tracker.update(dec!(9000));
        tracker.update(dec!(8000));

        let stats = tracker.statistics();

        // Total return = (8000 - 10000) / 10000 = -20%
        assert_eq!(stats.total_return, dec!(-0.2));
    }

    // =========================================================================
    // Reset Tests
    // =========================================================================

    #[test]
    fn test_reset_clears_state() {
        let mut tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // Create some history and drawdown
        tracker.update(dec!(11000));
        tracker.update(dec!(10000)); // ~9% drawdown from peak
        tracker.update(dec!(9500));

        assert!(tracker.session_mdd() > Decimal::ZERO);

        // Reset
        tracker.reset(dec!(15000));

        assert_eq!(tracker.peak_equity(), dec!(15000));
        assert_eq!(tracker.current_drawdown(), Decimal::ZERO);
        assert_eq!(tracker.session_mdd(), Decimal::ZERO);

        let stats = tracker.statistics();
        assert_eq!(stats.snapshots, 1); // Only the reset snapshot
        assert_eq!(stats.current_equity, dec!(15000));
    }

    #[test]
    fn test_reset_preserves_max_drawdown_limit() {
        let mut tracker = DrawdownTracker::new(dec!(0.05), dec!(10000));

        // Reset with different equity
        tracker.reset(dec!(20000));

        // Max drawdown limit should still work
        // 5% of 20000 = 1000
        tracker.update(dec!(19500)); // 2.5% drawdown
        assert!(!tracker.update(dec!(19500))); // Still under limit

        // Exceed limit
        assert!(tracker.update(dec!(18900))); // > 5% drawdown
    }

    // =========================================================================
    // History Management Tests
    // =========================================================================

    #[test]
    fn test_history_trimming() {
        let mut tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // Add many updates
        for i in 0..1500 {
            tracker.update(dec!(10000) + Decimal::from(i));
        }

        let stats = tracker.statistics();

        // Should be capped at max_history (1000)
        assert!(stats.snapshots <= 1000);
    }

    // =========================================================================
    // Edge Case Tests
    // =========================================================================

    #[test]
    fn test_exact_max_drawdown() {
        let mut tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // Exactly at max drawdown (10%)
        let exceeded = tracker.update(dec!(9000));

        // 10% drawdown >= 10% max = exceeded
        assert!(exceeded);
    }

    #[test]
    fn test_just_under_max_drawdown() {
        let mut tracker = DrawdownTracker::new(dec!(0.10), dec!(10000));

        // Just under 10% drawdown
        let exceeded = tracker.update(dec!(9001));

        // Drawdown = (10000 - 9001) / 10000 = 9.99% < 10%
        assert!(!exceeded);
    }

    #[test]
    fn test_very_small_equity_changes() {
        let mut tracker = DrawdownTracker::new(dec!(0.05), dec!(10000));

        // Tiny increase
        tracker.update(dec!(10000.01));
        assert_eq!(tracker.peak_equity(), dec!(10000.01));

        // Tiny decrease
        tracker.update(dec!(10000.00));
        // Drawdown = 0.01 / 10000.01 ≈ 0.0001%
        assert!(tracker.current_drawdown() < dec!(0.00001));
    }

    #[test]
    fn test_large_drawdown() {
        let mut tracker = DrawdownTracker::new(dec!(0.50), dec!(100000));

        // 40% drawdown
        let exceeded = tracker.update(dec!(60000));
        assert!(!exceeded);
        assert_eq!(tracker.current_drawdown(), dec!(0.4));

        // 50% drawdown exactly
        let exceeded = tracker.update(dec!(50000));
        assert!(exceeded);
        assert_eq!(tracker.session_mdd(), dec!(0.5));
    }

    #[test]
    fn test_recovery_after_drawdown() {
        let mut tracker = DrawdownTracker::new(dec!(0.20), dec!(10000));

        // Drawdown
        tracker.update(dec!(8000)); // 20% DD
        assert_eq!(tracker.session_mdd(), dec!(0.2));

        // Full recovery and beyond
        tracker.update(dec!(12000));
        assert_eq!(tracker.current_drawdown(), Decimal::ZERO);
        assert_eq!(tracker.peak_equity(), dec!(12000));

        // Session MDD still remembers worst
        assert_eq!(tracker.session_mdd(), dec!(0.2));
    }

    #[test]
    fn test_multiple_peaks_and_troughs() {
        let mut tracker = DrawdownTracker::new(dec!(0.30), dec!(10000));

        // Pattern: up, down, up higher, down, up higher again
        tracker.update(dec!(11000)); // New peak
        tracker.update(dec!(10000)); // 9.09% DD
        tracker.update(dec!(12000)); // New peak, DD resets
        tracker.update(dec!(11000)); // 8.33% DD
        tracker.update(dec!(13000)); // New peak, DD resets

        assert_eq!(tracker.peak_equity(), dec!(13000));
        assert_eq!(tracker.current_drawdown(), Decimal::ZERO);

        // Session MDD should be the worst (9.09%)
        // (11000 - 10000) / 11000 = 0.0909...
        let expected_mdd = dec!(1000) / dec!(11000);
        assert!((tracker.session_mdd() - expected_mdd).abs() < dec!(0.0001));
    }
}
