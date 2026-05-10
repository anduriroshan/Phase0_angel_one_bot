//! # risk_nse
//!
//! NSE F&O pre-trade risk checks that sit in front of NautilusTrader's own
//! `RiskEngine`.
//!
//! ## Architecture
//!
//! ```text
//! strategy::submit_order()
//!     │
//!     ▼
//! NseRiskCheck::validate()   ← this crate
//!     │
//!     ▼ (Approved)
//! NautilusTrader RiskEngine   ← position / notional / margin checks
//!     │
//!     ▼ (Approved)
//! AngelOneExecutionClient     ← REST submission
//! ```
//!
//! ## Checks run in order
//!
//! 1. **Lot size** — `qty % lot_size == 0`
//! 2. **Freeze quantity** — `qty <= 80% of NSE freeze_qty`
//! 3. **STT trap warning** — long option within N minutes of expiry (warning, not reject)
//! 4. **Physical settlement** — stock F&O order within N minutes of expiry (reject)
//!
//! All thresholds are config-driven; no magic numbers appear in logic code.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let config = NseRiskConfig::from_file("config/nse_risk.toml")?;
//! let risk = NseRiskCheck::new(config);
//!
//! let result = risk.validate(
//!     "NIFTY26JUNFUT",
//!     qty,
//!     None,       // expiry_utc — None for perpetuals / futures without expiry check
//!     now_utc,
//!     false,      // is_physical_settlement
//! );
//! ```

pub mod checks;
pub mod config;

pub use checks::{SttTrapCheckResult, check_freeze_qty, check_lot_size, check_physical_settlement, check_stt_trap};
pub use config::{InstrumentRiskConfig, NseRiskConfig};

use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// RiskRejectionReason
// ---------------------------------------------------------------------------

/// Structured reason codes emitted with every rejection.
///
/// Strategies and dashboards can pattern-match on this for alerting /
/// structured logging.
#[derive(Debug, Clone, PartialEq)]
pub enum RiskRejectionReason {
    /// Order quantity is not a whole multiple of the instrument's lot size.
    ///
    /// # reason: NSE rejects fractional-lot orders.
    InvalidLotSize {
        qty: u64,
        lot_size: u64,
        symbol: String,
    },

    /// Order quantity exceeds 80% of the NSE freeze quantity.
    ///
    /// # reason: NSE EXCH_ORDER_QUANTITY_FREEZE; 80% cap for safety headroom.
    ExceedsFreeze {
        qty: u64,
        freeze_qty: u64,
        cap: u64,
        symbol: String,
    },

    /// Stock F&O order submitted within the physical-settlement rejection window.
    ///
    /// # reason: SEBI physical settlement mandate (Oct 2019).
    PhysicalSettlementRisk {
        symbol: String,
        minutes_remaining: i64,
    },

    /// Instrument has no config in `nse_risk.toml` — unknown lot size / freeze qty.
    UnknownInstrument { symbol: String },
}

// ---------------------------------------------------------------------------
// RiskCheckResult
// ---------------------------------------------------------------------------

/// Outcome of a pre-trade risk check.
///
/// Strategies must treat anything other than `Approved` as a hard stop.
#[derive(Debug, Clone, PartialEq)]
pub enum RiskCheckResult {
    /// Order passed all checks — safe to forward to the execution engine.
    Approved,

