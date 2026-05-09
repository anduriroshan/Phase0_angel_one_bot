# Strategy Engine

> Defines the contract between strategy logic and the rest of the system:
> what a strategy may consume, what it must emit, and what it must never do.

**Status:** Phase 1 (planned). No strategies exist in Phase 0.

---

## Definition

A **strategy** is a pure function of (book state, position state, parameters, time) that emits zero or more `SignalEvent`s.

```rust
pub trait Strategy: Send + Sync {
    fn id(&self) -> &str;
    fn on_event(&mut self, ctx: &StrategyContext, event: &MarketEvent) -> Vec<SignalEvent>;
}

pub struct StrategyContext<'a> {
    pub book: &'a OrderBookCache,
    pub state: &'a StateHandle,
    pub clock: &'a dyn Clock,
    pub params: &'a StrategyParams,
}
```

A signal is **intent**, not action. The execution engine decides what to do with it (subject to risk gating). A strategy that places its own orders is structurally forbidden — see [vision/design_principles.md](../vision/design_principles.md#architecture-principles) principle 8.

---

## Hard Rules

These are enforced by code review, by `agents/strategy_engineer.md`, and by replay smoke tests.

### 1. No I/O
A strategy makes zero network calls, file reads, or syscalls outside the injected context. If you need external data, ingest it as events upstream.

### 2. No wall-clock
A strategy never calls `SystemTime::now()`, `Instant::now()`, or `chrono::Utc::now()`. Use `ctx.clock.now_ns()`. This is what makes the strategy replay-equivalent to live. See [runtime/replay_engine.md](replay_engine.md#core-invariant-identical-code-paths).

### 3. Deterministic randomness
If a strategy uses randomness (Monte Carlo features, exploration), the RNG is seeded from a parameter — not from `OsRng`, not from time. The seed is logged with every signal. Same seed + same input = same output.

### 4. No mutation of context
`book`, `state`, `clock`, `params` are immutable references. The strategy can't modify them. The only output is the returned `Vec<SignalEvent>`.

### 5. Pure function modulo `&mut self`
The strategy may maintain its own internal state (e.g., a moving average buffer). That state is reconstructable by replaying the same events from genesis. No hidden data outside `&mut self`.

### 6. Bounded compute per event
A strategy must return within the configured budget (default: 200µs P95). Strategies that exceed this are logged and, on persistent violation, deactivated. Long-running compute (model retraining, parameter sweeps) belongs in cold-path tasks that update strategy params via signals/events, not inside `on_event`.

### 7. Every signal is justified by event log
Given a complete event log, replaying produces the same signals. There is no "the strategy felt like it." If the test fails, the strategy is non-deterministic and must be fixed.

---

## Hot Path vs Cold Path

The strategy engine has two execution surfaces:

```
Hot path  (per market event, target P95 <200µs):
    on_event(ctx, event) → Vec<SignalEvent>
    – pure, fast, no I/O
    – uses precomputed features only

Cold path (periodic / async, target seconds-to-minutes):
    refresh_params(...) → StrategyParams
    – may do I/O, model inference, queries
    – emits a `StrategyParamUpdated` event into the bus
    – never directly mutates the running strategy
```

The strategy struct itself only sees `StrategyParams` updates as events. Cold-path computations (a CNN inference, a graph query) run in their own task and publish a `StrategyParamUpdated(strategy_id, params)` event. The hot path consumes that event like any other.

This is the **only** way to integrate slow components (Phase 3 LangGraph agents, Phase 2 ML models) without contaminating the hot path. See [from_chatgpt_gemini.md] discussion of "the latency-complexity paradox" — the architectural fix is asynchronous decoupling, not parallel speed.

---

## Lifecycle Hooks

```rust
pub trait Strategy {
    fn id(&self) -> &str;

    fn on_start(&mut self, ctx: &StrategyContext) {}
    // Called once when the strategy is loaded. Use to initialize internal state
    // from history (replay last N events to warm up). NO live I/O.

    fn on_event(&mut self, ctx: &StrategyContext, event: &MarketEvent) -> Vec<SignalEvent>;
    // The hot path. Per-event signal generation.

    fn on_param_update(&mut self, params: &StrategyParams) {}
    // Called when a `StrategyParamUpdated` event arrives.

    fn on_session_start(&mut self) {}
    fn on_session_end(&mut self) {}
    // Daily lifecycle hooks. Use to reset intraday state, snapshot to disk, etc.

    fn on_stop(&mut self, ctx: &StrategyContext) {}
    // Called when the strategy is deregistered. Final cleanup.
}
```

All hooks except `on_event` are infrequent and may take longer. None may do live network I/O — even `on_start` reads from the local event log, not from the broker.

---

## Strategy Registry

The strategy engine holds a registry of registered strategies:

```rust
pub struct StrategyEngine {
    strategies: Vec<Box<dyn Strategy>>,
    ctx: StrategyContext<'static>, // built at startup
}

impl StrategyEngine {
    pub fn register(&mut self, strategy: Box<dyn Strategy>);
    pub fn dispatch(&mut self, event: &MarketEvent) -> Vec<SignalEvent> {
        self.strategies
            .iter_mut()
            .flat_map(|s| s.on_event(&self.ctx, event))
            .collect()
    }
}
```

Strategies are isolated from each other. One strategy's bug does not corrupt another's state.

---

## SignalEvent Schema (Phase 1)

```rust
pub struct SignalEvent {
    pub signal_id: SignalId,        // strategy_id + monotonic counter
    pub strategy_id: String,
    pub ts_ns: i64,                 // ctx.clock.now_ns() at emission
    pub inst_id: i32,
    pub side: Side,                 // Buy | Sell
    pub qty: i64,                   // signed
    pub order_type: OrderType,      // Limit | Market | StopLoss
    pub limit_price_paise: Option<i64>,
    pub time_in_force: TimeInForce, // IOC | DAY | GTT
    pub rationale: SignalRationale, // structured: which features triggered, with values
    pub params_version: u32,
}
```

`rationale` is non-optional. Every signal must record **why** it was emitted: which features crossed which thresholds, with their values at decision time. This is what makes post-hoc analysis possible. A strategy that emits signals without rationale fails review.

Versioning: see [standards/event_contracts.md](../standards/event_contracts.md).

---

## Phase 1 Reference Strategy: Basis Arbitrage

To exercise the engine end-to-end, Phase 1 ships one trivial strategy: NIFTY futures vs. spot index basis monitor.

```text
basis = futures_mid_price - spot_index_price
if basis > rolling_mean + k * rolling_std:  emit Sell(futures), Buy(spot proxy)
if basis < rolling_mean - k * rolling_std:  emit Buy(futures), Sell(spot proxy)
```

Hot-path features (rolling mean/std) are O(1) updates. Cold-path: nothing initially; later, parameter tuning.

This is **not a profitable strategy** as written — NIFTY spot is an index (not directly tradable), so the leg structure has to be approximated by basket of constituents or by index ETF. But it exercises every component: book read, feature compute, signal emit, risk gate, execution, fill, state update. That's the point.

See [examples/signal_to_fill_flow.md](../examples/signal_to_fill_flow.md) for the end-to-end trace.

---

## Reference Architecture: NautilusTrader

| Concept | NautilusTrader path |
|---|---|
| Strategy trait + base | `knowledge/nautilus_trader/crates/trading/src/strategy/` |
| Algorithm wrappers | `knowledge/nautilus_trader/crates/trading/src/algorithm/` |
| Examples | `knowledge/nautilus_trader/crates/trading/src/examples/` |
| Session lifecycle | `knowledge/nautilus_trader/crates/trading/src/sessions.rs` |

**What to steal:** the lifecycle-hook taxonomy, the strategy registry pattern, the rationale-on-every-signal discipline.

---

## Implementation Location (planned)

- `strategy/src/lib.rs` — `Strategy` trait, `StrategyContext`, `StrategyEngine`
- `strategy/src/signal.rs` — `SignalEvent`, `SignalRationale`
- `strategy/src/registry.rs` — registration and dispatch
- `strategy/strategies/basis_arb/` — first reference strategy

---

## See Also

- [agents/strategy_engineer.md](../agents/strategy_engineer.md) — how to write strategies (hard rules in agent form)
- [runtime/event_bus.md](event_bus.md) — `SignalEvent` routing
- [runtime/risk_engine.md](risk_engine.md) — pre-trade gate signals must pass
- [runtime/execution_engine.md](execution_engine.md) — what consumes signals
- [runtime/order_book.md](order_book.md) — what strategies read
- [runtime/state_engine.md](state_engine.md) — position queries
- [runtime/replay_engine.md](replay_engine.md) — strategies must produce identical signals in replay
- [standards/event_contracts.md](../standards/event_contracts.md) — `SignalEvent` versioning
- [glossary.md](../glossary.md) — `Signal`, `SignalEvent` definitions

**Last verified against commit:** _pending Phase 1 implementation_
