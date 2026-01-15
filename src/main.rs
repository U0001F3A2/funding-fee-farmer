use anyhow::Result;
use funding_fee_farmer::Config;
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive(Level::INFO.into()),
        )
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .init();

    info!("Starting Funding Fee Farmer v{}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config = Config::load()?;
    info!(
        capital_utilization = %config.capital.max_utilization,
        max_drawdown = %config.risk.max_drawdown,
        "Configuration loaded"
    );

    // TODO: Initialize exchange client
    // TODO: Start market scanner
    // TODO: Initialize risk manager
    // TODO: Begin main trading loop

    info!("Funding Fee Farmer initialized successfully");

    // Keep the application running
    tokio::signal::ctrl_c().await?;
    info!("Shutdown signal received, exiting...");

    Ok(())
}
