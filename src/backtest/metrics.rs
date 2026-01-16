//! Performance metrics calculation for backtesting.
//!
//! Provides Sharpe ratio, Sortino ratio, drawdown analysis, and more.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// A point on the equity curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: DateTime<Utc>,
    pub balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_equity: Decimal,
    pub drawdown: Decimal,
    pub position_count: usize,
}

impl EquityPoint {
    /// Create a new equity point.
    pub fn new(
        timestamp: DateTime<Utc>,
        balance: Decimal,
        unrealized_pnl: Decimal,
        position_count: usize,
        peak_equity: Decimal,
    ) -> Self {
        let total_equity = balance + unrealized_pnl;
        let drawdown = if peak_equity > Decimal::ZERO {
            (peak_equity - total_equity) / peak_equity
        } else {
            Decimal::ZERO
        };

        Self {
            timestamp,
            balance,
            unrealized_pnl,
            total_equity,
            drawdown,
            position_count,
        }
    }
}

/// Comprehensive backtest performance metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestMetrics {
    // Returns
    /// Total absolute return (final - initial)
    pub total_return: Decimal,
    /// Total return as percentage
    pub total_return_pct: Decimal,
    /// Annualized return percentage
    pub annualized_return: Decimal,

    // Risk
    /// Maximum drawdown percentage
    pub max_drawdown: Decimal,
    /// Duration of maximum drawdown in hours
    pub max_drawdown_duration_hours: i64,
    /// Annualized volatility (std dev of returns)
    pub volatility: Decimal,

    // Risk-adjusted
    /// Sharpe ratio (assuming 0 risk-free rate)
    pub sharpe_ratio: Decimal,
    /// Sortino ratio (downside deviation only)
    pub sortino_ratio: Decimal,
    /// Calmar ratio (return / max drawdown)
    pub calmar_ratio: Decimal,

    // Strategy-specific
    /// Total funding fees received
    pub total_funding_received: Decimal,
    /// Total trading fees paid
    pub total_trading_fees: Decimal,
    /// Total margin interest paid
    pub total_interest_paid: Decimal,
    /// Net funding yield (funding - costs)
    pub net_funding_yield: Decimal,
    /// Ratio of funding to costs
    pub funding_to_cost_ratio: Decimal,

    // Activity
    /// Total number of trades
    pub total_trades: u64,
    /// Number of positions opened
    pub positions_opened: u64,
    /// Number of positions closed
    pub positions_closed: u64,
    /// Average position duration in hours
    pub avg_position_duration_hours: f64,
    /// Win rate (profitable positions / total)
    pub win_rate: Decimal,

    // Time
    /// Backtest duration in days
    pub duration_days: f64,
}

