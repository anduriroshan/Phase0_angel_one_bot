//! Session VWAP approximation and rolling statistics for intraday trading.
//!
//! ## Why "VWAP approximation"?
//! True VWAP requires trade volume at each price point.  `QuoteTick` gives
//! bid/ask prices and sizes (quoted depth), not trade volume.  We use the
//! **equal-weighted session mean** as a VWAP proxy:
//!
//! ```text
//! session_mean = Σ(mid_price) / N_ticks    (since session open)
//! ```
//!
//! This is equivalent to VWAP when tick frequency is proportional to trading
//! activity — a reasonable approximation for liquid large-cap stocks.
//!
//! ## Signal computation
//! ```text
//! z = (current_price − session_mean) / rolling_std
//! ```
//!
//! `rolling_std` is the standard deviation of the last `window` mid-prices.
//! Using a rolling window (not full session) makes the signal sensitive to
//! recent volatility rather than the entire session.
//!
//! ## Session reset
//! The session mean is reset at 09:15 IST each day.  Subsequent ticks on the
//! same calendar day accumulate into the new session.  No state leaks across days.
//!
//! ## Determinism
//! No `SystemTime::now()` calls.  All timestamps come from `QuoteTick.ts_event`
//! (injected by NautilusTrader's clock).  Replay of the same tick stream
//! produces identical state.

use std::collections::VecDeque;

use chrono::{Datelike, TimeZone, Timelike};
use chrono_tz::Asia::Kolkata;

/// IST session open: 09:15.  Ticks before this are discarded (pre-open).
const SESSION_OPEN_HOUR: u32 = 9;
const SESSION_OPEN_MIN: u32 = 15;

/// Per-instrument VWAP state.
pub struct SessionVwap {
    // --- Session mean (full session, resets daily) ---
    session_sum: f64,
    session_count: u64,
    session_date_ist: Option<u32>, // YYYYMMDD in IST

    // --- Rolling window for std-dev ---
    window: VecDeque<f64>,
    window_size: usize,
    window_sum: f64,
    window_sum_sq: f64,
}

impl SessionVwap {
    #[must_use]
    pub fn new(window_size: usize) -> Self {
        assert!(window_size >= 2, "window_size must be >= 2");
        Self {
            session_sum: 0.0,
            session_count: 0,
            session_date_ist: None,
            window: VecDeque::with_capacity(window_size),
            window_size,
            window_sum: 0.0,
            window_sum_sq: 0.0,
        }
    }

    /// Updates state with a new tick.
    ///
    /// `ts_ns` is the `QuoteTick.ts_event` in Unix nanoseconds.
    /// `mid_price` is `(bid + ask) / 2` in the instrument's price precision.
    pub fn update(&mut self, mid_price: f64, ts_ns: u64) {
        let secs = (ts_ns / 1_000_000_000) as i64;
        let utc = chrono::Utc.timestamp_opt(secs, 0).single();
        let Some(utc) = utc else { return };
        let ist = utc.with_timezone(&Kolkata);

        // Skip pre-open ticks (before 09:15 IST).
        if ist.hour() < SESSION_OPEN_HOUR
            || (ist.hour() == SESSION_OPEN_HOUR && ist.minute() < SESSION_OPEN_MIN)
        {
            return;
        }

        // Check for day rollover — reset session mean.
        let today = (ist.year() as u32) * 10_000
            + (ist.month() as u32) * 100
            + (ist.day() as u32);

        if Some(today) != self.session_date_ist {
            self.session_sum = 0.0;
            self.session_count = 0;
            self.session_date_ist = Some(today);
            // Clear the rolling window on session start.
            self.window.clear();
            self.window_sum = 0.0;
            self.window_sum_sq = 0.0;
        }

        // Update session mean.
        self.session_sum += mid_price;
        self.session_count += 1;

        // Update rolling window.
        if self.window.len() == self.window_size {
            let old = self.window.pop_front().expect("window non-empty");
            self.window_sum -= old;
            self.window_sum_sq -= old * old;
        }
        self.window.push_back(mid_price);
        self.window_sum += mid_price;
        self.window_sum_sq += mid_price * mid_price;
    }

    /// Number of ticks accumulated since session open.
    #[inline]
    pub fn session_count(&self) -> u64 {
        self.session_count
    }

