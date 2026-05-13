//! # Angel One WebSocket v2 Streaming Client
//!
//! Connects to `wss://smartapisocket.angelone.in/smart-stream`, subscribes
//! to instrument tokens, parses the binary stream into [`common::Tick`] structs,
//! and pushes them into a `tokio::sync::mpsc` channel for downstream consumption.

use std::collections::HashMap;

use crate::auth::AuthTokens;
use common::{parse_binary_packet, Tick};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

const WS_URL: &str = "wss://smartapisocket.angelone.in/smart-stream";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RECONNECT_ATTEMPTS: u32 = 5;
const RECONNECT_BASE_DELAY: Duration = Duration::from_secs(2);

/// One exchange bucket: all tokens sharing the same exchange_type.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    /// Angel One exchange_type (1=NSE_CM, 2=NSE_FO).
    pub exchange_type: u8,
    /// Angel One token strings for this exchange.
    pub tokens: Vec<String>,
}

/// Configuration for which instruments to subscribe to.
#[derive(Debug, Clone)]
pub struct SubscriptionConfig {
    /// Tokens grouped by exchange type — each bucket becomes one tokenList entry.
    pub buckets: Vec<TokenBucket>,
    /// Subscription mode (1=LTP, 2=Quote, 3=SnapQuote).
    pub mode: u8,
}

// Minimal serde structs to read config/trading.toml
#[derive(Debug, Deserialize)]
struct TomlConfig {
    instruments: Vec<TomlInstrument>,
}

#[derive(Debug, Deserialize)]
struct TomlInstrument {
    token: u32,
    exchange: String,
}

/// Load subscription config from `config/trading.toml`.
/// Falls back to NIFTY 50 only if the file cannot be read.
pub fn load_subscription_config() -> SubscriptionConfig {
    let config_text = match std::fs::read_to_string("config/trading.toml") {
        Ok(s) => s,
        Err(e) => {
            warn!("Cannot read config/trading.toml: {e}. Falling back to NIFTY 50 only.");
            return SubscriptionConfig {
                buckets: vec![TokenBucket { exchange_type: 1, tokens: vec!["26009".to_string()] }],
                mode: 3,
            };
        }
    };

    let cfg: TomlConfig = match toml::from_str(&config_text) {
        Ok(c) => c,
        Err(e) => {
            warn!("Cannot parse config/trading.toml: {e}. Falling back to NIFTY 50 only.");
            return SubscriptionConfig {
                buckets: vec![TokenBucket { exchange_type: 1, tokens: vec!["26009".to_string()] }],
                mode: 3,
            };
        }
    };

    // Group tokens by exchange_type: NFO=2, everything else (NSE/BSE equities)=1.
    let mut groups: HashMap<u8, Vec<String>> = HashMap::new();
    for inst in &cfg.instruments {
        let exchange_type: u8 = if inst.exchange == "NFO" { 2 } else { 1 };
        groups
            .entry(exchange_type)
            .or_default()
            .push(inst.token.to_string());
    }

    let buckets: Vec<TokenBucket> = groups
        .into_iter()
        .map(|(exchange_type, tokens)| {
            info!("Subscribing {} tokens on exchange_type={exchange_type}: {:?}", tokens.len(), tokens);
            TokenBucket { exchange_type, tokens }
        })
        .collect();

    info!("Loaded {} instrument(s) from config/trading.toml", cfg.instruments.len());

    SubscriptionConfig { buckets, mode: 3 }
}

/// Connect to the Angel One WebSocket and stream ticks into the provided channel.
///
/// This function handles:
/// - WSS connection with auth headers
/// - Subscription request
/// - Heartbeat pings every 10 seconds
/// - Binary frame parsing → Tick conversion
/// - Automatic reconnection with exponential backoff
pub async fn connect_and_stream(
    tokens: AuthTokens,
    config: SubscriptionConfig,
    tx: mpsc::Sender<Tick>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut attempt = 0u32;

    loop {
        match run_stream(&tokens, &config, &tx).await {
            Ok(()) => {
                info!("WebSocket stream ended gracefully");
                break Ok(());
            }
            Err(e) => {
                attempt += 1;
                if attempt > MAX_RECONNECT_ATTEMPTS {
                    error!("Max reconnect attempts ({MAX_RECONNECT_ATTEMPTS}) exceeded");
                    break Err(e);
                }
                let delay = RECONNECT_BASE_DELAY * 2u32.pow(attempt - 1);
                warn!(
                    "WebSocket error (attempt {attempt}/{MAX_RECONNECT_ATTEMPTS}): {e}. \
                     Reconnecting in {delay:?}..."
                );
                time::sleep(delay).await;
            }
        }
    }
}