impl BacktestMetrics {
    /// Calculate metrics from equity curve and trading state.
    pub fn calculate(
        equity_curve: &[EquityPoint],
        initial_balance: Decimal,
        total_funding: Decimal,
        total_fees: Decimal,
        total_interest: Decimal,
        positions_opened: u64,
        positions_closed: u64,
        winning_positions: u64,
        total_position_hours: f64,
    ) -> Self {
        if equity_curve.is_empty() {
            return Self::empty();
        }

        let first = &equity_curve[0];
        let last = &equity_curve[equity_curve.len() - 1];

        // Duration
        let duration = last.timestamp - first.timestamp;
        let duration_days = duration.num_seconds() as f64 / 86400.0;
        let duration_years = duration_days / 365.0;

        // Returns
        let total_return = last.total_equity - initial_balance;
        let total_return_pct = if initial_balance > Decimal::ZERO {
            total_return / initial_balance * dec!(100)
        } else {
            Decimal::ZERO
        };

        let annualized_return = if duration_years > 0.0 {
            let factor = 1.0 + total_return_pct.to_string().parse::<f64>().unwrap_or(0.0) / 100.0;
            let annualized = factor.powf(1.0 / duration_years) - 1.0;
            Decimal::from_f64_retain(annualized * 100.0).unwrap_or(Decimal::ZERO)
        } else {
            Decimal::ZERO
        };

        // Drawdown
        let (max_drawdown, max_dd_duration) = calculate_max_drawdown(equity_curve);

        // Returns for volatility calculation
        let returns = calculate_period_returns(equity_curve);
        let volatility = calculate_volatility(&returns, duration_years);

        // Risk-adjusted metrics
        let sharpe_ratio = calculate_sharpe(&returns, duration_years);
        let sortino_ratio = calculate_sortino(&returns, duration_years);
        let calmar_ratio = if max_drawdown > Decimal::ZERO {
            annualized_return / (max_drawdown * dec!(100))
        } else {
            Decimal::ZERO
        };

        // Strategy-specific
        let net_funding_yield = total_funding - total_fees - total_interest;
        let total_costs = total_fees + total_interest;
        let funding_to_cost_ratio = if total_costs > Decimal::ZERO {
            total_funding / total_costs
        } else {
            Decimal::ZERO
        };

        // Activity
        let total_trades = positions_opened + positions_closed;
        let avg_position_duration_hours = if positions_closed > 0 {
            total_position_hours / positions_closed as f64
        } else {
            0.0
        };

        let win_rate = if positions_closed > 0 {
            Decimal::from(winning_positions) / Decimal::from(positions_closed) * dec!(100)
        } else {
            Decimal::ZERO
        };

        Self {
            total_return,
            total_return_pct,
            annualized_return,
            max_drawdown,
            max_drawdown_duration_hours: max_dd_duration,
            volatility,
            sharpe_ratio,
            sortino_ratio,
            calmar_ratio,
            total_funding_received: total_funding,
            total_trading_fees: total_fees,
            total_interest_paid: total_interest,
            net_funding_yield,
            funding_to_cost_ratio,
            total_trades,
            positions_opened,
            positions_closed,
            avg_position_duration_hours,
            win_rate,
            duration_days,
        }
    }

    /// Create empty metrics (for error cases).
    pub fn empty() -> Self {
        Self {
            total_return: Decimal::ZERO,
            total_return_pct: Decimal::ZERO,
            annualized_return: Decimal::ZERO,
            max_drawdown: Decimal::ZERO,
            max_drawdown_duration_hours: 0,
            volatility: Decimal::ZERO,
            sharpe_ratio: Decimal::ZERO,
            sortino_ratio: Decimal::ZERO,
            calmar_ratio: Decimal::ZERO,
            total_funding_received: Decimal::ZERO,
            total_trading_fees: Decimal::ZERO,
            total_interest_paid: Decimal::ZERO,
            net_funding_yield: Decimal::ZERO,
            funding_to_cost_ratio: Decimal::ZERO,
            total_trades: 0,
            positions_opened: 0,
            positions_closed: 0,
            avg_position_duration_hours: 0.0,
            win_rate: Decimal::ZERO,
            duration_days: 0.0,
        }
    }

    /// Format metrics as a summary string.
    pub fn summary(&self) -> String {
        format!(
            r#"═══════════════════════════════════════════════
BACKTEST RESULTS ({:.1} days)
═══════════════════════════════════════════════
RETURNS
  Total Return:      ${:.2} ({:.2}%)
  Annualized:        {:.2}%

RISK
  Max Drawdown:      {:.2}%
  Volatility:        {:.2}%

RISK-ADJUSTED
  Sharpe Ratio:      {:.3}
  Sortino Ratio:     {:.3}
  Calmar Ratio:      {:.3}

FUNDING
  Funding Received:  ${:.2}
  Trading Fees:      ${:.2}
  Interest Paid:     ${:.2}
  Net Yield:         ${:.2}

ACTIVITY
  Total Trades:      {}
  Positions Opened:  {}
  Positions Closed:  {}
  Win Rate:          {:.1}%
═══════════════════════════════════════════════"#,
            self.duration_days,
            self.total_return,
            self.total_return_pct,
            self.annualized_return,
            self.max_drawdown * dec!(100),
            self.volatility * dec!(100),
            self.sharpe_ratio,
            self.sortino_ratio,
            self.calmar_ratio,
            self.total_funding_received,
            self.total_trading_fees,
            self.total_interest_paid,
            self.net_funding_yield,
            self.total_trades,
            self.positions_opened,
            self.positions_closed,
            self.win_rate,
        )
    }
}

