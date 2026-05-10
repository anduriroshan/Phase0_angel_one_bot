//! Configuration for the basis-arb strategy.
//!
//! All parameters are loaded from `config/strategy_basis_arb.toml`.
//! No tunable is hardcoded — every knob lives here.

use nautilus_model::identifiers::{InstrumentId, StrategyId};
use nautilus_trading::strategy::StrategyConfig;
use serde::{Deserialize, Serialize};

/// Tunable parameters for `BasisArbStrategy`.
///
/// Loaded at startup from `config/strategy_basis_arb.toml`.
/// A schema version bump is required whenever fields are added or removed
/// (see `schema_version` field).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasisArbParams {
    /// Schema version — bump when adding / removing fields to detect stale configs.
    pub schema_version: u32,

    /// Rolling window length in seconds over which the basis mean and variance
    /// are computed.  Default: 60.
    ///
    /// # reason: short window (60 s) reacts to intraday dislocations; longer
    /// windows miss the opportunity window.
    pub window_secs: u64,

    /// Z-score threshold for signal emission.  Default: 2.0.
    ///
    /// A signal is emitted when |z| > threshold.
    /// Increasing reduces trade frequency; decreasing increases noise exposure.
    ///
    /// # reason: 2σ dislocations are statistically uncommon in the NIFTY
    /// basis (~5% of ticks in a typical session), giving acceptable
    /// signal-to-noise ratio without requiring high-frequency execution.
    pub z_score_threshold: f64,

    /// Minimum number of samples required before a signal can be emitted.
    /// Default: 30.
    ///
    /// # reason: z-score is undefined until the window has at least
    /// `min_samples` observations; signals before this point are noise.
    pub min_samples: usize,

    /// NIFTY futures instrument ID (e.g. `"NIFTY26JUNFUT.NSE"`).
    pub futures_instrument_id: String,

    /// NIFTY spot index instrument ID (e.g. `"NIFTY.NSE"`).
    pub spot_instrument_id: String,

    /// Order quantity in units (not lots).  Default: 75 (= 1 NIFTY lot).
    ///
    /// # reason: minimum tradable size; risk_nse enforces lot-size alignment.
    pub trade_qty_units: u64,
}

impl Default for BasisArbParams {
    fn default() -> Self {
        Self {
            schema_version: 1,
            window_secs: 60,
            z_score_threshold: 2.0,
            min_samples: 30,
            futures_instrument_id: "NIFTY26JUNFUT.NSE".to_string(),
            spot_instrument_id: "NIFTY.NSE".to_string(),
            trade_qty_units: 75,
        }
    }
}

impl BasisArbParams {
    /// Loads from a TOML file at `path`.
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let params: Self = toml::from_str(&text)?;
        Ok(params)
    }
}

/// Full configuration for `BasisArbStrategy` — NautilusTrader base + our params.
#[derive(Debug, Clone)]
pub struct BasisArbConfig {
    /// NautilusTrader strategy base config (id, order tag, etc.).
    pub base: StrategyConfig,
    /// Domain-specific tunable parameters.
    pub params: BasisArbParams,
    /// Resolved futures instrument ID.
    pub futures_id: InstrumentId,
    /// Resolved spot instrument ID.
    pub spot_id: InstrumentId,
}

impl BasisArbConfig {
    /// Creates a new config from parsed params.
    #[must_use]
    pub fn new(params: BasisArbParams, futures_id: InstrumentId, spot_id: InstrumentId) -> Self {
        Self {
            base: StrategyConfig {
                strategy_id: Some(StrategyId::from("BASIS_ARB-001")),
                order_id_tag: Some("001".to_string()),
                ..Default::default()
            },
            params,
            futures_id,
            spot_id,
        }
    }
}
