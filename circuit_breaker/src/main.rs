//! # Circuit Breaker — Kill Switch
//!
//! An isolated binary that monitors the trading system's health and PnL via
//! ZeroMQ, and triggers an emergency shutdown if thresholds are breached.
//!
//! ## Kill Conditions
//! 1. **Heartbeat timeout:** No message received for > 50ms (after grace period)
//! 2. **PnL breach:** Cumulative loss exceeds `CIRCUIT_BREAKER_MAX_LOSS`
//!
//! ## Panic Sequence
//! 1. POST to Angel One `/cancelAllOrders` (skipped in Phase 0 / dry-run)
//! 2. POST to Angel One `/exitAllPositions` (skipped in Phase 0 / dry-run)
//! 3. Log the event
//! 4. `std::process::exit(1)` — hard exit
//!
//! ## ZMQ Protocol
//! Listens on `tcp://127.0.0.1:5555` (SUB socket).
//! Expected message format (JSON):
//! ```json
//! {"heartbeat": true, "pnl": -1500.0, "timestamp": 1700000000}
//! ```
//!
//! ## Environment Variables
//! - `CIRCUIT_BREAKER_MAX_LOSS` — max cumulative loss in INR (default: 10000)
//! - `CIRCUIT_BREAKER_GRACE_SECS` — startup grace period in seconds (default: 10)
//! - `CIRCUIT_BREAKER_DRY_RUN` — if "true", skip REST cancellation calls (default: false)
//! - `ANGEL_JWT_TOKEN` — JWT bearer token for REST API calls
//! - `ANGEL_API_KEY` — Angel One private API key
//!
//! Run with: `cargo run -p circuit_breaker`
use common::PnlMessage;
use reqwest::Client;
use tokio::time::{self, Duration, Instant};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zeromq::{Socket, SocketRecv, SubSocket};

/// ZMQ endpoint (TCP on Windows since IPC is not supported).
const ZMQ_ENDPOINT: &str = "tcp://127.0.0.1:5555";

/// Maximum time between heartbeats before triggering the kill switch.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(50);

/// Check interval for the heartbeat watchdog.
const WATCHDOG_INTERVAL: Duration = Duration::from_millis(10);

/// Angel One API base URL.
const API_BASE: &str = "https://apiconnect.angelone.in/rest/secure/angelbroking";

/// Startup grace period — watchdog is suppressed for this long after launch
/// so the ingestion node has time to authenticate and send its first heartbeat.
const DEFAULT_GRACE_SECS: u64 = 10;