/// Calculate period returns from equity curve.
fn calculate_period_returns(equity_curve: &[EquityPoint]) -> Vec<Decimal> {
    if equity_curve.len() < 2 {
        return vec![];
    }

    equity_curve
        .windows(2)
        .map(|w| {
            let prev = &w[0];
            let curr = &w[1];
            if prev.total_equity > Decimal::ZERO {
                (curr.total_equity - prev.total_equity) / prev.total_equity
            } else {
                Decimal::ZERO
            }
        })
        .collect()
}

/// Calculate maximum drawdown and its duration.
fn calculate_max_drawdown(equity_curve: &[EquityPoint]) -> (Decimal, i64) {
    if equity_curve.is_empty() {
        return (Decimal::ZERO, 0);
    }

    let mut peak = equity_curve[0].total_equity;
    let mut max_dd = Decimal::ZERO;
    let mut max_dd_start: Option<DateTime<Utc>> = None;
    let mut max_dd_duration: i64 = 0;
    let mut current_dd_start: Option<DateTime<Utc>> = None;

    for point in equity_curve {
        if point.total_equity > peak {
            peak = point.total_equity;
            current_dd_start = None;
        } else {
            let dd = (peak - point.total_equity) / peak;
            if dd > max_dd {
                max_dd = dd;
                if current_dd_start.is_none() {
                    current_dd_start = Some(point.timestamp);
                }
                max_dd_start = current_dd_start;
            }
        }

        if let Some(start) = max_dd_start {
            let duration = (point.timestamp - start).num_hours();
            if duration > max_dd_duration {
                max_dd_duration = duration;
            }
        }
    }

    (max_dd, max_dd_duration)
}

/// Calculate annualized volatility from returns.
fn calculate_volatility(returns: &[Decimal], duration_years: f64) -> Decimal {
    if returns.len() < 2 || duration_years <= 0.0 {
        return Decimal::ZERO;
    }

    let n = returns.len() as f64;
    let mean: f64 = returns
        .iter()
        .map(|r| r.to_string().parse::<f64>().unwrap_or(0.0))
        .sum::<f64>()
        / n;

    let variance: f64 = returns
        .iter()
        .map(|r| {
            let r_f64 = r.to_string().parse::<f64>().unwrap_or(0.0);
            (r_f64 - mean).powi(2)
        })
        .sum::<f64>()
        / n;

    let std_dev = variance.sqrt();

    // Annualize: multiply by sqrt(periods_per_year)
    // Assuming hourly data, ~8760 periods per year
    let periods_per_year = n / duration_years;
    let annualized = std_dev * periods_per_year.sqrt();

    Decimal::from_f64_retain(annualized).unwrap_or(Decimal::ZERO)
}

/// Calculate Sharpe ratio (assuming 0 risk-free rate).
fn calculate_sharpe(returns: &[Decimal], duration_years: f64) -> Decimal {
    if returns.is_empty() || duration_years <= 0.0 {
        return Decimal::ZERO;
    }

    let returns_f64: Vec<f64> = returns
        .iter()
        .map(|r| r.to_string().parse::<f64>().unwrap_or(0.0))
        .collect();

    let n = returns_f64.len() as f64;
    let mean = returns_f64.iter().sum::<f64>() / n;
    let variance = returns_f64.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
    let std_dev = variance.sqrt();

    if std_dev < 1e-10 {
        return Decimal::ZERO;
    }

    // Annualize
    let periods_per_year = n / duration_years;
    let annualized_return = mean * periods_per_year;
    let annualized_std = std_dev * periods_per_year.sqrt();

    let sharpe = annualized_return / annualized_std;
    Decimal::from_f64_retain(sharpe).unwrap_or(Decimal::ZERO)
}

