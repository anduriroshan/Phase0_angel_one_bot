//! VWAP mean-reversion intraday strategy implementation.
//!
//! ## Signal logic
//! For each subscribed stock at every tick:
//!
//! 1. Update the session VWAP state (cumulative mean + rolling std).
//! 2. Compute `z = (price − session_mean) / rolling_std`.
//! 3. **Entry** (when flat, before 14:45 IST):
//!    - `z < −threshold` → BUY  (price below average → expect bounce up)
//!    - `z > +threshold` → SELL (price above average → expect pullback)
//! 4. **Exit**:
//!    - Z reverts toward 0 (|z| < `exit_z_threshold`): close position.
//!    - Time ≥ 14:45 IST: force-close to avoid MIS auto-square penalty.
//! 5. **Position size** (signal-strength weighted):
//!    - `base_qty = floor(capital_per_stock / price)`
//!    - `qty = base_qty × min(|z| / threshold, max_signal_multiplier)`
//!    - Capped at `max_qty_per_stock` from config.
//!
//! ## Determinism guarantee
//! - No `SystemTime::now()`, `Instant::now()`, or `Utc::now()`.
//! - All timestamps come from `QuoteTick.ts_event` (NautilusTrader clock).
//! - IST cutoff computed from tick timestamp — deterministic in replay.

use std::{collections::HashMap, fmt::Debug};

use nautilus_common::actor::DataActor;
use nautilus_model::{
    data::QuoteTick,
    enums::{OrderSide, TimeInForce},
    identifiers::InstrumentId,
    types::Quantity,
};
use nautilus_trading::{Strategy, StrategyCore, nautilus_strategy};
use tracing::{debug, info, warn};
use ustr::Ustr;

use crate::{config::IntradayVwapConfig, vwap::SessionVwap};

// ---------------------------------------------------------------------------
// Per-instrument state
// ---------------------------------------------------------------------------

/// Tracks position for a single instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositionSide {
    Flat,
    Long,
    Short,
}

struct InstrumentState {
    vwap: SessionVwap,
    side: PositionSide,
    /// Absolute quantity currently held (always positive; side gives direction).
    qty: u64,
}

