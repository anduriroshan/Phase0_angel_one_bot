use anyhow::{Context, Result};
use clap::Parser;
use std::fs::File;
use std::path::PathBuf;
use tracing::info;

use arrow::array::{Float64Array, Int64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Data, QuoteTick},
    enums::{AccountType, BookType, OmsType},
    identifiers::{InstrumentId, Symbol, Venue},
    instruments::{Equity, InstrumentAny},
    types::{Currency, Money, Price, Quantity},
};
use rust_decimal::Decimal;
use serde::Deserialize;
use strategy_basis_arb::BasisArbStrategy;

#[derive(Parser, Debug)]
#[command(about = "Replay a fixture day through the BacktestEngine (Step 10 smoke test)")]
struct Args {
    /// Date to replay (YYYY-MM-DD)
    #[arg(long)]
    date: String,

    /// Base data directory (contains YYYY/MM/DD/<token>.parquet)
    #[arg(long, default_value = "./data/raw")]
    data_dir: String,

    /// Trading config file (used to map token integers to instrument symbols)
    #[arg(long, default_value = "config/trading.toml")]
    config: String,

    /// Which strategy to run: "basis", "vwap", or "both".
    /// Run one at a time to judge each strategy's edge in isolation.
    #[arg(long, default_value = "both")]
    strategy: String,
}

// ---------------------------------------------------------------------------
// Minimal config structs — mirrors trading/src/main.rs InstrumentEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TradingConfig {
    instruments: Vec<InstrumentEntry>,
}

#[derive(Debug, Deserialize)]
struct InstrumentEntry {
    symbol: String,
    token: u32,
    exchange: String,
}

/// Transaction-cost rates loaded from `config/costs.toml`.
///
/// Charged via NautilusTrader's default `MakerTakerFeeModel` as a per-side
/// fraction of notional. See `config/costs.toml` for the rate rationale.
#[derive(Debug, Clone, Deserialize)]
struct CostConfig {
    #[serde(default)]
    #[allow(dead_code)]
    schema_version: u32,
    equity_maker_fee: f64,
    equity_taker_fee: f64,
    futures_maker_fee: f64,
    futures_taker_fee: f64,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            equity_maker_fee: 0.00018,
            equity_taker_fee: 0.00018,
            futures_maker_fee: 0.00013,
            futures_taker_fee: 0.00013,
        }
    }
}

