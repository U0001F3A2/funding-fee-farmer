//! Binance WebSocket client for real-time market data and user updates.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

const FUTURES_WS_URL: &str = "wss://fstream.binance.com";
const FUTURES_TESTNET_WS_URL: &str = "wss://stream.binancefuture.com";

/// WebSocket event types.
#[derive(Debug, Clone)]
pub enum WsEvent {
    /// Funding rate update
    FundingRate(FundingRateUpdate),
    /// Book ticker update (best bid/ask)
    BookTicker(BookTickerUpdate),
    /// Mark price update
    MarkPrice(MarkPriceUpdate),
    /// User account update
    AccountUpdate(AccountUpdateEvent),
    /// Order update
    OrderUpdate(OrderUpdateEvent),
    /// Connection established
    Connected,
    /// Connection lost
    Disconnected,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FundingRateUpdate {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "r")]
    pub funding_rate: String,
    #[serde(rename = "T")]
    pub funding_time: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookTickerUpdate {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "b")]
    pub bid_price: String,
    #[serde(rename = "B")]
    pub bid_qty: String,
    #[serde(rename = "a")]
    pub ask_price: String,
    #[serde(rename = "A")]
    pub ask_qty: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkPriceUpdate {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "p")]
    pub mark_price: String,
    #[serde(rename = "r")]
    pub funding_rate: String,
    #[serde(rename = "T")]
    pub next_funding_time: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountUpdateEvent {
    #[serde(rename = "a")]
    pub data: AccountUpdateData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountUpdateData {
    #[serde(rename = "B")]
    pub balances: Vec<BalanceUpdate>,
    #[serde(rename = "P")]
    pub positions: Vec<PositionUpdate>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BalanceUpdate {
    #[serde(rename = "a")]
    pub asset: String,
    #[serde(rename = "wb")]
    pub wallet_balance: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PositionUpdate {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "pa")]
    pub position_amount: String,
    #[serde(rename = "ep")]
    pub entry_price: String,
    #[serde(rename = "up")]
    pub unrealized_profit: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderUpdateEvent {
    #[serde(rename = "o")]
    pub order: OrderUpdate,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderUpdate {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "i")]
    pub order_id: i64,
    #[serde(rename = "X")]
    pub status: String,
    #[serde(rename = "S")]
    pub side: String,
    #[serde(rename = "o")]
    pub order_type: String,
    #[serde(rename = "q")]
    pub original_qty: String,
    #[serde(rename = "z")]
    pub filled_qty: String,
    #[serde(rename = "ap")]
    pub avg_price: String,
}

/// Binance WebSocket client.
pub struct BinanceWebSocket {
    base_url: String,
    #[allow(dead_code)] // Stored for potential reconnection logic
    testnet: bool,
}

impl BinanceWebSocket {
    /// Create a new WebSocket client.
    pub fn new(testnet: bool) -> Self {
        let base_url = if testnet {
            FUTURES_TESTNET_WS_URL.to_string()
        } else {
            FUTURES_WS_URL.to_string()
        };

        Self { base_url, testnet }
    }

    /// Subscribe to mark price stream for all symbols.
    pub async fn subscribe_mark_price_all(&self, tx: mpsc::Sender<WsEvent>) -> Result<()> {
        let url = format!("{}/ws/!markPrice@arr@1s", self.base_url);
        self.connect_and_handle(url, tx, |msg| {
            if let Ok(updates) = serde_json::from_str::<Vec<MarkPriceUpdate>>(&msg) {
                updates.into_iter().map(WsEvent::MarkPrice).collect()
            } else {
                vec![]
            }
        })
        .await
    }

    /// Subscribe to book ticker stream for specific symbols.
    pub async fn subscribe_book_tickers(
        &self,
        symbols: Vec<String>,
        tx: mpsc::Sender<WsEvent>,
    ) -> Result<()> {
        let streams: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@bookTicker", s.to_lowercase()))
            .collect();

        let url = format!("{}/stream?streams={}", self.base_url, streams.join("/"));

        self.connect_and_handle(url, tx, |msg| {
            #[derive(Deserialize)]
            struct StreamWrapper {
                data: BookTickerUpdate,
            }

            if let Ok(wrapper) = serde_json::from_str::<StreamWrapper>(&msg) {
                vec![WsEvent::BookTicker(wrapper.data)]
            } else {
                vec![]
            }
        })
        .await
    }

    /// Generic WebSocket connection handler.
    async fn connect_and_handle<F>(
        &self,
        url: String,
        tx: mpsc::Sender<WsEvent>,
        parser: F,
    ) -> Result<()>
    where
        F: Fn(String) -> Vec<WsEvent> + Send + 'static,
    {
        info!("Connecting to WebSocket: {}", url);

        let (ws_stream, _) = connect_async(&url)
            .await
            .context("Failed to connect to WebSocket")?;

        let (_write, mut read) = ws_stream.split();

        // Notify connection established
        let _ = tx.send(WsEvent::Connected).await;

        // Handle incoming messages
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        for event in parser(text.to_string()) {
                            if tx.send(event).await.is_err() {
                                warn!("Event receiver dropped");
                                return;
                            }
                        }
                    }
                    Ok(Message::Ping(_data)) => {
                        debug!("Received ping, sending pong");
                        // Pong is handled automatically by tungstenite
                    }
                    Ok(Message::Close(_)) => {
                        info!("WebSocket closed by server");
                        let _ = tx.send(WsEvent::Disconnected).await;
                        return;
                    }
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        let _ = tx.send(WsEvent::Disconnected).await;
                        return;
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }
}
