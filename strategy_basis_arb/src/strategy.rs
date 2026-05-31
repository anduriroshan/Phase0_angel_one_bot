//! Basis-arb strategy implementation.
//!
//! Monitors the spread between NIFTY futures mid-price and NIFTY spot
//! index mid-price ("basis").  When the z-score of the rolling basis
//! exceeds a configurable threshold, it submits a 1-lot market order.
//!
//! **Direction logic:**
//! - Basis z-score > +threshold → futures expensive vs. spot → SELL futures
//! - Basis z-score < -threshold → futures cheap vs. spot → BUY futures
//!
//! The strategy never touches orders directly.  It calls `self.submit_order()`
//! which routes through NautilusTrader's risk engine before execution.
//!
//! ## Determinism contract
//! - No `SystemTime::now()`, `Instant::now()`, or `Utc::now()` calls anywhere.
//! - All state is inside `self`; replay of the same tick stream yields bit-
//!   identical orders.
//!
//! See PHASE_1_CHECKLIST.md §Step 6 and agents/strategy_engineer.md.

use std::fmt::Debug;

use nautilus_common::actor::DataActor;
use nautilus_model::{
    data::QuoteTick,
    enums::{OrderSide, TimeInForce},
    types::Quantity,
};
use nautilus_trading::{Strategy, StrategyCore, nautilus_strategy};
use tracing::{debug, info, warn};
use ustr::Ustr;

use crate::{config::BasisArbConfig, rolling_basis::RollingBasis};

/// NIFTY-futures-vs-spot basis-arb strategy.
pub struct BasisArbStrategy {
    pub(crate) core: StrategyCore,
    pub(crate) config: BasisArbConfig,

    /// Rolling window tracking `(futures_mid - spot_mid)`.
    pub(crate) basis_window: RollingBasis,

    /// Last observed futures mid-price (paise; updated every tick).
    pub(crate) futures_mid: Option<f64>,

    /// Last observed spot mid-price (paise; updated every tick).
    pub(crate) spot_mid: Option<f64>,

    /// Number of orders submitted this session (prevents runaway).
    pub(crate) orders_submitted: u64,

    /// Whether we are currently flat (no open position).
    /// Simple guard: only 1 position at a time.
    pub(crate) is_flat: bool,
}

impl BasisArbStrategy {
    /// Creates a new strategy from a `BasisArbConfig`.
    #[must_use]
    pub fn new(config: BasisArbConfig) -> Self {
        // Capacity: window_secs × 120 ticks/s (conservative upper bound).
        let capacity = (config.params.window_secs as usize).max(1) * 120;
        Self {
            core: StrategyCore::new(config.base.clone()),
            config,
            basis_window: RollingBasis::new(capacity),
            futures_mid: None,
            spot_mid: None,
            orders_submitted: 0,
            is_flat: true,
        }
    }

    /// Computes mid-price from a quote tick.
    /// For index instruments (NIFTY spot) where bid/ask are zero, falls back
    /// to the last-traded-price stored in `bid_price` (NautilusTrader maps the
    /// LTP into the quote when both sides are zero).
    fn mid(quote: &QuoteTick) -> f64 {
        let bid = quote.bid_price.as_f64();
        let ask = quote.ask_price.as_f64();
        if bid > 0.0 && ask > 0.0 {
            (bid + ask) / 2.0
        } else if bid > 0.0 {
            bid
        } else if ask > 0.0 {
            ask
        } else {
            // Both zero — should not happen for tradeable instruments,
            // but return 0.0 so callers can skip this tick.
            0.0
        }
    }

