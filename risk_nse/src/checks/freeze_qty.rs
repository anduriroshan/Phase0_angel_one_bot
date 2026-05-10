//! Freeze-quantity check: single-order quantity must not exceed the NSE
//! per-instrument freeze quantity.
//!
//! NSE rejects any single order above the freeze quantity with
//! `EXCH_ORDER_QUANTITY_FREEZE`.  We apply a hard cap at 80% of the
//! configured freeze quantity (conservative pre-trade gate).
//!
//! # reason: NSE freeze qty per nse_fo_specifics.md §Freeze Quantity.

use crate::{RiskCheckResult, RiskRejectionReason};

/// Percentage of the exchange freeze limit at which we cap orders.
///
/// 80 % leaves headroom below the NSE hard limit, absorbing minor
/// config-drift between NSE circulars and our loaded `nse_risk.toml`.
///
/// # reason: 80% cap documented in PHASE_1_CHECKLIST.md §Step 5.
const FREEZE_CAP_PCT: f64 = 0.80;

/// Returns `Ok(())` when `qty` is within 80 % of `freeze_qty`.
///
/// Returns `Rejected` with [`RiskRejectionReason::ExceedsFreeze`] otherwise.
#[must_use]
pub fn check_freeze_qty(qty: u64, freeze_qty: u64, symbol: &str) -> RiskCheckResult {
    let cap = (freeze_qty as f64 * FREEZE_CAP_PCT).floor() as u64;
    if qty > cap {
        RiskCheckResult::Rejected {
            reason: RiskRejectionReason::ExceedsFreeze {
                qty,
                freeze_qty,
                cap,
                symbol: symbol.to_string(),
            },
            message: format!(
                "[{symbol}] qty={qty} exceeds 80% freeze cap={cap} \
                 (freeze_qty={freeze_qty})"
            ),
        }
    } else {
        RiskCheckResult::Approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREEZE: u64 = 45_000; // 1800 lots × 25 units (indicative NIFTY)

    #[test]
    fn at_80_pct_is_approved() {
        let cap = (FREEZE as f64 * 0.80).floor() as u64;
        assert_eq!(
            check_freeze_qty(cap, FREEZE, "NIFTY"),
            RiskCheckResult::Approved
        );
    }

    #[test]
    fn above_80_pct_is_rejected() {
        let over = (FREEZE as f64 * 0.80).floor() as u64 + 1;
        let result = check_freeze_qty(over, FREEZE, "NIFTY");
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }

    #[test]
    fn at_freeze_qty_itself_is_rejected() {
        let result = check_freeze_qty(FREEZE, FREEZE, "NIFTY");
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }

    #[test]
    fn below_cap_is_approved() {
        assert_eq!(
            check_freeze_qty(75, FREEZE, "NIFTY"),
            RiskCheckResult::Approved
        );
    }

    #[test]
    fn zero_qty_is_approved() {
        assert_eq!(
            check_freeze_qty(0, FREEZE, "NIFTY"),
            RiskCheckResult::Approved
        );
    }
}
