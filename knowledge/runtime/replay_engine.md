# Replay Engine

> Defines how the system replays a recorded trading session from stored
> Parquet files, the determinism guarantees this provides, and the strict
> rule that live and replay code paths must be identical.

**Status:** Phase 1 (planned). Phase 0 records data but does not yet replay it.

---

## Why This Exists

> "It worked in backtest" is the most dangerous sentence in quantitative finance.
> — [vision/system_philosophy.md](../vision/system_philosophy.md#4-research-live-parity)

Replayability is axiom 3 of the system philosophy. Every signal, every order, every PnL calculation must be reproducible from stored events. Without this, backtests are fiction and live debugging is impossible.

---

## Core Invariant: Identical Code Paths

The strategy engine, risk engine, execution engine, and state engine **do not know** whether they are running live or in replay. The only thing that differs is the **data source**:

```
LIVE:                                   REPLAY:
WebSocket ──▶ Tick ──▶ Strategy        Parquet ──▶ Tick ──▶ Strategy
                       (same code)                          (same code)
```

Any code path that branches on `if live { ... } else { ... }` is a bug. The branching point is `main.rs`, which wires either a `LiveDataSource` or a `ReplayDataSource` into the same downstream pipeline.

**Forbidden:** strategies, risk, execution, or state code that calls `SystemTime::now()` or branches on environment. Time and environment are injected. See [vision/design_principles.md](../vision/design_principles.md#data-integrity-principles) principle 12 (timestamps come from the exchange) and the `agents/strategy_engineer.md` hard rules.

---

## Replay Data Sources

The replay engine reads:

| Source | Path | Content |
|---|---|---|
| Market data (cold sink) | `./data/raw/YYYY/MM/DD/{inst_id}.parquet` | Recorded `Tick` events |
| Order events (Phase 1+) | `./data/orders/YYYY/MM/DD/orders.parquet` | `OrderEvent`, `FillEvent`, `RejectionEvent` |
| System events (Phase 1+) | `./data/system/YYYY/MM/DD/events.parquet` | `Heartbeat`, `CircuitBreak`, etc. |

In Phase 1 we replay only market data. The strategy regenerates signals; the execution engine simulates fills via a fill model (below); the state engine replays its projection from scratch.

In Phase 2+, we additionally replay recorded order events to reconstruct **exactly what the live system did**, even if the strategy code has since changed. This is the audit trail.

---

## Simulated Clock

The replay engine drives a **monotonic simulated clock** that advances to the timestamp of the next event:

```text
sim_clock = events[0].ts_ns
for event in events:
    sim_clock = max(sim_clock, event.ts_ns)
    process(event)
```

`sim_clock` is injected into every component that needs "now":

```rust
pub trait Clock: Send + Sync {
    fn now_ns(&self) -> i64;
}

pub struct ReplayClock { /* reads sim_clock */ }
pub struct LiveClock    { /* wraps SystemTime */ }
```

No component calls `SystemTime::now()` directly. They call `clock.now_ns()`. In live mode, `clock` is `LiveClock`. In replay, it's `ReplayClock`. **This is the entire mechanism that makes replay deterministic.**

---

## Fill Model

In live mode, fills come from the broker. In replay mode, fills come from a `FillModel` that simulates how the order would have been filled given the recorded order book state at the time.

Phase 1 fill models, in increasing realism:

### Model A: Mid-Price Fill (UNREALISTIC — for smoke tests only)

Limit orders fill instantly at the mid-price. Market orders fill at mid-price. Used to verify the replay plumbing works; **never** used for actual research.

### Model B: Crossed-Spread Fill (Default for Phase 1)

- **Marketable LIMIT** (buy ≥ best ask, or sell ≤ best bid): fills at the limit price, qty up to the matched side's depth at that price level. Excess unfilled rests as PARTIALLY_FILLED.
- **Resting LIMIT** (buy < best ask, or sell > best bid): does not fill on the same tick. Stays in the simulated book; tries to fill on each subsequent tick when prices cross.
- **MARKET**: walks the book — first fills against the best level, then the next, etc., consuming depth until quantity is satisfied. Slippage is the volume-weighted average minus the pre-fill mid.

### Model C: Queue-Position Fill (Phase 2)

Resting limit orders model their queue position at the time of placement. Fills only occur after the depth ahead of them has been consumed. Requires more research; see arXiv 1312.0563 (queue-reactive models) in `knowledge/`.

The fill model is **configurable**, not hardcoded. Same strategy, different fill models = different simulated PnL — exposes how sensitive a strategy is to execution assumptions.

---

## What Determinism Guarantees (and Doesn't)

| Guarantee | Holds? |
|---|---|
| Same input data + same fill model + same strategy params → bit-identical signals | Yes |
| Same → bit-identical orders | Yes |
| Same → bit-identical simulated PnL | Yes |
| Replay matches live PnL | **No** — fill model approximates broker-side queue/match dynamics; network jitter, partial-fill ordering, and race conditions in live aren't replayed identically. |
| Strategy that consults wall-clock produces same output in replay as live | **No** — and this is a bug. Strategies must use the injected `Clock`. |

The honest framing: **replay is deterministic for our code; it approximates broker behavior**. The gap between replay PnL and live PnL is a measure of how much your strategy depends on broker idiosyncrasies. A small gap is healthy; a large gap means your strategy alpha is fragile.

See [adr/ADR-005-replay-determinism.md](../adr/ADR-005-replay-determinism.md) for the full statement.

---

## Replay Driver

```rust
pub struct ReplayDriver {
    sources: Vec<Box<dyn EventSource>>, // Parquet readers, sorted by next-event ts
    clock: ReplayClock,
    pipeline: Pipeline,                  // strategy + risk + execution + state
}

impl ReplayDriver {
    pub async fn run(&mut self) -> ReplayResult {
        loop {
            let next = self.sources.iter_mut()
                .filter_map(|s| s.peek_next_ts())
                .min(); // earliest pending event across all sources

            let Some(ts) = next else { break; };
            self.clock.set(ts);

            let event = self.next_source(ts).pop().unwrap();
            self.pipeline.handle(event).await?;
        }
        Ok(self.pipeline.finalize())
    }
}
```

Note the **k-way merge** across sources: market ticks, recorded order events, and system events are independent streams that must be processed in timestamp order.

---

## Reference Architecture: NautilusTrader

| Concept | NautilusTrader path |
|---|---|
| Backtest engine driver | `knowledge/nautilus_trader/crates/backtest/src/engine.rs` |
| Simulated exchange / fill engine | `knowledge/nautilus_trader/crates/backtest/src/exchange.rs` |
| Data iterator (k-way merge) | `knowledge/nautilus_trader/crates/backtest/src/data_iterator.rs` |
| Backtest data client | `knowledge/nautilus_trader/crates/backtest/src/data_client.rs` |
| Backtest execution client | `knowledge/nautilus_trader/crates/backtest/src/execution_client.rs` |
| Result accumulation | `knowledge/nautilus_trader/crates/backtest/src/accumulator.rs` |

**What to steal:** the data iterator's k-way merge pattern, the simulated exchange that injects fills into the same execution engine the live system uses (research-live parity by construction).

---

## Replay Output

Each replay run produces:

| Artifact | Path | Purpose |
|---|---|---|
| Signal log | `./replay/{run_id}/signals.parquet` | Every signal generated, with strategy params, book snapshot, decision rationale |
| Order log | `./replay/{run_id}/orders.parquet` | Simulated orders + fills |
| State snapshots | `./replay/{run_id}/positions.parquet` | Position and PnL projection over time |
| Tearsheet | `./replay/{run_id}/tearsheet.html` | Returns, Sharpe, max DD, hit rate, latency stats |
| Run config | `./replay/{run_id}/config.json` | Date range, fill model, strategy params, code commit SHA |

The `code commit SHA` is mandatory. A replay result is meaningless without knowing which version of the strategy produced it.

---

## Smoke-Test Invariants

After every replay run, the following must hold or the run is invalid:

| Invariant | Check |
|---|---|
| Strategy emitted the expected number of signals (within ±10% of historical) | Assert against rolling baseline |
| All orders reached a terminal state (FILLED / CANCELLED / REJECTED) | No orders left in non-terminal states at end of replay |
| Position state at end-of-replay is reachable by folding the order log | Cross-check state engine against fill events |
| `clock.now_ns()` is monotonically non-decreasing throughout the run | Trace assertion |
| No component called `SystemTime::now()` (detected via `tracing` filter or test fixture) | CI assertion |

---

## Implementation Location (planned)

- `replay/src/main.rs` — CLI entry point (`replay --date 2026-05-09 --strategy basis_arb`)
- `replay/src/driver.rs` — `ReplayDriver`
- `replay/src/source.rs` — Parquet event source iterators
- `replay/src/clock.rs` — `ReplayClock` (and the `Clock` trait, shared with live in `common`)
- `replay/src/fills.rs` — fill models A/B/C

The `Clock` trait moves to `common` so both live and replay binaries depend on it.

---

## See Also

- [vision/system_philosophy.md](../vision/system_philosophy.md#3-replayability-as-a-first-class-requirement) — axiom
- [vision/design_principles.md](../vision/design_principles.md) — principles 1-5, 11-12
- [runtime/event_bus.md](event_bus.md#replay-semantics) — replay event ordering
- [runtime/execution_engine.md](execution_engine.md) — same FSM is driven by replay events
- [runtime/state_engine.md](state_engine.md) — state projection from event log
- [examples/replay_session.md](../examples/replay_session.md) — concrete walkthrough of one replay
- [adr/ADR-005-replay-determinism.md](../adr/ADR-005-replay-determinism.md) — what determinism guarantees
- [glossary.md](../glossary.md) — `Event Sourcing`, `Event Log`, `Replay` definitions

**Last verified against commit:** _pending Phase 1 implementation_
