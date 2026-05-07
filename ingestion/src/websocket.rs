//! # Angel One WebSocket v2 Streaming Client
//!
//! Connects to `wss://smartapisocket.angelone.in/smart-stream`, subscribes
//! to instrument tokens, parses the binary stream into [`common::Tick`] structs,
//! and pushes them into a `tokio::sync::mpsc` channel for downstream consumption.

use crate::auth::AuthTokens;
use common::{parse_binary_packet, Tick};
use futures_util::{SinkExt, StreamExt};
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

/// Configuration for which instruments to subscribe to.
#[derive(Debug, Clone)]
pub struct SubscriptionConfig {
    /// Angel One instrument token strings (e.g. "26009").
    pub tokens: Vec<String>,
    /// Exchange type (2 = NSE_FO).
    pub exchange_type: u8,
    /// Subscription mode (1=LTP, 2=Quote, 3=SnapQuote).
    pub mode: u8,
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            tokens: vec!["26009".to_string(), "26000".to_string()],
            // NSE Cash Market (CM) = 1.  NIFTY 50 (26009) and NIFTY BANK (26000)
            // are index instruments traded on NSE CM, NOT NSE F&O (2).
            // Using the wrong exchange_type causes the server to silently
            // accept the subscription but send zero data.
            exchange_type: 1, // NSE_CM
            mode: 3,          // SnapQuote
        }
    }
}

/// Load subscription config from environment variables, falling back to defaults.
pub fn load_subscription_config() -> SubscriptionConfig {
    let tokens = std::env::var("SUBSCRIBE_TOKENS")
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
        .unwrap_or_else(|_| vec!["26009".to_string(), "26000".to_string()]);

    let exchange_type = std::env::var("SUBSCRIBE_EXCHANGE")
        .ok()
        .and_then(|s| s.parse().ok())
        // Default: NSE CM (1).  NIFTY 50 (26009) / NIFTY BANK (26000) are CM tokens.
        // Override with SUBSCRIBE_EXCHANGE=2 for F&O instruments.
        .unwrap_or(1u8);

    SubscriptionConfig {
        tokens,
        exchange_type,
        mode: 3, // SnapQuote for maximum data
    }
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

    // Send subscription request
    let subscribe_msg = json!({
        "correlationID": "phase0_sub",
        "action": 1,
        "params": {
            "mode": config.mode,
            "tokenList": [{
                "exchangeType": config.exchange_type,
                "tokens": config.tokens
            }]
        }
    });

    sink.send(Message::Text(subscribe_msg.to_string())).await?;
    info!(
        "Subscribed to {} tokens on exchange {} in mode {}",
        config.tokens.len(),
        config.exchange_type,
        config.mode
    );

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