/// Calculate Sortino ratio (downside deviation only).
fn calculate_sortino(returns: &[Decimal], duration_years: f64) -> Decimal {
    if returns.is_empty() || duration_years <= 0.0 {
        return Decimal::ZERO;
    }

    let returns_f64: Vec<f64> = returns
        .iter()
        .map(|r| r.to_string().parse::<f64>().unwrap_or(0.0))
        .collect();

    let n = returns_f64.len() as f64;
    let mean = returns_f64.iter().sum::<f64>() / n;

    // Downside deviation (negative returns only)
    let downside: Vec<f64> = returns_f64.iter().filter(|&&r| r < 0.0).cloned().collect();

    if downside.is_empty() {
        // No negative returns = infinite Sortino (cap at a large value)
        return dec!(100);
    }

    let downside_variance = downside.iter().map(|r| r.powi(2)).sum::<f64>() / downside.len() as f64;
    let downside_deviation = downside_variance.sqrt();

    if downside_deviation < 1e-10 {
        return dec!(100);
    }

    // Annualize
    let periods_per_year = n / duration_years;
    let annualized_return = mean * periods_per_year;
    let annualized_dd = downside_deviation * periods_per_year.sqrt();

    let sortino = annualized_return / annualized_dd;
    Decimal::from_f64_retain(sortino).unwrap_or(Decimal::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_equity_point_drawdown() {
        let point = EquityPoint::new(
            Utc::now(),
            dec!(9500),    // balance
            dec!(0),       // unrealized
            2,             // positions
            dec!(10000),   // peak
        );

        assert_eq!(point.total_equity, dec!(9500));
        assert_eq!(point.drawdown, dec!(0.05)); // 5% drawdown
    }

    #[test]
    fn test_max_drawdown_calculation() {
        let curve = vec![
            EquityPoint::new(
                Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                dec!(10000), dec!(0), 0, dec!(10000),
            ),
            EquityPoint::new(
                Utc.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).unwrap(),
                dec!(10500), dec!(0), 1, dec!(10500),
            ),
            EquityPoint::new(
                Utc.with_ymd_and_hms(2024, 1, 3, 0, 0, 0).unwrap(),
                dec!(9500), dec!(0), 1, dec!(10500),
            ),
            EquityPoint::new(
                Utc.with_ymd_and_hms(2024, 1, 4, 0, 0, 0).unwrap(),
                dec!(11000), dec!(0), 1, dec!(11000),
            ),
        ];

        let (max_dd, _duration) = calculate_max_drawdown(&curve);
        // Max DD was from 10500 to 9500 = 9.52%
        assert!(max_dd > dec!(0.09) && max_dd < dec!(0.10));
    }

    #[test]
    fn test_period_returns() {
        let curve = vec![
            EquityPoint::new(Utc::now(), dec!(10000), dec!(0), 0, dec!(10000)),
            EquityPoint::new(Utc::now(), dec!(10100), dec!(0), 0, dec!(10100)),
            EquityPoint::new(Utc::now(), dec!(10000), dec!(0), 0, dec!(10100)),
        ];

        let returns = calculate_period_returns(&curve);
        assert_eq!(returns.len(), 2);
        assert_eq!(returns[0], dec!(0.01)); // +1%
        // returns[1] ≈ -0.99%
    }

    #[test]
    fn test_metrics_summary() {
        let metrics = BacktestMetrics {
            total_return: dec!(500),
            total_return_pct: dec!(5),
            annualized_return: dec!(20),
            max_drawdown: dec!(0.02),
            max_drawdown_duration_hours: 24,
            volatility: dec!(0.15),
            sharpe_ratio: dec!(1.5),
            sortino_ratio: dec!(2.0),
            calmar_ratio: dec!(10),
            total_funding_received: dec!(600),
            total_trading_fees: dec!(50),
            total_interest_paid: dec!(50),
            net_funding_yield: dec!(500),
            funding_to_cost_ratio: dec!(6),
            total_trades: 20,
            positions_opened: 10,
            positions_closed: 10,
            avg_position_duration_hours: 168.0,
            win_rate: dec!(70),
            duration_days: 90.0,
        };

        let summary = metrics.summary();
        assert!(summary.contains("500.00"));
        assert!(summary.contains("Sharpe"));
    }
}
