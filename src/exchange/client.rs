//! Binance REST API client.

use crate::config::BinanceConfig;
use crate::exchange::types::*;
use anyhow::{anyhow, Context, Result};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use tracing::{debug, instrument, warn};

/// Default retry configuration
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 100;
const BACKOFF_MULTIPLIER: u64 = 5; // 100ms -> 500ms -> 2500ms

/// Check if an HTTP status code is retryable
fn is_retryable_status(status: StatusCode) -> bool {
    // Retry on server errors (5xx) and rate limiting (429)
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

/// Check if an error is retryable (network errors, timeouts)
fn is_retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request()
}

const FUTURES_BASE_URL: &str = "https://fapi.binance.com";
const FUTURES_TESTNET_URL: &str = "https://testnet.binancefuture.com";
const SPOT_BASE_URL: &str = "https://api.binance.com";
const SPOT_TESTNET_URL: &str = "https://testnet.binance.vision";

/// Binance API client for both spot and futures markets.
pub struct BinanceClient {
    http: Client,
    api_key: String,
    secret_key: String,
    futures_base_url: String,
    spot_base_url: String,
}

impl BinanceClient {
    /// Create a new Binance client from configuration.
    pub fn new(config: &BinanceConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        let (futures_base_url, spot_base_url) = if config.testnet {
            (
                FUTURES_TESTNET_URL.to_string(),
                SPOT_TESTNET_URL.to_string(),
            )
        } else {
            (FUTURES_BASE_URL.to_string(), SPOT_BASE_URL.to_string())
        };

        Ok(Self {
            http,
            api_key: config.api_key.clone(),
            secret_key: config.secret_key.clone(),
            futures_base_url,
            spot_base_url,
        })
    }