/// Inner stream loop for a single WebSocket session.
async fn run_stream(
    tokens: &AuthTokens,
    config: &SubscriptionConfig,
    tx: &mpsc::Sender<Tick>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Build the request with auth headers
    let mut request = WS_URL.into_client_request()?;
    let headers = request.headers_mut();
    headers.insert(
        "Authorization",
        format!("Bearer {}", tokens.jwt_token).parse()?,
    );
    headers.insert("x-api-key", tokens.api_key.parse()?);
    headers.insert("x-client-code", tokens.client_id.parse()?);
    headers.insert("x-feed-token", tokens.feed_token.parse()?);

    info!("Connecting to Angel One WebSocket...");
    let (ws_stream, _response) =
        tokio_tungstenite::connect_async(request).await?;
    info!("WebSocket connected successfully");

    let (mut sink, mut stream) = ws_stream.split();

    // Send subscription request — one tokenList entry per exchange bucket.
    let token_list: Vec<serde_json::Value> = config.buckets.iter().map(|b| {
        json!({ "exchangeType": b.exchange_type, "tokens": b.tokens })
    }).collect();
    let total_tokens: usize = config.buckets.iter().map(|b| b.tokens.len()).sum();

    let subscribe_msg = json!({
        "correlationID": "phase0_sub",
        "action": 1,
        "params": {
            "mode": config.mode,
            "tokenList": token_list
        }
    });

    sink.send(Message::Text(subscribe_msg.to_string())).await?;
    info!("Subscribed to {total_tokens} tokens across {} exchange buckets in mode {}",
        config.buckets.len(), config.mode);

    // Spawn heartbeat task
    let heartbeat_sink = tx.clone(); // just to keep the task alive
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = time::interval(HEARTBEAT_INTERVAL);
        loop {
            interval.tick().await;
            // The heartbeat_sink clone keeps us tied to the pipeline lifetime
            if heartbeat_sink.is_closed() {
                break;
            }
        }
    });

    // We also need to send pings through the sink, but since sink is moved,
    // we'll use a separate channel to coordinate
    let (ping_tx, mut ping_rx) = mpsc::channel::<()>(1);

    // Ping scheduler
    tokio::spawn(async move {
        let mut interval = time::interval(HEARTBEAT_INTERVAL);
        loop {
            interval.tick().await;
            if ping_tx.send(()).await.is_err() {
                break;
            }
        }
    });

    // Tick counter for logging
    let mut tick_count: u64 = 0;
    let mut last_log = tokio::time::Instant::now();

    loop {
        tokio::select! {
            // Incoming WebSocket message
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        match parse_binary_packet(&data) {
                            Ok(parsed) => {
                                let tick = parsed.to_tick();
                                if tx.send(tick).await.is_err() {
                                    info!("Tick channel closed, stopping stream");
                                    break;
                                }
                                tick_count += 1;

                                // Log throughput every 10 seconds
                                if last_log.elapsed() >= Duration::from_secs(10) {
                                    info!("Ingested {tick_count} ticks total");
                                    last_log = tokio::time::Instant::now();
                                }
                            }
                            Err(e) => {
                                // Warn (not debug) so parse failures are visible at the
                                // default log level. Hex-dump the header bytes to aid
                                // protocol debugging (e.g. new frame types from the broker).
                                let hex: String = data
                                    .iter()
                                    .take(16)
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                warn!("Binary parse error ({} bytes, header: [{hex}]): {e}", data.len());
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if text == "pong" {
                            // pong confirmations are very frequent; skip logging
                        } else {
                            info!("Received text message: {text}");
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {} // WebSocket pong — ignore
                    Some(Ok(Message::Close(frame))) => {
                        info!("Server sent close frame: {frame:?}");
                        break;
                    }
                    Some(Ok(_)) => {} // Ping, Frame — ignore
                    Some(Err(e)) => {
                        error!("WebSocket read error: {e}");
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended (None)");
                        break;
                    }
                }
            }
            // Send periodic ping
            _ = ping_rx.recv() => {
                if let Err(e) = sink.send(Message::Text("ping".to_string())).await {
                    error!("Failed to send heartbeat ping: {e}");
                    break;
                }
                // ping sent — no log needed, very frequent
            }
        }
    }

    heartbeat_handle.abort();
    info!("Stream session ended after {tick_count} ticks");

    Ok(())
}
