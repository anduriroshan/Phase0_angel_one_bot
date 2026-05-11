//! # trading — LiveTradingNode binary
//!
//! Wires together:
//! - `AngelOneDataClient`   — market data via Angel One SmartStream WebSocket
//! - `AngelOneExecutionClient` — order routing via Angel One SmartAPI REST
//! - `NseRiskCheck`         — NSE F&O lot-size / freeze-qty / physical-settlement gate
//! - `BasisArbStrategy`     — NIFTY futures-vs-spot basis-arb signal
//!
//! ## Configuration
//! - `config/trading.toml` — node config (instruments, trader ID, etc.)
//! - `config/nse_risk.toml` — NSE risk limits
//! - `config/strategy_basis_arb.toml` — strategy tuning
//! - Environment variables:
//!   - `ANGEL_CLIENT_ID`, `ANGEL_PASSWORD`, `ANGEL_TOTP_SECRET` — credentials
//!   - `ANGEL_API_KEY` — SmartAPI key
//!   - `ANGEL_ONE_DRY_RUN` — `"false"` to enable live order routing (default: true)
//!
//! ## Heartbeat
//! Publishes a ZMQ PUB heartbeat every 20 ms to `tcp://127.0.0.1:5555` for
//! the circuit breaker watchdog.  On SIGTERM / SIGINT, publishes a final
//! `GracefulShutdown` event before the node stops.
//!
//! ## Run
//! ```sh
//! cargo run -p trading
//! ```

use std::{collections::HashMap, sync::Arc, time::Duration};

use adapter_angelone::{
    AngelOneDataClientConfig, AngelOneDataClientFactory, AngelOneDataLiveConfig,
    AngelOneExecClientFactory, AngelOneExecLiveConfig, AngelOneExecutionClientConfig,
    InstrumentMapping,
};
use chrono::Utc;
use common::PnlMessage;
use nautilus_common::enums::Environment;
use nautilus_live::node::LiveNode;
use nautilus_model::identifiers::{AccountId, ClientId, InstrumentId, Symbol, TraderId, Venue};
use risk_nse::NseRiskCheck;
use serde::Deserialize;
use strategy_basis_arb::{BasisArbConfig, BasisArbParams, BasisArbStrategy};
use strategy_intraday_vwap::{IntradayVwapConfig, IntradayVwapParams, IntradayVwapStrategy};
use storage::storage_consumer;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zeromq::{PubSocket, Socket, SocketSend};

// ---------------------------------------------------------------------------
// Config schema (trading.toml)
// ---------------------------------------------------------------------------

/// Top-level config loaded from `config/trading.toml`.
#[derive(Debug, Deserialize)]
struct TradingConfig {
    /// NautilusTrader trader ID (e.g. `"ANGEL-TRADER-001"`).
    trader_id: String,

    /// Angel One account ID (must contain a dash, e.g. `"ANGEL-A321480"`).
    account_id: String,

    /// NSE venue name (should be `"NSE"`).
    venue: String,

    /// Angel One data client ID (e.g. `"ANGEL_ONE"`).
    data_client_id: String,

    /// List of instruments to subscribe to.
    instruments: Vec<InstrumentEntry>,
}

/// A single instrument in `trading.toml`.
#[derive(Debug, Deserialize)]
struct InstrumentEntry {
    /// NautilusTrader symbol (e.g. `"NIFTY26JUNFUT"`).
    symbol: String,

    /// Angel One integer token.
    token: u32,

    /// Angel One trading symbol (e.g. `"NIFTY25JUNFUT"`).
    trading_symbol: String,

    /// Angel One exchange string (`"NFO"` for F&O, `"NSE"` for equity).
    exchange: String,

    /// Optional expiry date in RFC 3339 format (e.g. `"2026-06-26T15:30:00Z"`).
    expiry_utc: Option<String>,

    /// Whether this instrument settles physically (stock F&O).
    #[serde(default)]
    is_physical_settlement: bool,

    /// Angel One product type: "MIS" (intraday), "CARRYFORWARD" (F&O positional), "NRML" (equity delivery).
    /// Default: "CARRYFORWARD" (safe for F&O; override to "MIS" for equity intraday).
    #[serde(default = "default_product_type")]
    product_type: String,
}

fn default_product_type() -> String {
    "CARRYFORWARD".to_string()
}

