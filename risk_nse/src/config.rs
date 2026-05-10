//! NSE F&O pre-trade risk configuration.
//!
//! All thresholds are loaded from `config/nse_risk.toml` at startup.
//! **No threshold is hardcoded in logic code** — the config file is the
//! single source of truth for every number, per the risk-engineer agent rules.
//!
//! A default config (suitable for paper trading) ships in `nse_risk.toml`.
//! Live traders **must** review and override before flipping
//! `ANGEL_ONE_DRY_RUN=false`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// InstrumentRiskConfig
// ---------------------------------------------------------------------------

/// Per-instrument risk parameters loaded from `[instruments.<symbol>]` in
/// `nse_risk.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InstrumentRiskConfig {
    /// Lot size (number of underlying units per lot).
    ///
    /// # reason: lot-size validation ensures we never submit a fractional-lot
    /// order which would be rejected by NSE with "invalid quantity."
    pub lot_size: u64,

    /// NSE per-order freeze quantity (in units, not lots).
    ///
    /// NSE rejects single orders whose quantity exceeds this limit.
    /// A hard cap of 80% of freeze_qty is applied before submission.
    ///
    /// # reason: NSE EXCH_ORDER_QUANTITY_FREEZE rejection; see nse_fo_specifics.md.
    pub freeze_qty: u64,

    /// Whether this instrument uses physical settlement at expiry (stock F&O).
    ///
    /// When `true`, the short-into-expiry-week check applies.
    ///
    /// # reason: SEBI mandated physical settlement for stock options (Oct-2019);
    /// see nse_fo_specifics.md §Physical Settlement.
    pub physical_settlement: bool,
}

// ---------------------------------------------------------------------------
// NseRiskConfig
// ---------------------------------------------------------------------------

/// Top-level NSE risk configuration loaded from `config/nse_risk.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NseRiskConfig {
    /// Maximum single-order notional (₹).
    ///
    /// Orders whose `price × qty` exceeds this are rejected pre-trade.
    ///
    /// # reason: fat-finger protection independent of freeze_qty;
    /// provides a money-denominated cap rather than a share-count cap.
    pub max_order_notional_inr: f64,

    /// Maximum number of lots per instrument held at any time.
    ///
    /// Checked post-fill as well as pre-trade.
    ///
    /// # reason: position concentration limit; see nse_fo_specifics.md §Margins.
    pub max_position_lots: u64,

    /// Minutes before expiry at which long ITM options trigger an STT-trap
    /// warning.  Default: 60 (1 hour before expiry).
    ///
    /// # reason: STT on exercise is ~20× higher than STT on sale;
    /// see nse_fo_specifics.md §The STT Trap on Option Exercise.
    pub stt_trap_warning_minutes_before_expiry: u64,

    /// Minutes before expiry at which stock F&O positions that would result
    /// in physical delivery are force-rejected.  Default: 90.
    ///
    /// # reason: SEBI physical settlement mandate (Oct 2019);
    /// see nse_fo_specifics.md §Physical Settlement of Stock Options.
    pub physical_settlement_reject_minutes_before_expiry: u64,

    /// Per-instrument overrides.  Key is the NautilusTrader symbol string
    /// (e.g. `"NIFTY26JUNFUT"`).
    #[serde(default)]
    pub instruments: HashMap<String, InstrumentRiskConfig>,
}

impl Default for NseRiskConfig {
    fn default() -> Self {
        Self {
            max_order_notional_inr: 50_000_000.0, // ₹5 crore — paper-trade safe
            max_position_lots: 50,
            stt_trap_warning_minutes_before_expiry: 60,
            physical_settlement_reject_minutes_before_expiry: 90,
            instruments: HashMap::new(),
        }
    }
}

impl NseRiskConfig {
    /// Loads configuration from a TOML file at `path`.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or parsed.
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&text)?;
        Ok(config)
    }

    /// Returns the per-instrument config for `symbol`, if present.
    pub fn instrument(&self, symbol: &str) -> Option<&InstrumentRiskConfig> {
        self.instruments.get(symbol)
    }
}
