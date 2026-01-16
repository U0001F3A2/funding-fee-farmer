//! Parameter sweep runner for backtesting optimization.
//!
//! Allows testing multiple config combinations in parallel.

use crate::backtest::{BacktestConfig, BacktestEngine, BacktestResult, DataLoader};
use crate::config::Config;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};

/// Defines the parameter space to explore during optimization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterSpace {
    // Pair selection parameters
    pub min_funding_rate: Vec<Decimal>,
    pub min_volume_24h: Vec<Decimal>,
    pub max_spread: Vec<Decimal>,

    // Capital allocation parameters
    pub max_utilization: Vec<Decimal>,
    pub max_single_position: Vec<Decimal>,

    // Execution parameters
    pub default_leverage: Vec<u8>,

    // Risk parameters
    pub max_drawdown: Vec<Decimal>,
}

impl Default for ParameterSpace {
    fn default() -> Self {
        Self {
            min_funding_rate: vec![dec!(0.0001), dec!(0.0002), dec!(0.0003)],
            min_volume_24h: vec![dec!(50_000_000), dec!(100_000_000), dec!(150_000_000)],
            max_spread: vec![dec!(0.0002), dec!(0.0003)],
            max_utilization: vec![dec!(0.7), dec!(0.8), dec!(0.9)],
            max_single_position: vec![dec!(0.2), dec!(0.3), dec!(0.4)],
            default_leverage: vec![3, 5, 7],
            max_drawdown: vec![dec!(0.03), dec!(0.05), dec!(0.07)],
        }
    }
}

impl ParameterSpace {
    /// Create a minimal parameter space for quick testing.
    pub fn minimal() -> Self {
        Self {
            min_funding_rate: vec![dec!(0.0001)],
            min_volume_24h: vec![dec!(100_000_000)],
            max_spread: vec![dec!(0.0002)],
            max_utilization: vec![dec!(0.85)],
            max_single_position: vec![dec!(0.3)],
            default_leverage: vec![5],
            max_drawdown: vec![dec!(0.05)],
        }
    }

    /// Count total number of combinations.
    pub fn combination_count(&self) -> usize {
        self.min_funding_rate.len()
            * self.min_volume_24h.len()
            * self.max_spread.len()
            * self.max_utilization.len()
            * self.max_single_position.len()
            * self.default_leverage.len()
            * self.max_drawdown.len()
    }

    /// Generate all config combinations.
    pub fn generate_configs(&self, base_config: &Config) -> Vec<Config> {
        let mut configs = Vec::with_capacity(self.combination_count());

        for &min_funding_rate in &self.min_funding_rate {
            for &min_volume_24h in &self.min_volume_24h {
                for &max_spread in &self.max_spread {
                    for &max_utilization in &self.max_utilization {
                        for &max_single_position in &self.max_single_position {
                            for &default_leverage in &self.default_leverage {
                                for &max_drawdown in &self.max_drawdown {
                                    let mut config = base_config.clone();

                                    config.pair_selection.min_funding_rate = min_funding_rate;
                                    config.pair_selection.min_volume_24h = min_volume_24h;
                                    config.pair_selection.max_spread = max_spread;

                                    config.capital.max_utilization = max_utilization;
                                    config.risk.max_single_position = max_single_position;

                                    config.execution.default_leverage = default_leverage;

                                    config.risk.max_drawdown = max_drawdown;

                                    configs.push(config);
                                }
                            }
                        }
                    }
                }
            }
        }

        configs
    }

    /// Describe a config's parameter values.
    pub fn describe_config(config: &Config) -> String {
        format!(
            "funding≥{:.4}% vol≥${}M spread≤{:.2}% util={:.0}% maxpos={:.0}% lev={}x mdd={:.0}%",
            config.pair_selection.min_funding_rate * dec!(100),
            config.pair_selection.min_volume_24h / dec!(1_000_000),
            config.pair_selection.max_spread * dec!(100),
            config.capital.max_utilization * dec!(100),
            config.risk.max_single_position * dec!(100),
            config.execution.default_leverage,
            config.risk.max_drawdown * dec!(100),
        )
    }
}