    /// Generate HMAC-SHA256 signature for authenticated requests.
    fn sign(&self, query_string: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret_key.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(query_string.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Get current timestamp in milliseconds.
    fn timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64
    }

    /// Execute an HTTP request with retry and exponential backoff.
    ///
    /// Retries on:
    /// - 5xx server errors
    /// - 429 rate limit errors
    /// - Network timeouts and connection errors
    ///
    /// Does NOT retry on:
    /// - 4xx client errors (except 429)
    /// - Authentication errors
    /// - Validation errors
    async fn retry_with_backoff<F, Fut>(&self, operation: &str, request_fn: F) -> Result<Response>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<Response, reqwest::Error>>,
    {
        let mut backoff_ms = INITIAL_BACKOFF_MS;
        let mut last_error = None;

        for attempt in 1..=MAX_RETRIES {
            match request_fn().await {
                Ok(response) => {
                    let status = response.status();

                    // Success or non-retryable client error
                    if status.is_success()
                        || (status.is_client_error() && !is_retryable_status(status))
                    {
                        return Ok(response);
                    }

                    // Retryable status code
                    if is_retryable_status(status) && attempt < MAX_RETRIES {
                        warn!(
                            %operation,
                            attempt,
                            status = %status,
                            backoff_ms,
                            "Retryable HTTP status, backing off"
                        );
                        sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms *= BACKOFF_MULTIPLIER;
                        last_error = Some(anyhow!("HTTP {} for {}", status, operation));
                        continue;
                    }

                    // Non-retryable or exhausted retries
                    return Ok(response);
                }
                Err(e) => {
                    if is_retryable_error(&e) && attempt < MAX_RETRIES {
                        warn!(
                            %operation,
                            attempt,
                            error = %e,
                            backoff_ms,
                            "Retryable network error, backing off"
                        );
                        sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms *= BACKOFF_MULTIPLIER;
                        last_error = Some(anyhow!("Network error for {}: {}", operation, e));
                        continue;
                    }

                    // Non-retryable error or exhausted retries
                    return Err(anyhow!(
                        "{} failed after {} attempts: {}",
                        operation,
                        attempt,
                        e
                    ));
                }
            }
        }

        // Exhausted all retries
        Err(last_error
            .unwrap_or_else(|| anyhow!("{} failed after {} retries", operation, MAX_RETRIES)))
    }

    // ==================== Market Data (Public) ====================

    /// Get funding rates for all perpetual contracts.
    #[instrument(skip(self))]
    pub async fn get_funding_rates(&self) -> Result<Vec<FundingRate>> {
        let url = format!("{}/fapi/v1/premiumIndex", self.futures_base_url);
        let response = self
            .retry_with_backoff("get_funding_rates", || self.http.get(&url).send())
            .await?;

        response
            .json()
            .await
            .context("Failed to parse funding rates response")
    }

    /// Get 24-hour ticker for all symbols.
    #[instrument(skip(self))]
    pub async fn get_24h_tickers(&self) -> Result<Vec<Ticker24h>> {
        let url = format!("{}/fapi/v1/ticker/24hr", self.futures_base_url);
        let response = self
            .retry_with_backoff("get_24h_tickers", || self.http.get(&url).send())
            .await?;

        response
            .json()
            .await
            .context("Failed to parse 24h ticker response")
    }

    /// Get 24-hour ticker for all spot symbols.
    #[instrument(skip(self))]
    pub async fn get_spot_24h_tickers(&self) -> Result<Vec<Ticker24h>> {
        let url = format!("{}/api/v3/ticker/24hr", self.spot_base_url);
        let response = self
            .retry_with_backoff("get_spot_24h_tickers", || self.http.get(&url).send())
            .await?;

        response
            .json()
            .await
            .context("Failed to parse spot 24h ticker response")
    }

    /// Get best bid/ask for all symbols.
    #[instrument(skip(self))]
    pub async fn get_book_tickers(&self) -> Result<Vec<BookTicker>> {
        let url = format!("{}/fapi/v1/ticker/bookTicker", self.futures_base_url);
        let response = self
            .retry_with_backoff("get_book_tickers", || self.http.get(&url).send())
            .await?;

        response
            .json()
            .await
            .context("Failed to parse book ticker response")
    }

    /// Get open interest for a specific symbol.
    #[instrument(skip(self))]
    pub async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterest> {
        let url = format!(
            "{}/fapi/v1/openInterest?symbol={}",
            self.futures_base_url, symbol
        );
        let response = self
            .retry_with_backoff("get_open_interest", || self.http.get(&url).send())
            .await?;

        response
            .json()
            .await
            .context("Failed to parse open interest response")
    }

    /// Get futures exchange info (for precision and rules).
    #[instrument(skip(self))]
    pub async fn get_futures_exchange_info(&self) -> Result<FuturesExchangeInfo> {
        let url = format!("{}/fapi/v1/exchangeInfo", self.futures_base_url);
        let response = self
            .retry_with_backoff("get_futures_exchange_info", || self.http.get(&url).send())
            .await?;

        response
            .json()
            .await
            .context("Failed to parse futures exchange info")
    }

    /// Get leverage brackets for all symbols (maintenance margin rates).
    #[instrument(skip(self))]
    pub async fn get_leverage_brackets(&self) -> Result<Vec<LeverageBracket>> {
        let timestamp = Self::timestamp();
        let query = format!("timestamp={}", timestamp);
        let signature = self.sign(&query);

        let url = format!(
            "{}/fapi/v1/leverageBracket?{}&signature={}",
            self.futures_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("get_leverage_brackets", || {
                self.http
                    .get(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        response
            .json()
            .await
            .context("Failed to parse leverage brackets response")
    }

    // ==================== Account (Authenticated) ====================

    /// Get account balance information.
    #[instrument(skip(self))]
    pub async fn get_account_balance(&self) -> Result<Vec<AccountBalance>> {
        let timestamp = Self::timestamp();
        let query = format!("timestamp={}", timestamp);
        let signature = self.sign(&query);

        let url = format!(
            "{}/fapi/v2/balance?{}&signature={}",
            self.futures_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("get_account_balance", || {
                self.http
                    .get(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        response
            .json()
            .await
            .context("Failed to parse account balance response")
    }

    /// Get current positions.
    #[instrument(skip(self))]
    pub async fn get_positions(&self) -> Result<Vec<Position>> {
        let timestamp = Self::timestamp();
        let query = format!("timestamp={}", timestamp);
        let signature = self.sign(&query);

        let url = format!(
            "{}/fapi/v2/positionRisk?{}&signature={}",
            self.futures_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("get_positions", || {
                self.http
                    .get(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        response
            .json()
            .await
            .context("Failed to parse positions response")
    }

    // ==================== Orders (Authenticated) ====================

    /// Place a new futures order.
    #[instrument(skip(self))]
    pub async fn place_futures_order(&self, order: &NewOrder) -> Result<OrderResponse> {
        let timestamp = Self::timestamp();
        let mut params = vec![
            ("symbol".to_string(), order.symbol.clone()),
            (
                "side".to_string(),
                format!("{:?}", order.side).to_uppercase(),
            ),
            (
                "type".to_string(),
                format!("{:?}", order.order_type).to_uppercase(),
            ),
            ("timestamp".to_string(), timestamp.to_string()),
        ];

        if let Some(qty) = &order.quantity {
            params.push(("quantity".to_string(), qty.to_string()));
        }

        if let Some(price) = &order.price {
            params.push(("price".to_string(), price.to_string()));
        }

        if let Some(tif) = &order.time_in_force {
            params.push((
                "timeInForce".to_string(),
                format!("{:?}", tif).to_uppercase(),
            ));
        }

        if let Some(reduce_only) = order.reduce_only {
            params.push(("reduceOnly".to_string(), reduce_only.to_string()));
        }

        if let Some(client_id) = &order.new_client_order_id {
            params.push(("newClientOrderId".to_string(), client_id.clone()));
        }

        let query_string: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        let signature = self.sign(&query_string);
        let url = format!(
            "{}/fapi/v1/order?{}&signature={}",
            self.futures_base_url, query_string, signature
        );

        debug!("Placing futures order: {:?}", order);

        let response = self
            .retry_with_backoff("place_futures_order", || {
                self.http
                    .post(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        response
            .json()
            .await
            .context("Failed to parse order response")
    }

    /// Cancel a futures order.
    #[instrument(skip(self))]
    pub async fn cancel_futures_order(&self, symbol: &str, order_id: i64) -> Result<OrderResponse> {
        let timestamp = Self::timestamp();
        let query = format!(
            "symbol={}&orderId={}&timestamp={}",
            symbol, order_id, timestamp
        );
        let signature = self.sign(&query);

        let url = format!(
            "{}/fapi/v1/order?{}&signature={}",
            self.futures_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("cancel_futures_order", || {
                self.http
                    .delete(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        response
            .json()
            .await
            .context("Failed to parse cancel response")
    }

    /// Set leverage for a symbol.
    #[instrument(skip(self))]
    pub async fn set_leverage(&self, symbol: &str, leverage: u8) -> Result<()> {
        let timestamp = Self::timestamp();
        let query = format!(
            "symbol={}&leverage={}&timestamp={}",
            symbol, leverage, timestamp
        );
        let signature = self.sign(&query);

        let url = format!(
            "{}/fapi/v1/leverage?{}&signature={}",
            self.futures_base_url, query, signature
        );

        self.retry_with_backoff("set_leverage", || {
            self.http
                .post(&url)
                .header("X-MBX-APIKEY", &self.api_key)
                .send()
        })
        .await?;

        Ok(())
    }

    /// Set margin type (isolated or cross) for a symbol.
    #[instrument(skip(self))]
    pub async fn set_margin_type(&self, symbol: &str, margin_type: MarginType) -> Result<()> {
        let timestamp = Self::timestamp();
        let margin_type_str = match margin_type {
            MarginType::Isolated => "ISOLATED",
            MarginType::Cross => "CROSSED",
        };
        let query = format!(
            "symbol={}&marginType={}&timestamp={}",
            symbol, margin_type_str, timestamp
        );
        let signature = self.sign(&query);

        let url = format!(
            "{}/fapi/v1/marginType?{}&signature={}",
            self.futures_base_url, query, signature
        );

        // This endpoint returns an error if margin type is already set
        // We ignore that specific error - no retry needed
        let _ = self
            .http
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await;

        Ok(())
    }

    // ==================== Spot Margin (Authenticated) ====================

    /// Get spot exchange info to check which pairs support margin trading.
    #[instrument(skip(self))]
    pub async fn get_spot_exchange_info(&self) -> Result<Vec<SpotSymbolInfo>> {
        let url = format!("{}/api/v3/exchangeInfo", self.spot_base_url);
        let response = self
            .retry_with_backoff("get_spot_exchange_info", || self.http.get(&url).send())
            .await?;

        #[derive(Deserialize)]
        struct ExchangeInfo {
            symbols: Vec<SpotSymbolInfo>,
        }

        let info: ExchangeInfo = response
            .json()
            .await
            .context("Failed to parse spot exchange info")?;

        Ok(info.symbols)
    }

    /// Get all margin assets and their borrowability.
    /// This endpoint requires signature authentication.
    #[instrument(skip(self))]
    pub async fn get_margin_all_assets(&self) -> Result<Vec<MarginAsset>> {
        let timestamp = Self::timestamp();
        let query = format!("timestamp={}", timestamp);
        let signature = self.sign(&query);

        let url = format!(
            "{}/sapi/v1/margin/allAssets?{}&signature={}",
            self.spot_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("get_margin_all_assets", || {
                self.http
                    .get(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        // Check for error response before parsing
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Margin assets API returned error status {}: {}",
                status,
                error_text
            );
        }

        response
            .json()
            .await
            .context("Failed to parse margin assets response")
    }

    /// Get cross margin account details.
    #[instrument(skip(self))]
    pub async fn get_cross_margin_account(&self) -> Result<CrossMarginAccount> {
        let timestamp = Self::timestamp();
        let query = format!("timestamp={}", timestamp);
        let signature = self.sign(&query);

        let url = format!(
            "{}/sapi/v1/margin/account?{}&signature={}",
            self.spot_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("get_cross_margin_account", || {
                self.http
                    .get(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        response
            .json()
            .await
            .context("Failed to parse cross margin account response")
    }

    /// Borrow an asset in cross margin.
    #[instrument(skip(self))]
    pub async fn margin_borrow(&self, asset: &str, amount: rust_decimal::Decimal) -> Result<()> {
        let timestamp = Self::timestamp();
        let query = format!("asset={}&amount={}&timestamp={}", asset, amount, timestamp);
        let signature = self.sign(&query);

        let url = format!(
            "{}/sapi/v1/margin/loan?{}&signature={}",
            self.spot_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("margin_borrow", || {
                self.http
                    .post(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Margin borrow failed: {}", error_text);
        }

        Ok(())
    }

    /// Repay borrowed asset in cross margin.
    #[instrument(skip(self))]
    pub async fn margin_repay(&self, asset: &str, amount: rust_decimal::Decimal) -> Result<()> {
        let timestamp = Self::timestamp();
        let query = format!("asset={}&amount={}&timestamp={}", asset, amount, timestamp);
        let signature = self.sign(&query);

        let url = format!(
            "{}/sapi/v1/margin/repay?{}&signature={}",
            self.spot_base_url, query, signature
        );

        let response = self
            .retry_with_backoff("margin_repay", || {
                self.http
                    .post(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Margin repay failed: {}", error_text);
        }

        Ok(())
    }

    /// Place a cross margin order.
    #[instrument(skip(self))]
    pub async fn place_margin_order(&self, order: &MarginOrder) -> Result<OrderResponse> {
        let timestamp = Self::timestamp();
        let mut params = vec![
            ("symbol".to_string(), order.symbol.clone()),
            (
                "side".to_string(),
                format!("{:?}", order.side).to_uppercase(),
            ),
            (
                "type".to_string(),
                format!("{:?}", order.order_type).to_uppercase(),
            ),
            ("timestamp".to_string(), timestamp.to_string()),
        ];

        if let Some(qty) = &order.quantity {
            params.push(("quantity".to_string(), qty.to_string()));
        }

        if let Some(price) = &order.price {
            params.push(("price".to_string(), price.to_string()));
        }

        if let Some(tif) = &order.time_in_force {
            params.push((
                "timeInForce".to_string(),
                format!("{:?}", tif).to_uppercase(),
            ));
        }

        if let Some(side_effect) = &order.side_effect_type {
            params.push((
                "sideEffectType".to_string(),
                format!("{:?}", side_effect).to_uppercase(),
            ));
        }

        let query_string: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        let signature = self.sign(&query_string);
        let url = format!(
            "{}/sapi/v1/margin/order?{}&signature={}",
            self.spot_base_url, query_string, signature
        );

        debug!("Placing margin order: {:?}", order);

        let response = self
            .retry_with_backoff("place_margin_order", || {
                self.http
                    .post(&url)
                    .header("X-MBX-APIKEY", &self.api_key)
                    .send()
            })
            .await?;

        response
            .json()
            .await
            .context("Failed to parse margin order response")
    }

    /// Get spot price for a symbol.
    #[instrument(skip(self))]
    pub async fn get_spot_price(&self, symbol: &str) -> Result<rust_decimal::Decimal> {
        let url = format!(
            "{}/api/v3/ticker/price?symbol={}",
            self.spot_base_url, symbol
        );

        #[derive(Deserialize)]
        struct PriceTicker {
            #[serde(with = "rust_decimal::serde::str")]
            price: rust_decimal::Decimal,
        }

        let response = self
            .retry_with_backoff("get_spot_price", || self.http.get(&url).send())
            .await?;

        let ticker: PriceTicker = response
            .json()
            .await
            .context("Failed to parse spot price response")?;

        Ok(ticker.price)
    }
}
