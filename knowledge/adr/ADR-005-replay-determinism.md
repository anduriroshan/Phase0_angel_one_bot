# ADR-005: Replay Determinism — What We Guarantee, What We Don't

**Status:** Accepted
**Date:** 2026-05-09
**Decision makers:** rosha
**Supersedes:** —

---

## Context

The system philosophy ([axiom 3](../vision/system_philosophy.md#3-replayability-as-a-first-class-requirement)) requires that every trading session be replayable from stored data. Without a precise statement of what "replayable" means, the requirement is unenforceable — and worse, can be used to mean different things by different components.

This ADR defines exactly what determinism the replay engine guarantees and, importantly, what it does not.

## Decision

The replay engine guarantees **bit-identical signals and orders** when replaying the same input data through the same strategy code with the same parameters and seed. It does **not** guarantee that replay PnL matches live PnL — that gap is a deliberately measured property of the strategy.

### Guaranteed (any replay run)

1. **Strategy outputs are deterministic.** Given `(event_log, strategy_params, seed)`, the sequence of `SignalEvent`s emitted is bit-identical run-to-run.
2. **Order outputs are deterministic.** Given the above plus `(risk_params, fill_model)`, the sequence of `OrderEvent`s and simulated `FillEvent`s is bit-identical.
3. **State projection is deterministic.** Given the above, the final `Portfolio` state is bit-identical.
4. **Simulated PnL is deterministic.** Bit-identical realized + unrealized PnL on every run with the same inputs.

### Not Guaranteed (and explicitly out of scope)

1. **Replay PnL == Live PnL.** The fill model approximates broker behavior. Real fills are subject to:
   - Queue position dynamics we cannot observe (no L3 data)
   - Network jitter at order submission
   - Cross-trading on the same tick (other participants moving the book)
   - Broker-side rounding, fees, STT computation differences
   The expected gap is measured per-strategy as a Phase 1 deliverable; an unexpectedly large gap signals strategy fragility.

2. **Replay timing matches live timing.** Replay processes events as fast as possible (subject to component compute). Wall-clock pacing is not preserved. This is intentional: backtests run faster than realtime.

3. **Cross-process replay.** The circuit breaker is a separate process; in replay it is **not** spawned. Heartbeat-loss scenarios are tested separately.

## Mechanism

### Injected Clock

No component calls `SystemTime::now()`. All "current time" reads go through:

```rust
pub trait Clock: Send + Sync {
    fn now_ns(&self) -> i64;
}
```

`LiveClock` wraps the OS clock. `ReplayClock` is driven by the next event's `ts_ns`. The clock is part of `StrategyContext`, `RiskContext`, and `ExecutionContext`. No global clock; no convenience method. This is the single mechanism that makes determinism work.

### Seeded Randomness

If a component uses RNG (Monte Carlo features, exploration), the seed is in `StrategyParams`. Same seed = same draws. `OsRng`, `thread_rng` are forbidden in replayable code paths. Tested via lint or grep-based CI check.

### Deterministic Iteration

`HashMap` iteration order is randomized per build. Code that needs a deterministic iteration order over an `HashMap` must:

```rust
let mut keys: Vec<_> = map.keys().collect();
keys.sort();
for k in keys { /* deterministic */ }
```

`BTreeMap` is preferred when iteration order matters intrinsically.

### Deterministic Fill Model

The `FillModel` is a pure function of `(order, book_at_ts, fill_model_params)`. No internal state. No randomness except what's seeded from `params`. The same inputs always produce the same simulated fill.

## Alternatives Considered

| Alternative | Why Rejected |
|---|---|
| **No formal determinism guarantee** | Backtests become subjective; "the result moved a bit" has no diagnosis. Determinism is the precondition of all other rigor. |
| **Wall-clock-paced replay** | Drastically slower (real-time backtests over months impossible). Adds a temporal coupling that doesn't make replay any more correct. |
| **Replay PnL == Live PnL as a target** | Unattainable without L3 data, exact broker queue dynamics, exact network jitter. Pretending it's possible misleads strategy evaluation. The gap is the data. |
| **Deterministic `HashMap` (e.g., `IndexMap`) everywhere** | Adds a dependency to gain a property that careful code already provides. Use `BTreeMap` where ordering matters; sort on iteration where it doesn't. |
| **Record live fills and re-inject during replay** | Useful for audit ("what did the system do?") but contaminates research replays ("what would a different strategy do?"). Phase 2 ships both modes; ADR-005 covers research replay; live-replay audit is a separate ADR (TBD). |
| **Skip seeded RNG, accept "close enough"** | Once you allow non-determinism, you can't tell whether a parameter change moved the result or whether RNG did. Strict determinism is cheaper than the alternative. |

## Tradeoffs

**Advantages:**
- Diagnostic clarity: a behavior change in replay output means **the code or data changed**, never "the run got lucky/unlucky."
- Trust: bit-identical results unblock CI assertions on numeric values, not just on shape.
- Forces good architecture: every component must declare its time and randomness dependencies, which is good documentation regardless.

**Disadvantages:**
- The discipline is contagious: a single `SystemTime::now()` in a previously-untested helper breaks the property silently. Mitigated by lint/grep checks and replay smoke tests.
- The `Clock` trait threads through more code than seems necessary. Acceptable: it's a small price for the property it gives us.
- Some legitimate uses of wall-clock (e.g., logging "now") are awkward. Convention: logging uses `chrono::Local::now()` directly because logs aren't replayable; business logic uses `clock.now_ns()`.

## Consequences

- **Hard rule for strategy/risk/execution code**: no `SystemTime`, no `Instant`, no `Utc::now()`. Use `clock.now_ns()`. Caught in code review (see [agents/reviewer.md](../agents/reviewer.md)) and ideally by a CI grep check.
- **Replay smoke test**: every CI run replays a fixed historical session and asserts the result hash matches the previous run. Any drift fails the build.
- **Strategy authors must justify any RNG**: an ADR or comment explaining the seed source. Default: no RNG.
- **Live-replay PnL gap is a tracked metric**: each strategy publishes its `(replay_pnl, live_pnl)` per session. A widening gap is investigated, not ignored.
- **The `Clock` trait lives in `common`**: shared between live and replay binaries. `LiveClock` and `ReplayClock` are the only two implementations.

---

## See Also

- [vision/system_philosophy.md](../vision/system_philosophy.md#1-deterministic-runtime) — axiom 1
- [runtime/replay_engine.md](../runtime/replay_engine.md) — replay engine design
- [runtime/strategy_engine.md](../runtime/strategy_engine.md) — strategy hard rules
- [standards/testing_strategy.md](../standards/testing_strategy.md#level-5--replay-tests-phase-1) — replay test discipline
- [agents/reviewer.md](../agents/reviewer.md) — determinism checklist in code review