// ---------------------------------------------------------------------------
// Helper: build instrument maps from config
// ---------------------------------------------------------------------------

fn build_instrument_maps(
    entries: &[InstrumentEntry],
    venue: &str,
) -> (
    HashMap<u32, InstrumentId>,              // token → instrument_id (DataClient)
    HashMap<u32, u8>,                        // token → exchange_type (1=NSE_CM, 2=NSE_FO)
    HashMap<InstrumentId, InstrumentMapping>, // instrument_id → mapping (ExecClient)
) {
    let venue = Venue::new(venue);
    let mut data_map = HashMap::new();
    let mut exchange_map: HashMap<u32, u8> = HashMap::new();
    let mut exec_map = HashMap::new();

    for entry in entries {
        let instrument_id = InstrumentId::new(Symbol::new(&entry.symbol), venue);

        // NSE equities and index live on NSE_CM (exchange_type=1).
        // NFO derivatives live on NSE_FO (exchange_type=2).
        let exchange_type: u8 = if entry.exchange == "NFO" { 2 } else { 1 };

        data_map.insert(entry.token, instrument_id);
        exchange_map.insert(entry.token, exchange_type);

        let expiry_utc = entry.expiry_utc.as_deref().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    warn!("Failed to parse expiry_utc '{}' for {}: {e}", s, entry.symbol);
                    e
                })
                .ok()
        });

        exec_map.insert(
            instrument_id,
            InstrumentMapping {
                token: entry.token.to_string(),
                trading_symbol: entry.trading_symbol.clone(),
                exchange: entry.exchange.clone(),
                expiry_utc,
                is_physical_settlement: entry.is_physical_settlement,
                product_type: entry.product_type.clone(),
            },
        );
    }

    (data_map, exchange_map, exec_map)
}

// ---------------------------------------------------------------------------
// ZMQ heartbeat task
// ---------------------------------------------------------------------------