    /// Attempts to compute the current basis and emit a signal if the
    /// z-score threshold is breached.
    ///
    /// Called after every tick on either instrument.
    fn on_both_prices_updated(&mut self) -> anyhow::Result<()> {
        let (fut, spot) = match (self.futures_mid, self.spot_mid) {
            (Some(f), Some(s)) => (f, s),
            _ => return Ok(()), // need both prices
        };

        // Sanity check: futures and spot must be in the same ballpark.
        // NIFTY futures trade near spot (typical premium < 2%).
        // If they differ by > 5%, one side has stale/garbage data — skip.
        let ratio = fut / spot;
        if ratio < 0.95 || ratio > 1.05 {
            debug!(
                "BASIS_ARB: skipping tick — price mismatch fut={fut:.2} spot={spot:.2} ratio={ratio:.4}"
            );
            return Ok(());
        }

        let basis = fut - spot;
        self.basis_window.push(basis);

        if self.basis_window.count() < self.config.params.min_samples {
            debug!(
                "BASIS_ARB: warming up ({}/{} samples)",
                self.basis_window.count(),
                self.config.params.min_samples
            );
            return Ok(());
        }

        let mean = match self.basis_window.mean() {
            Some(m) => m,
            None => return Ok(()),
        };
        let std_dev = match self.basis_window.std_dev() {
            Some(s) => s,
            None => return Ok(()),
        };
        let z = match self.basis_window.z_score(basis) {
            Some(z) => z,
            None => return Ok(()),
        };

        debug!(
            "BASIS_ARB: basis={basis:.2} mean={mean:.2} std={std_dev:.2} z={z:.3}"
        );

        // Guard: basis variance too small → z is numerically unstable (warm-up).
        if std_dev < self.config.params.min_std_dev {
            debug!(
                "BASIS_ARB: skip — std_dev {std_dev:.4} < min_std_dev {:.4}",
                self.config.params.min_std_dev
            );
            return Ok(());
        }

        // Guard: absurd z-score → warm-up / near-zero-variance artifact, not a
        // real dislocation (see NEXT_STEPS: the z=-24 open artifact).
        if z.abs() > self.config.params.max_z_score {
            warn!(
                "BASIS_ARB: skip — |z| {:.2} > max_z_score {:.2} (warm-up artifact?)",
                z.abs(),
                self.config.params.max_z_score
            );
            return Ok(());
        }

        if !self.is_flat {
            return Ok(()); // already in a position — wait for exit signal
        }

        let threshold = self.config.params.z_score_threshold;
        if z.abs() < threshold {
            return Ok(());
        }

        let side = if z > 0.0 {
            OrderSide::Sell // futures rich → sell futures
        } else {
            OrderSide::Buy // futures cheap → buy futures
        };

        self.submit_signal(side, basis, mean, std_dev, z)?;
        Ok(())
    }

    /// Builds and submits a market order with a structured rationale tag.
    fn submit_signal(
        &mut self,
        side: OrderSide,
        basis: f64,
        mean: f64,
        std_dev: f64,
        z_score: f64,
    ) -> anyhow::Result<()> {
        let instrument_id = self.config.futures_id;
        let qty = Quantity::new(self.config.params.trade_qty_units as f64, 0);

        // Rationale tag (structured, searchable in logs).
        // Format: "basis_arb|z={z:.3}|basis={basis:.2}|mean={mean:.2}|std={std:.2}|side={side}"
        let rationale = format!(
            "basis_arb|z={z_score:.3}|basis={basis:.2}|mean={mean:.2}|std={std_dev:.2}|side={side:?}|threshold={}|window_secs={}",
            self.config.params.z_score_threshold,
            self.config.params.window_secs,
        );
        let tags = vec![Ustr::from(&rationale)];

        info!(
            "BASIS_ARB: signal emitted — {side:?} {qty} NIFTY futures | {rationale}"
        );

        let order = self.core.order_factory().market(
            instrument_id,
            side,
            qty,
            Some(TimeInForce::Day),
            None,          // reduce_only
            None,          // quote_quantity
            None,          // exec_algorithm_id
            None,          // exec_algorithm_params
            Some(tags),    // rationale in tags
            None,          // client_order_id (auto-generated)
        );

        self.submit_order(order, None, None, None)?;
        self.orders_submitted += 1;
        self.is_flat = false;
        Ok(())
    }
}

nautilus_strategy!(BasisArbStrategy);

impl Debug for BasisArbStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasisArbStrategy")
            .field("futures_id", &self.config.futures_id)
            .field("spot_id", &self.config.spot_id)
            .field("window_secs", &self.config.params.window_secs)
            .field("z_threshold", &self.config.params.z_score_threshold)
            .field("orders_submitted", &self.orders_submitted)
            .finish()
    }
}

impl DataActor for BasisArbStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        info!(
            "BasisArbStrategy starting: futures={} spot={} window={}s z_threshold={}",
            self.config.futures_id,
            self.config.spot_id,
            self.config.params.window_secs,
            self.config.params.z_score_threshold,
        );
        // Pre-allocate is done in `new`; nothing else to do here.
        self.subscribe_quotes(self.config.futures_id, None, None);
        self.subscribe_quotes(self.config.spot_id, None, None);
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        info!(
            "BasisArbStrategy stopping: {} orders submitted this session",
            self.orders_submitted
        );
        self.unsubscribe_quotes(self.config.futures_id, None, None);
        self.unsubscribe_quotes(self.config.spot_id, None, None);
        Ok(())
    }

    fn on_quote(&mut self, quote: &QuoteTick) -> anyhow::Result<()> {
        let mid = Self::mid(quote);

        // Skip ticks with no valid price data.
        if mid == 0.0 {
            return Ok(());
        }

        if quote.instrument_id == self.config.futures_id {
            self.futures_mid = Some(mid);
        } else if quote.instrument_id == self.config.spot_id {
            self.spot_mid = Some(mid);
        } else {
            warn!(
                "BASIS_ARB: unexpected instrument {} in on_quote",
                quote.instrument_id
            );
            return Ok(());
        }

        self.on_both_prices_updated()
    }
}
