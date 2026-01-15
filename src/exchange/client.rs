//! Binance REST API client.

use crate::config::BinanceConfig;
use crate::exchange::types::*;
use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, instrument};

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
            (FUTURES_TESTNET_URL.to_string(), SPOT_TESTNET_URL.to_string())
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

    // ==================== Market Data (Public) ====================

    /// Get funding rates for all perpetual contracts.
    #[instrument(skip(self))]
    pub async fn get_funding_rates(&self) -> Result<Vec<FundingRate>> {
        let url = format!("{}/fapi/v1/premiumIndex", self.futures_base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch funding rates")?;

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
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch 24h tickers")?;

        response
            .json()
            .await
            .context("Failed to parse 24h ticker response")
    }

    /// Get best bid/ask for all symbols.
    #[instrument(skip(self))]
    pub async fn get_book_tickers(&self) -> Result<Vec<BookTicker>> {
        let url = format!("{}/fapi/v1/ticker/bookTicker", self.futures_base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch book tickers")?;

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
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch open interest")?;

        response
            .json()
            .await
            .context("Failed to parse open interest response")
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
            .http
            .get(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .context("Failed to fetch account balance")?;

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
            .http
            .get(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .context("Failed to fetch positions")?;

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
            ("side".to_string(), format!("{:?}", order.side).to_uppercase()),
            ("type".to_string(), format!("{:?}", order.order_type).to_uppercase()),
            ("timestamp".to_string(), timestamp.to_string()),
        ];

        if let Some(qty) = &order.quantity {
            params.push(("quantity".to_string(), qty.to_string()));
        }

        if let Some(price) = &order.price {
            params.push(("price".to_string(), price.to_string()));
        }

        if let Some(tif) = &order.time_in_force {
            params.push(("timeInForce".to_string(), format!("{:?}", tif).to_uppercase()));
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
            .http
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .context("Failed to place futures order")?;

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
            .http
            .delete(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .context("Failed to cancel futures order")?;

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

        self.http
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .context("Failed to set leverage")?;

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
        // We ignore that specific error
        let _ = self
            .http
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await;

        Ok(())
    }
}
