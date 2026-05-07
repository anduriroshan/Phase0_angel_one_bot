//! # Ingestion Node — Entry Point
//!
//! Orchestrates the Phase 0 data ingestion pipeline:
//! 1. Authenticates with Angel One SmartAPI (REST + TOTP)
//! 2. Opens a WebSocket stream and pushes ticks into an mpsc channel
//! 3. Consumes ticks and fans out to storage sinks
//! 4. Publishes heartbeat/PnL on a ZMQ socket for the circuit breaker
//!
//! Run with: `cargo run -p ingestion`

mod auth;
mod websocket;

use common::{PnlMessage, Tick};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zeromq::{PubSocket, Socket, SocketSend};

/// Channel buffer size — large enough to absorb bursts without backpressure.
const CHANNEL_BUFFER: usize = 8192;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .with_thread_ids(true)
        .init();

    info!("Phase 0 — Ingestion Node starting");

    // Load .env file
    dotenvy::dotenv().ok();

    // Step 1: Authenticate
    info!("Authenticating with Angel One SmartAPI...");
    let tokens = auth::authenticate().await.map_err(|e| {
        error!("Authentication failed: {e}");
        e
    })?;
    info!("Authentication successful, feed token acquired");
    
    // Save JWT to a file so the circuit breaker can pick it up automatically
    if let Err(e) = std::fs::write(".jwt_token", &tokens.jwt_token) {
        warn!("Failed to save JWT to .jwt_token: {e}");
    } else {
        info!("JWT token saved to .jwt_token for circuit breaker use");
    }

    info!("--------------------------------------------------");
    info!("ANGEL_JWT_TOKEN: {}", tokens.jwt_token);
    info!("--------------------------------------------------");

    // Step 2: Create tick channel
    let (tx, mut rx) = mpsc::channel::<Tick>(CHANNEL_BUFFER);

    // Step 3: Load subscription config
    let sub_config = websocket::load_subscription_config();
    info!("Subscription config: {:?}", sub_config);

    // Step 4: Spawn WebSocket ingestion task
    let ws_tokens = tokens.clone();
    let ws_tx = tx.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = websocket::connect_and_stream(ws_tokens, sub_config, ws_tx).await {
            error!("WebSocket stream fatal error: {e}");
        }
    });

    // Drop the original sender so the channel closes when WS task ends
    drop(tx);

    // Step 5: Setup ZMQ PUB socket for circuit breaker
    let mut zmq_socket = PubSocket::new();
    if let Err(e) = zmq_socket.bind("tcp://127.0.0.1:5555").await {
        error!("Failed to bind ZMQ PUB socket: {e}");
        error!("Check if another instance is running or if the port is occupied.");
    } else {
        info!("ZMQ PUB socket bound to tcp://127.0.0.1:5555");
    }

    // Step 6: Spawn a dedicated heartbeat timer task.
    //
    // This sends a heartbeat every 20ms INDEPENDENT of tick flow, so the
    // circuit breaker (50ms timeout) never trips during slow markets or
    // startup. We use a watch channel to pass the latest PnL from the
    // consumer so the heartbeat task can include it.
    let (pnl_tx, mut pnl_rx) = watch::channel::<f64>(0.0);

    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(20));
        loop {
            interval.tick().await;
            let current_pnl = *pnl_rx.borrow_and_update();
            let pnl_msg = PnlMessage {
                heartbeat: true,
                pnl: current_pnl,
                timestamp: chrono::Utc::now().timestamp(),
            };
            match serde_json::to_string(&pnl_msg) {
                Ok(json) => {
                    if let Err(e) = zmq_socket.send(json.into()).await {
                        // Suppress the "connection reset" debug noise from the
                        // ZMQ library when no subscriber is yet connected.
                        warn!("ZMQ heartbeat send warning: {e}");
                    }
                }
                Err(e) => {
                    error!("Failed to serialize heartbeat: {e}");
                }
            }
        }
    });

    // Step 7: Consume ticks (storage fan-out)
    //
    // In the full pipeline, this calls into the `storage` crate.
    // For now, we log received ticks and forward to storage when available.
    let consumer_handle = tokio::spawn(async move {
        let mut count: u64 = 0;
        let mut last_tick: Option<Tick> = None;

        while let Some(tick) = rx.recv().await {
            count += 1;
            last_tick = Some(tick.clone());

            // Log every 100th tick to avoid flooding stdout
            if count % 100 == 0 {
                info!(
                    "Tick #{count}: inst_id={} price={:.2} qty={} seq={}",
                    tick.inst_id, tick.price, tick.qty, tick.seq_no
                );
            }

            // Update the shared PnL so the heartbeat task sends the latest value.
            // TODO: Replace 0.0 with real PnL calculation.
            let _ = pnl_tx.send(0.0);

            // TODO: Forward to storage::write_tick(&tick)
        }

        if let Some(tick) = last_tick {
            info!(
                "Consumer finished. Total ticks: {count}. Last: inst_id={} price={:.2}",
                tick.inst_id, tick.price
            );
        } else {
            info!("Consumer finished with no ticks received");
        }
    });

    // Step 8: Wait for graceful shutdown
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl+C received, shutting down...");
        }
        _ = ws_handle => {
            info!("WebSocket task completed");
        }
    }

    // Stop the heartbeat task and give the consumer a moment to drain
    heartbeat_handle.abort();
    let _ = tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        consumer_handle,
    )
    .await;

    info!("Ingestion node shut down cleanly");
    Ok(())
}