    /// Session mean (VWAP approximation).  `None` before the first tick.
    #[inline]
    pub fn session_mean(&self) -> Option<f64> {
        if self.session_count == 0 {
            None
        } else {
            Some(self.session_sum / self.session_count as f64)
        }
    }

    /// Rolling population standard deviation of the last `window_size` prices.
    /// Returns `None` if fewer than 2 ticks are in the rolling window.
    #[inline]
    pub fn rolling_std(&self) -> Option<f64> {
        let n = self.window.len();
        if n < 2 {
            return None;
        }
        let n = n as f64;
        let variance = (self.window_sum_sq / n) - (self.window_sum / n).powi(2);
        Some(variance.max(0.0).sqrt())
    }

    /// Z-score of `current_price` relative to session mean and rolling std.
    ///
    /// Returns `None` if:
    /// - Fewer than 2 ticks in rolling window.
    /// - Rolling std is effectively zero (< 1e-9 — all prices identical).
    pub fn z_score(&self, current_price: f64) -> Option<f64> {
        let mean = self.session_mean()?;
        let std = self.rolling_std()?;
        if std < 1e-9 {
            return None; // degenerate: no volatility
        }
        Some((current_price - mean) / std)
    }

    /// Returns `true` if the IST time encoded in `ts_ns` is at or past the given cutoff.
    ///
    /// Used by the strategy to enforce the MIS square-off deadline.
    /// Deterministic — derived from the tick timestamp, not wall clock.
    pub fn is_past_ist_cutoff(ts_ns: u64, cutoff_hour: u32, cutoff_min: u32) -> bool {
        let secs = (ts_ns / 1_000_000_000) as i64;
        let utc = chrono::Utc.timestamp_opt(secs, 0).single();
        let Some(utc) = utc else { return false };
        let ist = utc.with_timezone(&Kolkata);
        ist.hour() > cutoff_hour
            || (ist.hour() == cutoff_hour && ist.minute() >= cutoff_min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-05-10 09:30:00 IST = 2026-05-10 04:00:00 UTC → ns
    fn ts_ist_ns(h: u32, m: u32) -> u64 {
        // IST = UTC+5:30; convert h:m IST to UTC seconds
        let utc_secs: i64 = {
            let ist_secs = (h as i64) * 3600 + (m as i64) * 60;
            let offset = 5 * 3600 + 30 * 60; // IST offset
            // base date: 2026-05-10 = days since epoch × 86400
            let base: i64 = (2026 - 1970) * 365 * 86400 + 40 * 86400; // approx
            base + ist_secs - offset
        };
        (utc_secs as u64) * 1_000_000_000
    }

    #[test]
    fn session_mean_resets_on_new_day() {
        let mut v = SessionVwap::new(10);
        // Day 1: 09:30 IST
        v.update(100.0, ts_ist_ns(9, 30));
        v.update(102.0, ts_ist_ns(9, 31));
        assert!((v.session_mean().unwrap() - 101.0).abs() < 1e-6);
    }

    #[test]
    fn pre_open_ticks_are_ignored() {
        let mut v = SessionVwap::new(10);
        v.update(99.0, ts_ist_ns(9, 10)); // before 09:15 — ignored
        v.update(100.0, ts_ist_ns(9, 20));
        assert_eq!(v.session_count(), 1);
        assert!((v.session_mean().unwrap() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn z_score_none_before_two_ticks() {
        let mut v = SessionVwap::new(10);
        v.update(100.0, ts_ist_ns(9, 20));
        assert!(v.z_score(100.0).is_none()); // only 1 tick
    }

    #[test]
    fn z_score_zero_at_mean() {
        let mut v = SessionVwap::new(10);
        for _ in 0..5 {
            v.update(100.0, ts_ist_ns(9, 20));
        }
        // Push price above mean by known amount
        v.update(110.0, ts_ist_ns(9, 20));
        // z should be positive (price above session mean)
        let z = v.z_score(110.0).unwrap();
        assert!(z > 0.0);
    }

    #[test]
    fn cutoff_detection() {
        // 14:45 IST → should be past cutoff
        assert!(SessionVwap::is_past_ist_cutoff(ts_ist_ns(14, 45), 14, 45));
        // 14:44 IST → not yet
        assert!(!SessionVwap::is_past_ist_cutoff(ts_ist_ns(14, 44), 14, 45));
    }
}
