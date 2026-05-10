//! Physical-settlement risk check (stock F&O).
//!
//! Since SEBI mandated physical settlement for stock options (Oct 2019),
//! any stock-option position held to expiry that is ITM results in delivery
//! of the underlying shares — long call → buy shares; short call → sell shares.
//!
//! **Rule:** stock F&O positions that would result in a net short position
//! at expiry must be flat or closed by
//! `physical_settlement_reject_minutes_before_expiry` minutes before expiry.
//!
//! This check **rejects** new orders that would increase such exposure within
//! the window.  It does not apply to index instruments (cash-settled).
//!
//! # reason: SEBI physical settlement mandate (Oct 2019);
//! nse_fo_specifics.md §Physical Settlement of Stock Options;
//! PHASE_1_CHECKLIST.md §Step 5 check (4).

use chrono::{DateTime, Duration, Utc};
use tracing::warn;

use crate::{RiskCheckResult, RiskRejectionReason};

/// Returns `Rejected` when `physical_settlement=true` and the order is
/// within `reject_minutes_before_expiry` of expiry.
///
/// # Parameters
/// - `symbol`: instrument symbol.
/// - `is_physical_settlement`: from `InstrumentRiskConfig`.
/// - `expiry_utc`: instrument expiry.
/// - `now_utc`: current time (injected from clock — do not call `Utc::now()`
///   directly in replay paths).
/// - `reject_minutes_before_expiry`: threshold from `NseRiskConfig`.
pub fn check_physical_settlement(
    symbol: &str,
    is_physical_settlement: bool,
    expiry_utc: DateTime<Utc>,
    now_utc: DateTime<Utc>,
    reject_minutes_before_expiry: u64,
) -> RiskCheckResult {
    if !is_physical_settlement {
        return RiskCheckResult::Approved; // index instrument — cash settled
    }

    let remaining = expiry_utc.signed_duration_since(now_utc);
    let minutes_remaining = remaining.num_minutes();

    if minutes_remaining < 0 {
        // Already expired — reject all new orders (can't deliver on expired contract).
        return RiskCheckResult::Rejected {
            reason: RiskRejectionReason::PhysicalSettlementRisk {
                symbol: symbol.to_string(),
                minutes_remaining,
            },
            message: format!(
                "[{symbol}] contract has already expired ({minutes_remaining} min); \
                 no new orders accepted"
            ),
        };
    }

    if minutes_remaining < reject_minutes_before_expiry as i64 {
        warn!(
            "[PHYSICAL SETTLEMENT] {symbol}: within {minutes_remaining} min of expiry \
             (threshold={reject_minutes_before_expiry} min). Rejecting order to prevent \
             unintended physical delivery."
        );
        RiskCheckResult::Rejected {
            reason: RiskRejectionReason::PhysicalSettlementRisk {
                symbol: symbol.to_string(),
                minutes_remaining,
            },
            message: format!(
                "[{symbol}] physical-settlement stock F&O within {minutes_remaining} min \
                 of expiry (threshold={reject_minutes_before_expiry} min). \
                 Flatten position before accepting new orders."
            ),
        }
    } else {
        RiskCheckResult::Approved
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn expiry() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 26, 8, 0, 0).unwrap() // 08:00 UTC = 13:30 IST
    }

    const THRESHOLD: u64 = 90; // minutes

    #[test]
    fn non_physical_instrument_is_always_approved() {
        let now = expiry() - Duration::minutes(30); // inside threshold
        let result =
            check_physical_settlement("NIFTY26JUNFUT", false, expiry(), now, THRESHOLD);
        assert_eq!(result, RiskCheckResult::Approved);
    }

    #[test]
    fn physical_outside_window_is_approved() {
        let now = expiry() - Duration::minutes(120); // 2 hours before expiry
        let result =
            check_physical_settlement("RELIANCE26JUN3000CE", true, expiry(), now, THRESHOLD);
        assert_eq!(result, RiskCheckResult::Approved);
    }

    #[test]
    fn physical_at_threshold_boundary_is_approved() {
        // exactly 90 min remaining — not < 90, so should be approved
        let now = expiry() - Duration::minutes(90);
        let result =
            check_physical_settlement("RELIANCE26JUN3000CE", true, expiry(), now, THRESHOLD);
        assert_eq!(result, RiskCheckResult::Approved);
    }

    #[test]
    fn physical_inside_window_is_rejected() {
        let now = expiry() - Duration::minutes(89);
        let result =
            check_physical_settlement("RELIANCE26JUN3000CE", true, expiry(), now, THRESHOLD);
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }

    #[test]
    fn physical_already_expired_is_rejected() {
        let now = expiry() + Duration::minutes(5);
        let result =
            check_physical_settlement("RELIANCE26JUN3000CE", true, expiry(), now, THRESHOLD);
        assert!(matches!(result, RiskCheckResult::Rejected { .. }));
    }
}
