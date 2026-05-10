//! Lot-size check: order quantity must be a whole multiple of the instrument's
//! lot size.
//!
//! NSE rejects orders whose `qty % lot_size != 0` with "invalid quantity".
//! This pre-trade gate catches that before the REST round-trip.
//!
//! # reason: NSE lot-size validation per nse_fo_specifics.md §Lot Sizes.

use crate::{RiskCheckResult, RiskRejectionReason};

/// Returns `Ok(())` when `qty` is a whole-number multiple of `lot_size`.
///
/// Returns `Rejected` with [`RiskRejectionReason::InvalidLotSize`] otherwise.
#[must_use]
pub fn check_lot_size(qty: u64, lot_size: u64, symbol: &str) -> RiskCheckResult {
    if lot_size == 0 {
        return RiskCheckResult::Rejected {
            reason: RiskRejectionReason::InvalidLotSize {
                qty,
                lot_size,
                symbol: symbol.to_string(),
            },
            message: format!("[{symbol}] lot_size=0 is invalid in config"),
        };
    }
    if qty % lot_size != 0 {
        RiskCheckResult::Rejected {
            reason: RiskRejectionReason::InvalidLotSize {
                qty,
                lot_size,
                symbol: symbol.to_string(),
            },
            message: format!(
                "[{symbol}] qty={qty} is not a multiple of lot_size={lot_size} \
                 (remainder={})",
                qty % lot_size
            ),
        }
    } else {
        RiskCheckResult::Approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_multiple_is_approved() {
        assert_eq!(check_lot_size(75, 75, "NIFTY26JUNFUT"), RiskCheckResult::Approved);
        assert_eq!(check_lot_size(150, 75, "NIFTY26JUNFUT"), RiskCheckResult::Approved);
        assert_eq!(check_lot_size(750, 75, "NIFTY26JUNFUT"), RiskCheckResult::Approved);
    }

    #[test]
    fn fractional_lot_is_rejected() {
        let result = check_lot_size(76, 75, "NIFTY26JUNFUT");
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }

    #[test]
    fn one_unit_below_lot_is_rejected() {
        let result = check_lot_size(74, 75, "NIFTY26JUNFUT");
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }

    #[test]
    fn zero_lot_size_in_config_is_rejected() {
        let result = check_lot_size(75, 0, "NIFTY26JUNFUT");
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }

    #[test]
    fn zero_qty_is_approved_as_multiple_of_any_lot() {
        // qty=0 is (0 % anything) == 0, so lot check passes.
        // A separate min-qty check must guard against zero-qty orders.
        assert_eq!(check_lot_size(0, 75, "NIFTY26JUNFUT"), RiskCheckResult::Approved);
    }
}