/// Results from a parameter sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepResults {
    /// All individual run results
    pub runs: Vec<(Config, BacktestResult)>,

    /// Best config by Sharpe ratio
    pub best_by_sharpe: Option<usize>,

    /// Best config by total return
    pub best_by_return: Option<usize>,

    /// Best config by Calmar ratio (return/drawdown)
    pub best_by_calmar: Option<usize>,

    /// Total combinations tested
    pub total_combinations: usize,

    /// Successful runs
    pub successful_runs: usize,

    /// Failed runs
    pub failed_runs: usize,
}

impl SweepResults {
    /// Get the best result by Sharpe ratio.
    pub fn best_sharpe(&self) -> Option<&(Config, BacktestResult)> {
        self.best_by_sharpe.map(|i| &self.runs[i])
    }

    /// Get the best result by total return.
    pub fn best_return(&self) -> Option<&(Config, BacktestResult)> {
        self.best_by_return.map(|i| &self.runs[i])
    }

    /// Get the best result by Calmar ratio.
    pub fn best_calmar(&self) -> Option<&(Config, BacktestResult)> {
        self.best_by_calmar.map(|i| &self.runs[i])
    }

    /// Export results to CSV.
    pub fn to_csv(&self, path: &str) -> Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(path)?;

        // Header
        writeln!(
            file,
            "min_funding_rate,min_volume_24h,max_spread,max_utilization,max_single_position,leverage,max_drawdown,total_return_pct,sharpe_ratio,sortino_ratio,calmar_ratio,max_dd_pct,funding_received,net_yield"
        )?;

        // Data rows
        for (config, result) in &self.runs {
            writeln!(
                file,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                config.pair_selection.min_funding_rate,
                config.pair_selection.min_volume_24h,
                config.pair_selection.max_spread,
                config.capital.max_utilization,
                config.risk.max_single_position,
                config.execution.default_leverage,
                config.risk.max_drawdown,
                result.metrics.total_return_pct,
                result.metrics.sharpe_ratio,
                result.metrics.sortino_ratio,
                result.metrics.calmar_ratio,
                result.metrics.max_drawdown * dec!(100),
                result.metrics.total_funding_received,
                result.metrics.net_funding_yield,
            )?;
        }

        Ok(())
    }

    /// Generate a summary comparison table.
    pub fn summary(&self) -> String {
        let mut s = String::new();

        s.push_str("═══════════════════════════════════════════════════════════════\n");
        s.push_str("PARAMETER SWEEP RESULTS\n");
        s.push_str("═══════════════════════════════════════════════════════════════\n");
        s.push_str(&format!(
            "Total: {} | Successful: {} | Failed: {}\n\n",
            self.total_combinations, self.successful_runs, self.failed_runs
        ));

        if let Some((config, result)) = self.best_sharpe() {
            s.push_str("BEST BY SHARPE RATIO:\n");
            s.push_str(&format!("  Config: {}\n", ParameterSpace::describe_config(config)));
            s.push_str(&format!(
                "  Sharpe: {:.3} | Return: {:.2}% | MaxDD: {:.2}%\n\n",
                result.metrics.sharpe_ratio,
                result.metrics.total_return_pct,
                result.metrics.max_drawdown * dec!(100)
            ));
        }

        if let Some((config, result)) = self.best_return() {
            s.push_str("BEST BY RETURN:\n");
            s.push_str(&format!("  Config: {}\n", ParameterSpace::describe_config(config)));
            s.push_str(&format!(
                "  Return: {:.2}% | Sharpe: {:.3} | MaxDD: {:.2}%\n\n",
                result.metrics.total_return_pct,
                result.metrics.sharpe_ratio,
                result.metrics.max_drawdown * dec!(100)
            ));
        }

        if let Some((config, result)) = self.best_calmar() {
            s.push_str("BEST BY CALMAR RATIO:\n");
            s.push_str(&format!("  Config: {}\n", ParameterSpace::describe_config(config)));
            s.push_str(&format!(
                "  Calmar: {:.3} | Return: {:.2}% | MaxDD: {:.2}%\n",
                result.metrics.calmar_ratio,
                result.metrics.total_return_pct,
                result.metrics.max_drawdown * dec!(100)
            ));
        }

        s.push_str("═══════════════════════════════════════════════════════════════\n");

        s
    }
}

/// Parameter sweep runner for parallel backtesting.
pub struct SweepRunner {
    parameter_space: ParameterSpace,
    base_config: Config,
    backtest_config: BacktestConfig,
    parallelism: usize,
}

