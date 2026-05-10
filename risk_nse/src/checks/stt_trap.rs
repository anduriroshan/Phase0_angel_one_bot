//! STT-trap warning check.
//!
//! If a long option position is within `stt_trap_warning_minutes_before_expiry`
//! minutes of expiry, this check emits a `SttTrapWarning` event.
//!
//! It does **not** reject the order — the position should have been closed
//! already, so the warning fires when an active long-option position is
//! detected close to expiry.  The executor or operator must intervene.
//!
//! # Rationale
//! STT on option exercise (0.125% of settlement value) is ~20× higher than
//! STT on closing via sale (0.0625% of premium).  Holding a long option to
//! auto-exercise is therefore almost always economically irrational for retail
//! lot sizes.  See nse_fo_specifics.md §The STT Trap.
//!
//! # reason: STT trap warning per PHASE_1_CHECKLIST.md §Step 5 check (3).

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Asia::Kolkata;
use tracing::warn;

/// Outcome of the STT-trap check.
#[derive(Debug, Clone, PartialEq)]
pub enum SttTrapCheckResult {
    /// No warning — position is either not close to expiry or not a long option.
    Clear,
    /// Warning emitted — operator should close the position before expiry.
    Warning {
        symbol: String,
        minutes_remaining: i64,
        threshold_minutes: u64,
    },
}

/// Checks whether a long-option position is dangerously close to expiry.
///
/// # Parameters
/// - `symbol`: instrument symbol (for logging).
/// - `expiry_utc`: expiry timestamp in UTC.
/// - `now_utc`: current time in UTC (injected — never call `Utc::now()` in
///   replay paths; pass `clock.timestamp_ns()` converted to `DateTime<Utc>`).
/// - `threshold_minutes`: warning threshold from `NseRiskConfig`.
///
/// # Returns
/// [`SttTrapCheckResult::Warning`] if `minutes_remaining < threshold_minutes`.
pub fn check_stt_trap(
    symbol: &str,
    expiry_utc: DateTime<Utc>,
    now_utc: DateTime<Utc>,
    threshold_minutes: u64,
) -> SttTrapCheckResult {
    let remaining = expiry_utc.signed_duration_since(now_utc);
    let minutes_remaining = remaining.num_minutes();

    if minutes_remaining < 0 {
        // Already expired — no point warning.
        return SttTrapCheckResult::Clear;
    }

    if minutes_remaining < threshold_minutes as i64 {
        // Emit a loud warning.  Do NOT reject — it is too late for pre-trade
        // logic; the position must be closed by a separate action.
        warn!(
            "[STT TRAP] {symbol}: long option expires in {minutes_remaining} min \
             (threshold={threshold_minutes} min).  Close before expiry to avoid \
             STT on exercise (~20× premium STT)."
        );
        SttTrapCheckResult::Warning {
            symbol: symbol.to_string(),
            minutes_remaining,
            threshold_minutes,
        }
    } else {
        SttTrapCheckResult::Clear
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn expiry() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 26, 10, 0, 0).unwrap() // 10:00 UTC = 15:30 IST
    }

    #[test]
    fn well_before_expiry_is_clear() {
        let now = expiry() - Duration::minutes(120);
        let result = check_stt_trap("NIFTY26JUN22000CE", expiry(), now, 60);
        assert_eq!(result, SttTrapCheckResult::Clear);
    }

    #[test]
    fn at_threshold_is_warning() {
        // exactly at threshold → still within (< 60 is warning; 60 == 60 is NOT < 60)
        let now = expiry() - Duration::minutes(60);
        let result = check_stt_trap("NIFTY26JUN22000CE", expiry(), now, 60);
        assert_eq!(result, SttTrapCheckResult::Clear); // boundary: not < threshold
    }

    #[test]
    fn inside_threshold_is_warning() {
        let now = expiry() - Duration::minutes(59);
        let result = check_stt_trap("NIFTY26JUN22000CE", expiry(), now, 60);
        assert!(matches!(result, SttTrapCheckResult::Warning { .. }));
    }

    #[test]
    fn already_expired_is_clear() {
        let now = expiry() + Duration::minutes(10);
        let result = check_stt_trap("NIFTY26JUN22000CE", expiry(), now, 60);
        assert_eq!(result, SttTrapCheckResult::Clear);
    }
}