#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    info!("Circuit Breaker starting");
    info!("Listening on {ZMQ_ENDPOINT}");
    info!("Heartbeat timeout: {HEARTBEAT_TIMEOUT:?}");

    // Load environment
    dotenvy::dotenv().ok();

    let max_loss: f64 = std::env::var("CIRCUIT_BREAKER_MAX_LOSS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000.0);

    let grace_secs: u64 = std::env::var("CIRCUIT_BREAKER_GRACE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_GRACE_SECS);
    let grace_period = Duration::from_secs(grace_secs);

    // In Phase 0 this is a read-only pipeline — no live orders are ever placed,
    // so the REST cancellation endpoints are not applicable. Set DRY_RUN=true
    // (or leave unset; it defaults to true) to skip those calls and exit cleanly.
    // Set DRY_RUN=false explicitly when you move to Phase 1 live trading.
    let dry_run: bool = std::env::var("CIRCUIT_BREAKER_DRY_RUN")
        .map(|v| v.to_lowercase() != "false")
        .unwrap_or(true);

    info!("Max loss threshold: ₹{max_loss:.2}");
    info!("Startup grace period: {grace_secs}s");
    info!("Dry-run mode (skip REST calls): {dry_run}");

    // Load auth tokens for the panic REST calls
    let mut jwt_token = std::env::var("ANGEL_JWT_TOKEN").unwrap_or_default();
    let api_key = std::env::var("ANGEL_API_KEY").unwrap_or_default();

    if jwt_token.is_empty() {
        // Try reading from the temporary file created by the ingestion node
        if let Ok(token) = std::fs::read_to_string(".jwt_token") {
            jwt_token = token.trim().to_string();
            info!("Loaded ANGEL_JWT_TOKEN from .jwt_token file");
        }
    }

    if !dry_run && (jwt_token.is_empty() || api_key.is_empty()) {
        warn!(
            "ANGEL_JWT_TOKEN or ANGEL_API_KEY not set. \
             Circuit breaker will detect faults but cannot execute REST cancellation. \
             Start the ingestion node first to generate these."
        );
    }

    // Connect ZMQ SUB socket
    let mut socket = SubSocket::new();
    socket.subscribe("").await.unwrap_or_else(|e| {
        error!("Failed to set ZMQ subscription: {e}");
        std::process::exit(1);
    });

    if let Err(e) = socket.connect(ZMQ_ENDPOINT).await {
        error!("Failed to connect to ZMQ at {ZMQ_ENDPOINT}: {e}");
        error!("Make sure the ingestion node is running and publishing on this endpoint.");
        info!("Waiting for publisher to come online...");

        // Retry connection
        let mut connected = false;
        for attempt in 1..=10 {
            time::sleep(Duration::from_secs(2)).await;
            match socket.connect(ZMQ_ENDPOINT).await {
                Ok(()) => {
                    info!("Connected to ZMQ on attempt {attempt}");
                    connected = true;
                    break;
                }
                Err(e) => {
                    warn!("ZMQ connect attempt {attempt}/10 failed: {e}");
                }
            }
        }

        if !connected {
            error!("Could not connect to ZMQ after 10 attempts. Exiting.");
            std::process::exit(1);
        }
    }

    info!("ZMQ SUB socket connected. Monitoring heartbeat and PnL...");

    let http_client = Client::new();

    // Seed the heartbeat clock at startup. The grace period suppresses the
    // watchdog so this initial value never triggers a false positive.
    let mut last_heartbeat = Instant::now();
    let startup_time = Instant::now();

    info!("Watchdog armed — grace period active for {grace_secs}s");

    loop {
        tokio::select! {
            // Check for incoming ZMQ messages
            msg = socket.recv() => {
                match msg {
                    Ok(zmq_msg) => {
                        let bytes = zmq_msg.get(0)
                            .map(|f| f.to_vec())
                            .unwrap_or_default();
                        let text = String::from_utf8_lossy(&bytes);

                        match serde_json::from_str::<PnlMessage>(&text) {
                            Ok(pnl_msg) => {
                                // Update heartbeat timestamp
                                if pnl_msg.heartbeat {
                                    last_heartbeat = Instant::now();
                                }

                                // Check PnL breach
                                if pnl_msg.pnl.abs() >= max_loss {
                                    error!(
                                        "PnL BREACH DETECTED! PnL={:.2} exceeds max_loss={:.2}",
                                        pnl_msg.pnl, max_loss
                                    );
                                    execute_panic_sequence(
                                        &http_client,
                                        &jwt_token,
                                        &api_key,
                                        dry_run,
                                    )
                                    .await;
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse ZMQ message: {e} — raw: {text}");
                            }
                        }
                    }
                    Err(e) => {
                        error!("ZMQ recv error: {e}");
                    }
                }
            }
            // Heartbeat watchdog — suppressed during the grace period
            _ = time::sleep(WATCHDOG_INTERVAL) => {
                let in_grace = startup_time.elapsed() < grace_period;
                if !in_grace && last_heartbeat.elapsed() > HEARTBEAT_TIMEOUT {
                    error!(
                        "HEARTBEAT TIMEOUT! No message for {:?} (threshold: {:?})",
                        last_heartbeat.elapsed(),
                        HEARTBEAT_TIMEOUT
                    );
                    execute_panic_sequence(
                        &http_client,
                        &jwt_token,
                        &api_key,
                        dry_run,
                    )
                    .await;
                }
            }
        }
    }
}

/// Execute the emergency shutdown sequence.
///
/// 1. Cancel all open orders   (skipped when `dry_run = true`)
/// 2. Exit all open positions   (skipped when `dry_run = true`)
/// 3. Log the event
/// 4. Hard exit with code 1
///
/// `dry_run` should be `true` for Phase 0 (read-only) where no live orders
/// exist. Set it to `false` in Phase 1 when the execution engine is live.
async fn execute_panic_sequence(
    client: &Client,
    jwt_token: &str,
    api_key: &str,
    dry_run: bool,
) {
    error!("╔══════════════════════════════════════╗");
    error!("║   CIRCUIT BREAKER TRIGGERED          ║");
    error!("║   Executing emergency shutdown...    ║");
    error!("╚══════════════════════════════════════╝");

    if dry_run {
        warn!(
            "DRY-RUN mode — skipping REST order cancellation. \
             Phase 0 is read-only; no live orders exist. \
             Set CIRCUIT_BREAKER_DRY_RUN=false in Phase 1."
        );
        error!("Panic sequence complete (dry-run). Hard exit.");
        std::process::exit(1);
    }

    if jwt_token.is_empty() || api_key.is_empty() {
        error!("Cannot execute REST cancellation — auth tokens not configured!");
        error!("MANUAL INTERVENTION REQUIRED!");
        std::process::exit(1);
    }

    // Fetch our real public IP so the WAF doesn't reject us for using 127.0.0.1
    let public_ip = fetch_public_ip(client).await;

    // Step 1: Cancel all orders
    let cancel_url = format!("{API_BASE}/order/v1/cancelAllOrders");
    match client
        .post(&cancel_url)
        .header("Authorization", format!("Bearer {jwt_token}"))
        .header("X-PrivateKey", api_key)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("X-UserType", "USER")
        .header("X-SourceID", "WEB")
        .header("X-ClientLocalIP", "127.0.0.1")
        .header("X-ClientPublicIP", &public_ip)
        .header("X-MACAddress", "00-00-00-00-00-00")
        .header("User-Agent", "AngelOneTradingBot/1.0")
        .body("{}")
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                info!("Cancel all orders: {status} — {body}");
            } else {
                error!("Cancel all orders failed: HTTP {status} — {body}");
            }
        }
        Err(e) => {
            error!("Failed to cancel orders: {e}");
        }
    }

    // Step 2: Exit all positions
    let close_url = format!("{API_BASE}/order/v1/exitAllPositions");
    match client
        .post(&close_url)
        .header("Authorization", format!("Bearer {jwt_token}"))
        .header("X-PrivateKey", api_key)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("X-UserType", "USER")
        .header("X-SourceID", "WEB")
        .header("X-ClientLocalIP", "127.0.0.1")
        .header("X-ClientPublicIP", &public_ip)
        .header("X-MACAddress", "00-00-00-00-00-00")
        .header("User-Agent", "AngelOneTradingBot/1.0")
        .body("{}")
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                info!("Exit all positions: {status} — {body}");
            } else {
                error!("Exit all positions failed: HTTP {status} — {body}");
            }
        }
        Err(e) => {
            error!("Failed to exit positions: {e}");
        }
    }

    // Step 3: Hard exit
    error!("Panic sequence complete. Hard exit.");
    std::process::exit(1);
}

/// Fetch the machine's real public IP from a lightweight STUN-like service.
/// Falls back to a safe placeholder on failure.
async fn fetch_public_ip(client: &Client) -> String {
    match client
        .get("https://api.ipify.org")
        .header("User-Agent", "AngelOneTradingBot/1.0")
        .send()
        .await
    {
        Ok(resp) => resp.text().await.unwrap_or_else(|_| "0.0.0.0".to_string()),
        Err(_) => {
            warn!("Could not fetch public IP, using fallback");
            "0.0.0.0".to_string()
        }
    }
}