/// Spawns a tokio task that publishes a ZMQ heartbeat every 20 ms on
/// `tcp://127.0.0.1:5555`.  The circuit breaker watchdog times out after 50 ms,
/// so 20 ms gives 2× headroom.
///
/// Returns a handle to signal the task to stop, and a join handle.
fn spawn_heartbeat_task() -> (Arc<std::sync::atomic::AtomicBool>, tokio::task::JoinHandle<()>) {
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop_flag.clone();

    let handle = tokio::spawn(async move {
        let mut socket = PubSocket::new();
        if let Err(e) = socket.bind("tcp://127.0.0.1:5555").await {
            error!("Failed to bind ZMQ heartbeat socket: {e}. Is the circuit_breaker port in use?");
            return;
        }
        info!("ZMQ heartbeat publishing on tcp://127.0.0.1:5555 (20 ms interval)");

        let mut interval = tokio::time::interval(Duration::from_millis(20));
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            interval.tick().await;

            let msg = PnlMessage {
                heartbeat: true,
                pnl: 0.0, // TODO: wire PnL from portfolio when available
                timestamp: Utc::now().timestamp(),
            };

            match serde_json::to_string(&msg) {
                Ok(json) => {
                    if let Err(e) = socket.send(json.into()).await {
                        warn!("ZMQ heartbeat send error: {e}");
                    }
                }
                Err(e) => error!("Failed to serialize heartbeat: {e}"),
            }
        }

        // Send a final graceful-shutdown message before exiting.
        let shutdown_msg = serde_json::json!({"heartbeat": false, "shutdown": true,
            "timestamp": Utc::now().timestamp()});
        let _ = socket.send(shutdown_msg.to_string().into()).await;
        info!("ZMQ heartbeat task stopped");
    });

    (stop_flag, handle)
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Logging ---
    // Note: We DO NOT initialize tracing_subscriber here.
    // NautilusTrader's LiveNode automatically initializes its own logger.
    // Initializing twice causes a "non-Nautilus logger is already registered" panic.

    info!("Angel One LiveTradingNode starting");

    // Load .env if present (dev only; production uses process env)
    dotenvy::dotenv().ok();

    // --- Load configs ---
    let trading_cfg: TradingConfig = {
        let text = std::fs::read_to_string("config/trading.toml")
            .map_err(|e| anyhow::anyhow!("Cannot read config/trading.toml: {e}"))?;
        toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Failed to parse config/trading.toml: {e}"))?
    };

    let strategy_params = BasisArbParams::from_file("config/strategy_basis_arb.toml")
        .map_err(|e| anyhow::anyhow!("Cannot load config/strategy_basis_arb.toml: {e}"))?;

    let nse_risk = Arc::new(
        NseRiskCheck::from_file("config/nse_risk.toml")
            .map_err(|e| anyhow::anyhow!("Cannot load config/nse_risk.toml: {e}"))?,
    );

    info!(
        "Config loaded: trader={} account={} venue={} instruments={}",
        trading_cfg.trader_id,
        trading_cfg.account_id,
        trading_cfg.venue,
        trading_cfg.instruments.len()
    );

    // --- Build instrument maps ---
    let (data_instrument_map, token_exchange_map, exec_instrument_map) =
        build_instrument_maps(&trading_cfg.instruments, &trading_cfg.venue);

    // --- Build client configs ---
    let venue = Venue::new(&trading_cfg.venue);
    let data_client_id = ClientId::new(&trading_cfg.data_client_id);
    let exec_client_id = ClientId::new("ANGEL_ONE_EXEC");
    let account_id = AccountId::new(&trading_cfg.account_id);

    // --- Storage pipeline: every raw tick is also written to QuestDB + Parquet ---
    // Buffer of 8192 ticks (~400 ms at 20 ticks/s × 5 instruments) before backpressure.
    std::fs::create_dir_all("./data/raw").ok();
    let (tick_tx, tick_rx) = tokio::sync::mpsc::channel(8192);
    tokio::spawn(storage_consumer(tick_rx));
    info!("Storage consumer spawned — writing ticks to QuestDB + ./data/raw/");

    let data_cfg = AngelOneDataClientConfig::new(
        data_client_id,
        venue,
        data_instrument_map,
        token_exchange_map,
    )
    .with_tick_sender(tick_tx);

    let exec_cfg = AngelOneExecutionClientConfig::new(
        exec_client_id,
        venue,
        account_id,
        nautilus_model::enums::OmsType::Netting,
        exec_instrument_map,
    )
    .with_nse_risk(nse_risk);

    info!(
        "Dry-run mode: {}",
        exec_cfg.dry_run
    );

    // --- Build LiveNode ---
    let trader_id = TraderId::new(&trading_cfg.trader_id);
    let mut node = LiveNode::builder(trader_id, Environment::Live)?
        .add_data_client(
            None,
            Box::new(AngelOneDataClientFactory),
            Box::new(AngelOneDataLiveConfig(data_cfg)),
        )?
        .add_exec_client(
            None,
            Box::new(AngelOneExecClientFactory),
            Box::new(AngelOneExecLiveConfig(exec_cfg)),
        )?
        .build()?;

    // --- Register strategy ---
    let futures_id = InstrumentId::new(
        Symbol::new(&strategy_params.futures_instrument_id.replace(&format!(".{}", &trading_cfg.venue), "")),
        venue,
    );
    let spot_id = InstrumentId::new(
        Symbol::new(&strategy_params.spot_instrument_id.replace(&format!(".{}", &trading_cfg.venue), "")),
        venue,
    );
    let strategy = BasisArbStrategy::new(BasisArbConfig::new(strategy_params, futures_id, spot_id));
    node.add_strategy(strategy)?;
    info!("BasisArbStrategy registered");

    // --- Load and register IntradayVwapStrategy ---
    let vwap_params = IntradayVwapParams::from_file("config/strategy_intraday_vwap.toml")
        .map_err(|e| anyhow::anyhow!("Cannot load config/strategy_intraday_vwap.toml: {e}"))?;
    let vwap_strategy = IntradayVwapStrategy::new(IntradayVwapConfig::new(vwap_params));
    node.add_strategy(vwap_strategy)?;
    info!("IntradayVwapStrategy registered");

    // --- Start ZMQ heartbeat ---
    let (hb_stop, hb_handle) = spawn_heartbeat_task();

    // --- Run node (blocks until SIGINT/SIGTERM) ---
    info!("LiveTradingNode running — press Ctrl+C to stop");
    let result = node.run().await;

    // --- Cleanup ---
    hb_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = hb_handle.await;

    match &result {
        Ok(()) => info!("LiveTradingNode stopped cleanly"),
        Err(e) => error!("LiveTradingNode exited with error: {e}"),
    }

    result
}