impl InstrumentState {
    fn new(window_size: usize) -> Self {
        Self {
            vwap: SessionVwap::new(window_size),
            side: PositionSide::Flat,
            qty: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot extracted from InstrumentState for borrow-checker safety
// ---------------------------------------------------------------------------

/// Decided action — computed from immutable self, executed by mutable self.
enum Action {
    Nothing,
    ClosePosition {
        current_side: PositionSide,
        qty: u64,
        reason: &'static str,
    },
    EntryOrder {
        order_side: OrderSide,
        qty: u64,
        mid: f64,
        z: f64,
        session_mean: f64,
        rolling_std: f64,
        new_side: PositionSide,
    },
}

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

/// VWAP mean-reversion intraday strategy.
///
/// Subscribes to quote ticks for each configured equity instrument and
/// emits MIS market orders when price deviates significantly from the
/// session mean.
pub struct IntradayVwapStrategy {
    pub(crate) core: StrategyCore,
    pub(crate) config: IntradayVwapConfig,
    /// Per-instrument rolling state.
    pub(crate) states: HashMap<InstrumentId, InstrumentState>,
    /// Cumulative order count for this session (logging only).
    pub(crate) orders_submitted: u64,
}

impl IntradayVwapStrategy {
    #[must_use]
    pub fn new(config: IntradayVwapConfig) -> Self {
        let mut states = HashMap::new();
        for &id in &config.instrument_ids {
            states.insert(id, InstrumentState::new(config.params.rolling_window));
        }
        Self {
            core: StrategyCore::new(config.base.clone()),
            config,
            states,
            orders_submitted: 0,
        }
    }

    /// Computes mid-price from a quote tick.
    fn mid(quote: &QuoteTick) -> f64 {
        (quote.bid_price.as_f64() + quote.ask_price.as_f64()) / 2.0
    }

    /// Computes the signal-strength-weighted order quantity.
    fn compute_qty(&self, mid_price: f64, z_abs: f64) -> u64 {
        let p = &self.config.params;
        let base = (p.capital_per_stock_inr / mid_price).floor() as u64;
        if base == 0 {
            return 0;
        }
        let multiplier = (z_abs / p.z_score_threshold).min(p.max_signal_multiplier);
        let qty = ((base as f64) * multiplier).floor() as u64;
        qty.max(1).min(p.max_qty_per_stock)
    }

    /// Decides what action to take — pure read, no mutation.
    /// All mutable execution happens separately so the borrow can be released.
    fn decide(&self, id: InstrumentId, mid: f64, ts_ns: u64) -> Action {
        let state = match self.states.get(&id) {
            Some(s) => s,
            None => return Action::Nothing,
        };
        let p = &self.config.params;

        if state.vwap.session_count() < p.min_samples as u64 {
            debug!("VWAP warm-up {id}: {}/{}", state.vwap.session_count(), p.min_samples);
            return Action::Nothing;
        }

        let session_mean = match state.vwap.session_mean() { Some(v) => v, None => return Action::Nothing };
        let rolling_std  = match state.vwap.rolling_std()  { Some(v) => v, None => return Action::Nothing };
        let z            = match state.vwap.z_score(mid)   { Some(v) => v, None => return Action::Nothing };

        let past_cutoff = SessionVwap::is_past_ist_cutoff(ts_ns, p.exit_hour_ist, p.exit_minute_ist);

        if past_cutoff && state.side != PositionSide::Flat {
            return Action::ClosePosition { current_side: state.side, qty: state.qty, reason: "mis_cutoff" };
        }
        if past_cutoff {
            return Action::Nothing;
        }

        let exit_z = p.exit_z_threshold;
        if state.side == PositionSide::Long && z >= -exit_z {
            return Action::ClosePosition { current_side: state.side, qty: state.qty, reason: "z_reversion" };
        }
        if state.side == PositionSide::Short && z <= exit_z {
            return Action::ClosePosition { current_side: state.side, qty: state.qty, reason: "z_reversion" };
        }

        if state.side == PositionSide::Flat && z.abs() >= p.z_score_threshold {
            let (order_side, new_side) = if z < 0.0 {
                (OrderSide::Buy, PositionSide::Long)
            } else {
                (OrderSide::Sell, PositionSide::Short)
            };
            let qty = self.compute_qty(mid, z.abs());
            if qty == 0 { return Action::Nothing; }
            return Action::EntryOrder { order_side, qty, mid, z, session_mean, rolling_std, new_side };
        }

        Action::Nothing
    }

    /// Executes a close-position action.
    fn execute_close(
        &mut self,
        id: InstrumentId,
        current_side: PositionSide,
        qty: u64,
        reason: &str,
    ) -> anyhow::Result<()> {
        let close_side = match current_side {
            PositionSide::Long  => OrderSide::Sell,
            PositionSide::Short => OrderSide::Buy,
            PositionSide::Flat  => return Ok(()),
        };
        let tags = vec![Ustr::from(&format!("intraday_vwap|close|reason={reason}"))];
        info!("VWAP: EXIT {close_side:?} {qty} {id} | reason={reason}");
        let order = self.core.order_factory().market(
            id, close_side, Quantity::new(qty as f64, 0),
            Some(TimeInForce::Day), None, None, None, None, Some(tags), None,
        );
        self.submit_order(order, None, None, None)?;
        self.orders_submitted += 1;
        if let Some(state) = self.states.get_mut(&id) {
            state.side = PositionSide::Flat;
            state.qty = 0;
        }
        Ok(())
    }

    /// Executes an entry order action.
    fn execute_entry(
        &mut self,
        id: InstrumentId,
        order_side: OrderSide,
        qty: u64,
        mid: f64,
        z: f64,
        session_mean: f64,
        rolling_std: f64,
        new_side: PositionSide,
    ) -> anyhow::Result<()> {
        let p = &self.config.params;
        let rationale = format!(
            "intraday_vwap|z={z:.3}|price={mid:.2}|mean={session_mean:.2}\
             |std={rolling_std:.2}|threshold={}|window={}",
            p.z_score_threshold, p.rolling_window,
        );
        let tags = vec![Ustr::from(&rationale)];
        info!("VWAP: ENTRY {order_side:?} {qty} {id} | {rationale}");
        let order = self.core.order_factory().market(
            id, order_side, Quantity::new(qty as f64, 0),
            Some(TimeInForce::Day), None, None, None, None, Some(tags), None,
        );
        self.submit_order(order, None, None, None)?;
        self.orders_submitted += 1;
        if let Some(state) = self.states.get_mut(&id) {
            state.side = new_side;
            state.qty = qty;
        }
        Ok(())
    }
}

nautilus_strategy!(IntradayVwapStrategy);

impl Debug for IntradayVwapStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntradayVwapStrategy")
            .field("symbols", &self.config.params.symbols)
            .field("z_threshold", &self.config.params.z_score_threshold)
            .field("capital_per_stock", &self.config.params.capital_per_stock_inr)
            .field("orders_submitted", &self.orders_submitted)
            .finish()
    }
}

impl DataActor for IntradayVwapStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        info!(
            "IntradayVwapStrategy starting: {} instruments, z_threshold={}, capital=₹{}/stock",
            self.config.instrument_ids.len(),
            self.config.params.z_score_threshold,
            self.config.params.capital_per_stock_inr,
        );
        // Collect IDs into a local Vec to release the immutable borrow before
        // calling subscribe_quotes(&mut self).
        let ids: Vec<_> = self.config.instrument_ids.clone();
        for id in ids {
            self.subscribe_quotes(id, None, None);
            info!("  Subscribed to quotes: {id}");
        }
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        info!(
            "IntradayVwapStrategy stopping: {} orders submitted this session",
            self.orders_submitted
        );
        let ids: Vec<_> = self.config.instrument_ids.clone();
        for id in ids {
            self.unsubscribe_quotes(id, None, None);
        }
        Ok(())
    }

    fn on_quote(&mut self, quote: &QuoteTick) -> anyhow::Result<()> {
        let id = quote.instrument_id;

        if !self.config.instrument_ids.contains(&id) {
            return Ok(());
        }

        let mid = Self::mid(quote);
        let ts_ns = quote.ts_event.as_u64();

        // Phase 1: update VWAP state — release mutable borrow before phase 2.
        if let Some(state) = self.states.get_mut(&id) {
            state.vwap.update(mid, ts_ns);
        }

        // Phase 2: decide action from immutable snapshot (no mutable borrow live).
        let action = self.decide(id, mid, ts_ns);

        // Phase 3: execute — mutable borrow is safe now.
        match action {
            Action::Nothing => {}
            Action::ClosePosition { current_side, qty, reason } => {
                self.execute_close(id, current_side, qty, reason)?;
            }
            Action::EntryOrder { order_side, qty, mid, z, session_mean, rolling_std, new_side } => {
                if qty == 0 {
                    warn!("VWAP: computed qty=0 for {id}, skipping signal");
                } else {
                    self.execute_entry(id, order_side, qty, mid, z, session_mean, rolling_std, new_side)?;
                }
            }
        }

        Ok(())
    }
}