impl SweepRunner {
    /// Create a new sweep runner.
    pub fn new(
        parameter_space: ParameterSpace,
        base_config: Config,
        backtest_config: BacktestConfig,
        parallelism: usize,
    ) -> Self {
        Self {
            parameter_space,
            base_config,
            backtest_config,
            parallelism: parallelism.max(1),
        }
    }

    /// Run the parameter sweep.
    pub async fn run<D: DataLoader + Clone + Send + Sync + 'static>(
        &self,
        data_loader: D,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<SweepResults> {
        let configs = self.parameter_space.generate_configs(&self.base_config);
        let total_combinations = configs.len();

        info!(
            "Starting parameter sweep with {} combinations, parallelism={}",
            total_combinations, self.parallelism
        );

        let semaphore = Arc::new(Semaphore::new(self.parallelism));
        let data_loader = Arc::new(data_loader);
        let backtest_config = self.backtest_config.clone();

        let mut handles = Vec::with_capacity(configs.len());

        for (i, config) in configs.into_iter().enumerate() {
            let sem = semaphore.clone();
            let loader = data_loader.clone();
            let bt_config = backtest_config.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                info!(
                    "[{}/{}] Testing: {}",
                    i + 1,
                    total_combinations,
                    ParameterSpace::describe_config(&config)
                );

                // Create a new data loader instance for this run
                // This is needed because the loader may have internal state
                let loader_clone = (*loader).clone();

                let mut engine = BacktestEngine::new(loader_clone, config.clone(), bt_config);

                match engine.run(start, end).await {
                    Ok(result) => {
                        info!(
                            "[{}/{}] Complete: Sharpe={:.3} Return={:.2}%",
                            i + 1,
                            total_combinations,
                            result.metrics.sharpe_ratio,
                            result.metrics.total_return_pct
                        );
                        Some((config, result))
                    }
                    Err(e) => {
                        warn!("[{}/{}] Failed: {}", i + 1, total_combinations, e);
                        None
                    }
                }
            });

            handles.push(handle);
        }

        // Collect results
        let mut runs = Vec::new();
        let mut failed_runs = 0;

        for handle in handles {
            match handle.await {
                Ok(Some((config, result))) => runs.push((config, result)),
                Ok(None) => failed_runs += 1,
                Err(e) => {
                    warn!("Task panicked: {}", e);
                    failed_runs += 1;
                }
            }
        }

        // Find best results
        let best_by_sharpe = runs
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.1.metrics
                    .sharpe_ratio
                    .partial_cmp(&b.1.metrics.sharpe_ratio)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);

        let best_by_return = runs
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.1.metrics
                    .total_return_pct
                    .partial_cmp(&b.1.metrics.total_return_pct)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);

        let best_by_calmar = runs
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.1.metrics
                    .calmar_ratio
                    .partial_cmp(&b.1.metrics.calmar_ratio)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);

        Ok(SweepResults {
            runs,
            best_by_sharpe,
            best_by_return,
            best_by_calmar,
            total_combinations,
            successful_runs: total_combinations - failed_runs,
            failed_runs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_space_count() {
        let space = ParameterSpace::default();
        let count = space.combination_count();

        // 3 * 3 * 2 * 3 * 3 * 3 * 3 = 1458
        assert_eq!(count, 3 * 3 * 2 * 3 * 3 * 3 * 3);
    }

    #[test]
    fn test_minimal_space() {
        let space = ParameterSpace::minimal();
        assert_eq!(space.combination_count(), 1);
    }

    #[test]
    fn test_generate_configs() {
        let space = ParameterSpace {
            min_funding_rate: vec![dec!(0.0001), dec!(0.0002)],
            min_volume_24h: vec![dec!(100_000_000)],
            max_spread: vec![dec!(0.0002)],
            max_utilization: vec![dec!(0.85)],
            max_single_position: vec![dec!(0.3)],
            default_leverage: vec![5],
            max_drawdown: vec![dec!(0.05)],
        };

        let base = Config::default();
        let configs = space.generate_configs(&base);

        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].pair_selection.min_funding_rate, dec!(0.0001));
        assert_eq!(configs[1].pair_selection.min_funding_rate, dec!(0.0002));
    }

    #[test]
    fn test_describe_config() {
        let config = Config::default();
        let desc = ParameterSpace::describe_config(&config);

        assert!(desc.contains("funding"));
        assert!(desc.contains("vol"));
        assert!(desc.contains("lev"));
    }
}
