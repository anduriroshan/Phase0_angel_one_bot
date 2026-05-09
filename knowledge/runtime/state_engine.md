# State Engine

> Defines positions, exposure, and PnL as projections of the event log.
> No mutable state is "the truth." The event log is truth. State is derived
> by folding events.

**Status:** Phase 1 (planned). Phase 0 has no positions or PnL because no orders are placed.

---

## Why a Separate "State Engine"

In a naive trading system, position and PnL are mutated inline as fills arrive — a counter that goes up and down. This is a category error. Positions are a **projection** of the order/fill event stream; mutating them inline conflates the projection with the source.

The system philosophy (axiom 2: event sourcing) requires that:

1. The event log is the single source of truth.
2. Current state is **always** reconstructable by replaying events from genesis.
3. No silent mutations.

The state engine is the component that maintains the projection. Strategies, risk, and observability read from it. Only the state engine writes to it, and only in response to events.

See [vision/system_philosophy.md](../vision/system_philosophy.md#2-event-sourcing) and [vision/design_principles.md](../vision/design_principles.md) principles 1-4.

---

## Projections Maintained

### Position (per instrument)

```rust
pub struct Position {
    inst_id: i32,
    qty: i64,                 // signed: positive long, negative short
    avg_entry_price_paise: i64,
    realized_pnl_paise: i64,  // closed slices' PnL
    last_update_ns: i64,
}
```

`avg_entry_price` is the volume-weighted average over the **open** quantity. When a position partially closes, the average doesn't change (only `qty` and `realized_pnl` change). When a position flips sign (e.g., long 5, sell 7 → short 2), the average resets to the new entry price.

### Portfolio (across instruments)

```rust
pub struct Portfolio {
    positions: HashMap<i32, Position>,
    cash_paise: i64,            // available cash; updated by margin blocks/releases
    margin_used_paise: i64,
    realized_pnl_today_paise: i64,
    unrealized_pnl_paise: i64,  // recomputed on every BookUpdated event
    last_update_ns: i64,
}
```

`unrealized_pnl` is recomputed (not accumulated) on every relevant `BookUpdated` event. This is the only projection that depends on market data, not just order events — without market prices, an open position has no PnL.

### Daily / Session aggregates

Realized PnL today, gross/net exposure, max position size reached, fill count, signal count. All derived; all reset at session start.

---

## State Update Protocol

The state engine subscribes to the event bus and reacts:

| Event | State change |
|---|---|
| `OrderFilled` (full or partial) | Update `Position.qty`, `avg_entry_price`, `realized_pnl`. Update `Portfolio.cash` and `margin_used`. |
| `OrderCancelled` | Release any reserved margin. |
| `OrderRejected` (post-submit) | Release any reserved margin. |
| `BookUpdated` | Recompute `unrealized_pnl` for any open position in the affected instrument. |
| `SessionStart` | Reset daily aggregates. Snapshot positions for next-day continuity. |
| `SessionEnd` | Snapshot positions. Lock state. |

The state engine **never** initiates state changes from internal logic. It is a pure event consumer. This is what makes replay possible — given the same event sequence, the same state is reconstructed.

---

## Folding Function

The core operation is a fold:

```rust
impl State {
    pub fn apply(&mut self, event: &SystemEvent) -> Result<StateDelta, StateError> {
        match event {
            SystemEvent::OrderFilled(fill)    => self.apply_fill(fill),
            SystemEvent::OrderCancelled(c)    => self.apply_cancel(c),
            SystemEvent::BookUpdated(b)       => self.apply_book(b),
            SystemEvent::SessionStart         => self.apply_session_start(),
            SystemEvent::SessionEnd           => self.apply_session_end(),
            // Read-only events: ignored
            _ => Ok(StateDelta::None),
        }
    }
}
```

`apply` is **deterministic and pure modulo `&mut self`**. Given the same `(state_before, event)`, it always produces the same `state_after`. No I/O, no wall-clock, no hidden randomness. This property is what makes replay produce identical state to live.

---

## Checkpointing & Recovery

State is rebuilt from events on every cold start. For very long event logs (multi-month), full replay becomes slow. The mitigation is checkpoints:

1. Periodically (every N events, or every market session boundary), serialize the current `Portfolio` to a snapshot file.
2. On startup: load latest snapshot, then replay events with `ts_ns > snapshot.ts_ns`.

**Snapshots are convenience, not truth.** A snapshot can be deleted at any time; the event log is sufficient to reconstruct everything. If a snapshot disagrees with replay, replay wins.

Snapshot path: `./data/state/checkpoints/{YYYY-MM-DD}.snapshot`

---

## Read API

Strategy and risk components read state through an immutable handle:

```rust
pub struct StateHandle {
    inner: Arc<RwLock<State>>,
}

impl StateHandle {
    pub fn position(&self, inst_id: i32) -> Option<Position>;
    pub fn portfolio_summary(&self) -> PortfolioSummary;
    pub fn realized_pnl(&self) -> i64;
    pub fn unrealized_pnl(&self) -> i64;
    pub fn open_orders(&self) -> Vec<OrderRef>;
}
```

The `RwLock` is fine in Phase 1 because reads vastly outnumber writes (writes only on fills/cancels/book updates, ~10s of events per second; reads happen on every signal evaluation). If contention shows up in profiling, switch to a lock-free snapshot pattern (publish via `Arc::swap`, readers see a consistent snapshot). See [standards/rust_patterns.md](../standards/rust_patterns.md#interior-mutability-watch-over-mutex) for the principle.

---

## Reference Architecture

NautilusTrader's portfolio crate is the closest match:

| Concept | NautilusTrader path |
|---|---|
| Portfolio aggregate | `knowledge/nautilus_trader/crates/portfolio/src/portfolio.rs` |
| Portfolio manager | `knowledge/nautilus_trader/crates/portfolio/src/manager.rs` |

Disruptor (vendored at `knowledge/disruptor/`) is the canonical reference for the broader event-sourcing pattern: a single writer, multiple readers, lock-free for ordered event consumption. We don't need Disruptor's lock-free architecture in Phase 1 (volume is too low to justify it), but the **conceptual model** — events flow through a sequence, projections fold over them — is exactly what the state engine implements.

---

## Invariants

The following must hold at all times. Violation is a system bug:

| Invariant | Check |
|---|---|
| Sum of position quantities does not exceed allowed exposure cap | After every fill |
| `cash + margin_used + unrealized_pnl ≈ initial_capital + realized_pnl` (within rounding) | After every event |
| All position prices are positive integers (paise) | Type-level + assertion |
| `last_update_ns` strictly non-decreasing | Per-instrument |
| No position with `qty == 0` is retained (closed positions are removed from the map) | After every fill |

These invariants are checked in debug builds and as part of replay smoke tests. See [examples/replay_session.md](../examples/replay_session.md) for verification flow.

---

## Failure Modes

| Mode | Cause | Response |
|---|---|---|
| Negative cash | Margin calc bug or out-of-order events | Halt: emit `CircuitBreak`, refuse new signals |
| Position quantity overflow (`i64`) | Bug — should be impossible at NSE retail volumes | Halt |
| Event applied with `ts_ns < last_update_ns` | Out-of-order event delivery | Drop event, log error, continue |
| Snapshot disagrees with replay | Snapshot corruption or code bug | Discard snapshot, full replay from event log |

The first two are halts because they indicate the system has lost track of reality. The circuit breaker takes over.

---

## Implementation Location (planned)

- `state/src/lib.rs` — `State`, `StateHandle`, `apply`
- `state/src/position.rs` — `Position`, fold helpers
- `state/src/portfolio.rs` — `Portfolio`, aggregates
- `state/src/checkpoint.rs` — snapshot serialize / restore

---

## See Also

- [vision/system_philosophy.md](../vision/system_philosophy.md#2-event-sourcing) — axiom
- [runtime/event_bus.md](event_bus.md) — event types the state engine consumes
- [runtime/execution_engine.md](execution_engine.md) — emits the fill events state consumes
- [runtime/replay_engine.md](replay_engine.md) — state is reconstructed via replay
- [runtime/risk_engine.md](risk_engine.md) — pre-trade risk reads from state
- [glossary.md](../glossary.md) — `Event Sourcing`, `Event Log`, `Actor` definitions
- `knowledge/disruptor/` (vendored) — event-sourcing reference

**Last verified against commit:** _pending Phase 1 implementation_