impl CostConfig {
    /// Loads from a TOML file, falling back to defaults (with a warning) on error.
    fn load(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to parse {path}: {e}; using default cost model");
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!("Cannot read {path}: {e}; using default cost model");
                Self::default()
            }
        }
    }

    /// Returns `(maker_fee, taker_fee)` for an instrument, branched by symbol.
    /// Futures symbols (containing "FUT") get the futures rate; everything else
    /// gets the equity rate.
    fn fees_for(&self, symbol: &str) -> (Decimal, Decimal) {
        let (m, t) = if symbol.contains("FUT") {
            (self.futures_maker_fee, self.futures_taker_fee)
        } else {
            (self.equity_maker_fee, self.equity_taker_fee)
        };
        (
            Decimal::try_from(m).unwrap_or_default(),
            Decimal::try_from(t).unwrap_or_default(),
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a token → InstrumentId map from config so we can derive correct
/// symbol names from the integer-named parquet files (e.g. 1594 → INFY.NSE).
fn build_token_map(cfg: &TradingConfig, venue: Venue) -> std::collections::HashMap<u32, InstrumentId> {
    let mut map: std::collections::HashMap<u32, InstrumentId> = cfg
        .instruments
        .iter()
        .map(|e| (e.token, InstrumentId::new(Symbol::new(&e.symbol), venue)))
        .collect();

    // Historical token aliases (see NEXT_STEPS gotcha #6): across the recorded
    // May dataset the NIFTY futures token rolled (57515 on 2026-05-13 -> 66071
    // afterwards) and the spot index token was corrected (26009 -> 26000 from
    // 2026-05-24). Map the legacy tokens onto whichever NIFTY future / spot is
    // registered in config so multi-day backtests don't silently drop early days.
    // Only inserted if the target symbol is actually registered, and never
    // overrides an existing token mapping.
    for (legacy, symbol) in [(57515u32, "NIFTY26MAYFUT"), (26009u32, "NIFTY")] {
        let id = InstrumentId::new(Symbol::new(symbol), venue);
        if map.values().any(|v| *v == id) {
            map.entry(legacy).or_insert(id);
        }
    }
    map
}

/// Construct a minimal Equity instrument for engine registration.
///
/// We register every instrument (including NIFTY futures) as an Equity for
/// the smoke test — this is sufficient for L1 quote routing and P&L accounting.
/// NSE tick size is 0.05 for both equities and NIFTY futures.
fn make_instrument(instrument_id: InstrumentId, maker_fee: Decimal, taker_fee: Decimal) -> InstrumentAny {
    InstrumentAny::Equity(Equity::new(
        instrument_id,
        instrument_id.symbol, // raw_symbol
        None,                 // isin
        Currency::INR(),
        2,                    // price_precision (paise, 2 decimal places)
        Price::from("0.05"),  // NSE minimum tick size
        Some(Quantity::from("1")), // lot_size = 1 share
        None,
        None,
        None,
        None,
        None, // margin_init
        None, // margin_maint
        Some(maker_fee), // maker_fee
        Some(taker_fee), // taker_fee
        None, // info
        UnixNanos::from(0u64),
        UnixNanos::from(0u64),
    ))
}

/// Load a single parquet file into `Vec<Data>` for the given instrument.
///
/// Parquet schema written by `storage::parquet_sink`:
///   col 0: ts_ns          Int64
///   col 1: inst_id        Int32  (skipped — we use instrument_id arg)
///   col 2: side           Int16  (skipped)
///   col 3: price          Float64 (skipped — last trade; use bid/ask below)
///   col 4: qty            Int64  (skipped)
///   col 5: seq_no         Int64  (skipped)
///   col 6: best_bid_price Float64
///   col 7: best_bid_qty   Int64
///   col 8: best_ask_price Float64
///   col 9: best_ask_qty   Int64
fn load_parquet(path: &PathBuf, instrument_id: InstrumentId) -> Result<Vec<Data>> {
    let file = File::open(path).with_context(|| format!("Cannot open {path:?}"))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let mut reader = builder.build()?;
    let mut data = Vec::new();

    // Angel One sends alternating single-sided depth packets: some ticks have
    // only bid data (ask=0), others only ask data (bid=0). Forward-fill the
    // last known non-zero value for each side so every QuoteTick is valid.
    // When one side never appears (e.g. NIFTY index has no depth at all,
    // SUNPHARMA may have only one-sided depth), infer it from the other side
    // (zero spread), which is acceptable for a backtest smoke test.
    let mut last_bid_p: f64 = 0.0;
    let mut last_bid_q: i64 = 1;
    let mut last_ask_p: f64 = 0.0;
    let mut last_ask_q: i64 = 1;

    while let Some(batch_res) = reader.next() {
        let batch = batch_res?;
        let ts_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("ts_ns (col 0) is not Int64")?;
        let ltp_col = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("price (col 3) is not Float64")?;
        let bid_p = batch
            .column(6)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("best_bid_price (col 6) is not Float64")?;
        let bid_q = batch
            .column(7)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("best_bid_qty (col 7) is not Int64")?;
        let ask_p = batch
            .column(8)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("best_ask_price (col 8) is not Float64")?;
        let ask_q = batch
            .column(9)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("best_ask_qty (col 9) is not Int64")?;

        for i in 0..batch.num_rows() {
            let raw_bid_p = bid_p.value(i);
            let raw_ask_p = ask_p.value(i);
            let ltp = ltp_col.value(i);

            // Update running last-known values when the packet has that side.
            if raw_bid_p > 0.0 {
                last_bid_p = raw_bid_p;
                last_bid_q = bid_q.value(i).max(1);
            }
            if raw_ask_p > 0.0 {
                last_ask_p = raw_ask_p;
                last_ask_q = ask_q.value(i).max(1);
            }

            // Skip until at least one side has been seen (or we have an LTP).
            if last_bid_p <= 0.0 && last_ask_p <= 0.0 && ltp <= 0.0 {
                continue;
            }

            // If one side has never appeared, infer from the other (zero spread).
            // For index instruments (e.g. NIFTY spot) where bid/ask are always 0,
            // use the LTP as both bid and ask — the strategy sees mid = LTP.
            let eff_bid_p = if last_bid_p > 0.0 { last_bid_p } else if last_ask_p > 0.0 { last_ask_p } else { ltp };
            let eff_bid_q = if last_bid_p > 0.0 { last_bid_q } else { last_ask_q };
            let eff_ask_p = if last_ask_p > 0.0 { last_ask_p } else if last_bid_p > 0.0 { last_bid_p } else { ltp };
            let eff_ask_q = if last_ask_p > 0.0 { last_ask_q } else { last_bid_q };

            // Skip crossed quotes (bid > ask) — pre-market garbage data.
            // Also skip quotes with spread > 1% (same filter as storage layer).
            if eff_bid_p > 0.0 && eff_ask_p > 0.0 {
                if eff_bid_p > eff_ask_p {
                    continue;
                }
                let spread_pct = (eff_ask_p - eff_bid_p) / eff_ask_p;
                if spread_pct > 0.01 {
                    continue;
                }
            }

            let ts = UnixNanos::from(ts_col.value(i) as u64);
            data.push(Data::Quote(QuoteTick::new(
                instrument_id,
                Price::new(eff_bid_p, 2),
                Price::new(eff_ask_p, 2),
                Quantity::new(eff_bid_q as f64, 0),
                Quantity::new(eff_ask_q as f64, 0),
                ts,
                ts,
            )));
        }
    }

    Ok(data)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    info!("Starting backtest for date: {}", args.date);

    // --- Load trading config to build token→symbol map ---
    let cfg_raw = std::fs::read_to_string(&args.config)
        .with_context(|| format!("Cannot read {}", args.config))?;
    let cfg: TradingConfig = toml::from_str(&cfg_raw)?;

    let venue = Venue::new("NSE");
    let token_map = build_token_map(&cfg, venue);

    // --- BacktestEngine + simulated venue ---
    let mut engine = BacktestEngine::new(BacktestEngineConfig::default())?;

    engine.add_venue(
        SimulatedVenueConfig::builder()
            .venue(venue)
            .oms_type(OmsType::Netting) // matches live node config
            .account_type(AccountType::Margin)
            .book_type(BookType::L1_MBP)
            // ₹50 lakh starting capital — enough for all five instruments at MIS margin
            .starting_balances(vec![Money::from("5000000 INR")])
            .build(),
    )?;

    // BUG FIX: register every instrument before adding data or strategies.
    // Without this the engine has no instrument definitions and cannot route
    // ticks or validate orders.
    let cost_cfg = CostConfig::load("config/costs.toml");
    info!(
        "Cost model loaded: equity_taker={} futures_taker={} (per-side fraction of notional)",
        cost_cfg.equity_taker_fee, cost_cfg.futures_taker_fee
    );
    for entry in &cfg.instruments {
        let instrument_id = InstrumentId::new(Symbol::new(&entry.symbol), venue);
        let (maker_fee, taker_fee) = cost_cfg.fees_for(&entry.symbol);
        let instrument = make_instrument(instrument_id, maker_fee, taker_fee);
        engine.add_instrument(&instrument)?;
        info!("Registered instrument: {instrument_id} (maker_fee={maker_fee} taker_fee={taker_fee})");
    }

    // --- Load Parquet data ---
    let date_parts: Vec<&str> = args.date.split('-').collect();
    if date_parts.len() != 3 {
        anyhow::bail!("Date must be YYYY-MM-DD, got: {}", args.date);
    }
    let dir = PathBuf::from(&args.data_dir)
        .join(date_parts[0])
        .join(date_parts[1])
        .join(date_parts[2]);

    if !dir.exists() {
        anyhow::bail!(
            "Data directory does not exist: {dir:?}\n\
             Run the live node for one session first to record tick data."
        );
    }

    let mut all_data: Vec<Data> = Vec::new();
    for dir_entry in std::fs::read_dir(&dir)? {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("parquet") {
            continue;
        }
        // Parquet files are named "{token}.parquet" (legacy) or
        // "{token}_{flush_millis}.parquet" (current, timestamped to avoid
        // overwriting data from previous hourly flushes).
        // Extract the token as the part before the first '_' (or the whole stem
        // for legacy single-flush files).
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let token_str = stem.split('_').next().unwrap_or(stem);
        let token: u32 = token_str
            .parse()
            .with_context(|| format!("Parquet filename token part is not an integer: {stem}"))?;
        let instrument_id = match token_map.get(&token) {
            Some(&id) => id,
            None => {
                tracing::warn!(
                    "Token {token} in filename {path:?} not found in config, skipping"
                );
                continue;
            }
        };
        info!("Loading {path:?} → {instrument_id}");
        all_data.append(&mut load_parquet(&path, instrument_id)?);
    }

    if all_data.is_empty() {
        anyhow::bail!(
            "No ticks loaded from {dir:?}. \
             Check that the live node ran on {} and produced parquet files.",
            args.date
        );
    }

    info!(
        "Loaded {} ticks total. Adding to engine with timestamp sort...",
        all_data.len()
    );
    // BUG FIX: sort_timestamps=true — data from multiple files is not in
    // chronological order until sorted here.
    engine.add_data(all_data, None, true, true)?;

    // --- Add strategies (selected via --strategy) ---
    let run_basis = matches!(args.strategy.as_str(), "basis" | "both");
    let run_vwap = matches!(args.strategy.as_str(), "vwap" | "both");
    if !run_basis && !run_vwap {
        anyhow::bail!(
            "--strategy must be one of: basis, vwap, both (got '{}')",
            args.strategy
        );
    }

    if run_basis {
        let basis_params =
            strategy_basis_arb::BasisArbParams::from_file("config/strategy_basis_arb.toml")
                .context("Failed to load config/strategy_basis_arb.toml")?;
        let futures_id = InstrumentId::new(
            Symbol::new(&basis_params.futures_instrument_id.replace(".NSE", "")),
            venue,
        );
        let spot_id = InstrumentId::new(
            Symbol::new(&basis_params.spot_instrument_id.replace(".NSE", "")),
            venue,
        );
        engine.add_strategy(BasisArbStrategy::new(strategy_basis_arb::BasisArbConfig::new(
            basis_params,
            futures_id,
            spot_id,
        )))?;
        info!("Added BasisArbStrategy");
    }

    if run_vwap {
        let vwap_params = strategy_intraday_vwap::IntradayVwapParams::from_file(
            "config/strategy_intraday_vwap.toml",
        )
        .context("Failed to load config/strategy_intraday_vwap.toml")?;
        engine.add_strategy(strategy_intraday_vwap::IntradayVwapStrategy::new(
            strategy_intraday_vwap::IntradayVwapConfig::new(vwap_params),
        ))?;
        info!("Added IntradayVwapStrategy");
    }

    info!("Running backtest...");
    engine.run(None, None, None, false)?;
    info!("Backtest complete.");

    Ok(())
}
