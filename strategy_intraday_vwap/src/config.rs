//! Configuration for the VWAP mean-reversion intraday strategy.
//!
//! Loaded from `config/strategy_intraday_vwap.toml`.

use nautilus_model::identifiers::{InstrumentId, StrategyId, Symbol, Venue};
use nautilus_trading::strategy::StrategyConfig;
use serde::{Deserialize, Serialize};

/// Tunable parameters per-run, loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntradayVwapParams {
    /// Schema version — bump when fields are added/removed.
    pub schema_version: u32,

    /// Number of tick prices kept in the rolling std-dev window.
    /// Larger → smoother signal, slower reaction.
    /// Default: 40 ticks (~2 min at 20 ticks/s).
    pub rolling_window: usize,

    /// Z-score of (price − session_mean) / rolling_std required to enter.
    /// Default: 2.0.
    pub z_score_threshold: f64,

    /// Exit when |z| drops below this value (price reverted toward mean).
    /// Default: 0.5.
    pub exit_z_threshold: f64,

    /// Minimum ticks in the session before signals are emitted (warm-up).
    /// Default: 40.
    pub min_samples: usize,

    /// Capital allocated per stock (INR).  Position size is capped so the
    /// notional value ≤ capital_per_stock_inr × max_signal_multiplier.
    /// Default: ₹50,000.
    pub capital_per_stock_inr: f64,

    /// Maximum multiplier applied to the base quantity when signal is strong.
    /// Base qty = floor(capital / price).
    /// Actual qty = base × min(|z| / threshold, max_signal_multiplier).
    /// Default: 2.0 (i.e. 2× capital at 2σ or above).
    pub max_signal_multiplier: f64,

    /// Maximum absolute quantity per stock (shares), regardless of signal.
    /// Safety cap so a runaway price doesn't create an enormous position.
    /// Default: 200.
    pub max_qty_per_stock: u64,

    /// IST hour at which to force-close all open positions (24-hour clock).
    /// Default: 14 (i.e. 14:45 IST → 15 min before Angel One MIS square-off).
    pub exit_hour_ist: u32,

    /// IST minute for the force-close cutoff.
    /// Default: 45.
    pub exit_minute_ist: u32,

    /// List of instrument symbols to trade (e.g. `["INFY", "HCLTECH"]`).
    /// Venue is set separately in `IntradayVwapConfig`.
    pub symbols: Vec<String>,

    /// Venue name for all instruments (e.g. `"NSE"`).
    pub venue: String,
}

impl Default for IntradayVwapParams {
    fn default() -> Self {
        Self {
            schema_version: 1,
            rolling_window: 40,
            z_score_threshold: 2.0,
            exit_z_threshold: 0.5,
            min_samples: 40,
            capital_per_stock_inr: 50_000.0,
            max_signal_multiplier: 2.0,
            max_qty_per_stock: 200,
            exit_hour_ist: 14,
            exit_minute_ist: 45,
            symbols: vec![
                "INFY".to_string(),
                "HCLTECH".to_string(),
                "SUNPHARMA".to_string(),
            ],
            venue: "NSE".to_string(),
        }
    }
}

impl IntradayVwapParams {
    /// Loads from a TOML file at `path`.
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let params: Self = toml::from_str(&text)?;
        Ok(params)
    }

    /// Resolves the `symbols` list into `InstrumentId`s.
    pub fn instrument_ids(&self) -> Vec<InstrumentId> {
        let venue = Venue::new(&self.venue);
        self.symbols
            .iter()
            .map(|s| InstrumentId::new(Symbol::new(s), venue))
            .collect()
    }
}

/// Full config for `IntradayVwapStrategy`.
#[derive(Debug, Clone)]
pub struct IntradayVwapConfig {
    pub base: StrategyConfig,
    pub params: IntradayVwapParams,
    pub instrument_ids: Vec<InstrumentId>,
}

impl IntradayVwapConfig {
    #[must_use]
    pub fn new(params: IntradayVwapParams) -> Self {
        let instrument_ids = params.instrument_ids();
        Self {
            base: StrategyConfig {
                strategy_id: Some(StrategyId::from("INTRADAY_VWAP-001")),
                order_id_tag: Some("002".to_string()),
                ..Default::default()
            },
            params,
            instrument_ids,
        }
    }
}