    /// Order rejected. Contains a structured reason code and a human-readable message.
    Rejected {
        reason: RiskRejectionReason,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// NseRiskCheck
// ---------------------------------------------------------------------------

/// Pre-trade risk gate for NSE F&O orders.
///
/// Construct once at node startup; call [`validate`](NseRiskCheck::validate)
/// for every `SubmitOrder` command before forwarding to the execution engine.
pub struct NseRiskCheck {
    config: NseRiskConfig,
}

impl NseRiskCheck {
    /// Creates a new `NseRiskCheck` from the given config.
    #[must_use]
    pub fn new(config: NseRiskConfig) -> Self {
        Self { config }
    }

    /// Loads config from the default file path (`config/nse_risk.toml`) and
    /// constructs the check.
    ///
    /// # Errors
    /// Returns an error if the file is missing or malformed.
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let config = NseRiskConfig::from_file(path)?;
        Ok(Self::new(config))
    }

    /// Validates a pending order against all NSE F&O pre-trade rules.
    ///
    /// # Parameters
    /// - `symbol`: NautilusTrader symbol string (used to look up instrument config).
    /// - `qty`: order quantity **in units** (not lots).
    /// - `expiry_utc`: instrument expiry time. Pass `None` for instruments
    ///   where expiry-window checks should be skipped (e.g., equities).
    /// - `now_utc`: current wall-clock time.  **Must be injected** — never call
    ///   `Utc::now()` in the call site for replay correctness.
    /// - `is_physical_settlement`: `true` for stock F&O (physical delivery at expiry).
    ///
    /// # Returns
    /// [`RiskCheckResult::Approved`] if all checks pass.
    /// [`RiskCheckResult::Rejected`] if any hard check fails (lot-size, freeze-qty,
    /// physical-settlement).
    ///
    /// STT-trap is a warning-only check; it does not reject and does not affect
    /// the return value.
    pub fn validate(
        &self,
        symbol: &str,
        qty: u64,
        expiry_utc: Option<DateTime<Utc>>,
        now_utc: DateTime<Utc>,
        is_physical_settlement: bool,
    ) -> RiskCheckResult {
        // Look up instrument config — reject unknown instruments.
        let inst = match self.config.instrument(symbol) {
            Some(c) => c,
            None => {
                tracing::warn!(
                    "[risk_nse] No config for instrument {symbol}; rejecting order \
                     (add to nse_risk.toml)"
                );
                return RiskCheckResult::Rejected {
                    reason: RiskRejectionReason::UnknownInstrument {
                        symbol: symbol.to_string(),
                    },
                    message: format!(
                        "[{symbol}] instrument not found in nse_risk.toml; \
                         cannot validate lot_size or freeze_qty"
                    ),
                };
            }
        };

        // Check 1: lot size.
        let ls = check_lot_size(qty, inst.lot_size, symbol);
        if !matches!(ls, RiskCheckResult::Approved) {
            return ls;
        }

        // Check 2: freeze quantity.
        let fq = check_freeze_qty(qty, inst.freeze_qty, symbol);
        if !matches!(fq, RiskCheckResult::Approved) {
            return fq;
        }

        // Checks 3 & 4 require an expiry timestamp.
        if let Some(expiry) = expiry_utc {
            // Check 3: STT trap warning (non-blocking, just logs).
            check_stt_trap(
                symbol,
                expiry,
                now_utc,
                self.config.stt_trap_warning_minutes_before_expiry,
            );

            // Check 4: physical settlement rejection.
            if is_physical_settlement || inst.physical_settlement {
                let ps = check_physical_settlement(
                    symbol,
                    true,
                    expiry,
                    now_utc,
                    self.config.physical_settlement_reject_minutes_before_expiry,
                );
                if !matches!(ps, RiskCheckResult::Approved) {
                    return ps;
                }
            }
        }

        RiskCheckResult::Approved
    }

    /// Returns a reference to the underlying config.
    pub fn config(&self) -> &NseRiskConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Property tests (proptest)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod prop_tests {
    use chrono::{TimeZone, Utc};
    use proptest::prelude::*;

    use super::*;
    use crate::config::InstrumentRiskConfig;

    fn make_check(lot_size: u64, freeze_qty: u64) -> NseRiskCheck {
        let mut config = NseRiskConfig::default();
        config.instruments.insert(
            "TEST_FUT".to_string(),
            InstrumentRiskConfig {
                lot_size,
                freeze_qty,
                physical_settlement: false,
            },
        );
        NseRiskCheck::new(config)
    }

    proptest! {
        /// Any qty that is NOT a multiple of lot_size must be rejected.
        #[test]
        fn prop_non_multiple_qty_rejected(
            lot_size in 1u64..=100u64,
            base in 0u64..=100u64,
            remainder in 1u64..=100u64,
        ) {
            prop_assume!(remainder < lot_size);
            let qty = base * lot_size + remainder; // guaranteed non-multiple
            // freeze_qty much larger so it doesn't interfere
            let check = make_check(lot_size, qty * 10 + 10_000);
            let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
            let result = check.validate("TEST_FUT", qty, None, now, false);
            prop_assert!(
                matches!(result, RiskCheckResult::Rejected { reason: RiskRejectionReason::InvalidLotSize { .. }, .. }),
                "Expected InvalidLotSize rejection for qty={qty} lot_size={lot_size}"
            );
        }

        /// Any qty that IS a multiple of lot_size, and below the freeze cap,
        /// must be approved (ignoring expiry checks, which are disabled here).
        #[test]
        fn prop_valid_multiple_below_freeze_approved(
            lot_size in 1u64..=100u64,
            lots in 1u64..=100u64,
        ) {
            let qty = lots * lot_size;
            // freeze_qty = 200× qty so cap (80% of that) >> qty
            let freeze_qty = qty * 200;
            let check = make_check(lot_size, freeze_qty);
            let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
            let result = check.validate("TEST_FUT", qty, None, now, false);
            prop_assert_eq!(result, RiskCheckResult::Approved);
        }

        /// Any qty above 80% of freeze_qty must be rejected (even if it's a
        /// valid multiple of lot_size).
        #[test]
        fn prop_above_freeze_cap_rejected(
            lot_size in 1u64..=10u64,
            lots in 1u64..=50u64,
        ) {
            let qty = lots * lot_size;
            // freeze_qty such that qty > 80% of freeze_qty
            // freeze_qty < qty / 0.80 → freeze_qty = qty - 1 (always less)
            let freeze_qty = if qty == 0 { 1 } else { qty.saturating_sub(1) };
            prop_assume!(freeze_qty > 0);
            // Also make sure qty is a multiple of lot_size (it is by construction)
            let check = make_check(lot_size, freeze_qty);
            let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
            let result = check.validate("TEST_FUT", qty, None, now, false);
            // Either lot-size or freeze rejection is acceptable
            prop_assert!(
                matches!(result, RiskCheckResult::Rejected { .. }),
                "Expected rejection for qty={qty} freeze_qty={freeze_qty}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Integration unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use super::*;
    use crate::config::InstrumentRiskConfig;

    fn nifty_check() -> NseRiskCheck {
        let mut config = NseRiskConfig {
            max_order_notional_inr: 50_000_000.0,
            max_position_lots: 50,
            stt_trap_warning_minutes_before_expiry: 60,
            physical_settlement_reject_minutes_before_expiry: 90,
            instruments: Default::default(),
        };
        config.instruments.insert(
            "NIFTY26JUNFUT".to_string(),
            InstrumentRiskConfig {
                lot_size: 75,
                freeze_qty: 45_000,
                physical_settlement: false,
            },
        );
        config.instruments.insert(
            "RELIANCE26JUN3000CE".to_string(),
            InstrumentRiskConfig {
                lot_size: 250,
                freeze_qty: 10_000,
                physical_settlement: true,
            },
        );
        NseRiskCheck::new(config)
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 4, 0, 0).unwrap() // 09:30 IST, well before expiry
    }

    fn expiry() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 26, 10, 0, 0).unwrap() // 15:30 IST on expiry day
    }

    // ---- Happy path ----

    #[test]
    fn valid_nifty_order_approved() {
        let check = nifty_check();
        let result = check.validate("NIFTY26JUNFUT", 75, Some(expiry()), now(), false);
        assert_eq!(result, RiskCheckResult::Approved);
    }

    #[test]
    fn valid_multi_lot_nifty_approved() {
        let check = nifty_check();
        let result = check.validate("NIFTY26JUNFUT", 750, Some(expiry()), now(), false); // 10 lots
        assert_eq!(result, RiskCheckResult::Approved);
    }

    // ---- Lot size failures ----

    #[test]
    fn fractional_lot_rejected() {
        let check = nifty_check();
        let result = check.validate("NIFTY26JUNFUT", 76, Some(expiry()), now(), false);
        assert!(matches!(
            result,
            RiskCheckResult::Rejected { reason: RiskRejectionReason::InvalidLotSize { .. }, .. }
        ));
    }

    #[test]
    fn one_below_lot_rejected() {
        let check = nifty_check();
        let result = check.validate("NIFTY26JUNFUT", 74, Some(expiry()), now(), false);
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }

    // ---- Freeze quantity failures ----

    #[test]
    fn exceeds_80pct_freeze_cap_rejected() {
        let check = nifty_check();
        // 80% of 45000 = 36000; 36001 should fail
        let result = check.validate("NIFTY26JUNFUT", 36_075, Some(expiry()), now(), false); // 481 lots, just over cap
        assert!(matches!(
            result,
            RiskCheckResult::Rejected { reason: RiskRejectionReason::ExceedsFreeze { .. }, .. }
        ));
    }

    #[test]
    fn at_80pct_freeze_cap_approved() {
        let check = nifty_check();
        // 80% of 45000 = 36000; nearest multiple of 75 at or below: 36000 (36000/75=480 lots)
        let result = check.validate("NIFTY26JUNFUT", 36_000, Some(expiry()), now(), false);
        assert_eq!(result, RiskCheckResult::Approved);
    }

    // ---- Physical settlement ----

    #[test]
    fn stock_option_inside_expiry_window_rejected() {
        let check = nifty_check();
        let close_to_expiry = expiry() - Duration::minutes(30); // 30 min before expiry
        let result = check.validate(
            "RELIANCE26JUN3000CE",
            250, // 1 lot
            Some(expiry()),
            close_to_expiry,
            false, // is_physical_settlement already in config
        );
        assert!(matches!(
            result,
            RiskCheckResult::Rejected {
                reason: RiskRejectionReason::PhysicalSettlementRisk { .. },
                ..
            }
        ));
    }

    #[test]
    fn stock_option_before_window_approved() {
        let check = nifty_check();
        let early = expiry() - Duration::minutes(120); // 2 hours before expiry
        let result = check.validate("RELIANCE26JUN3000CE", 250, Some(expiry()), early, false);
        assert_eq!(result, RiskCheckResult::Approved);
    }

    // ---- Unknown instrument ----

    #[test]
    fn unknown_instrument_rejected() {
        let check = nifty_check();
        let result = check.validate("UNKNOWN_FUT", 100, None, now(), false);
        assert!(matches!(
            result,
            RiskCheckResult::Rejected {
                reason: RiskRejectionReason::UnknownInstrument { .. },
                ..
            }
        ));
    }
}
