# ADR-006: Execution Engine Isolation — Dedicated Task Within the Trading Process

**Status:** Accepted
**Date:** 2026-05-09
**Decision makers:** rosha
**Supersedes:** —

---

## Context

Phase 1 introduces an execution engine: the only component that talks to the broker order API. Strategies, risk, and state interact with it through events.

The question: where does the execution engine run?

Three reasonable options:

1. **In-process Tokio task** within the same binary as ingestion + strategy + risk.
2. **Separate Tokio task** within a new "trading" binary, alongside strategy + risk; ingestion remains separate.
3. **Separate OS process** (like the circuit breaker, see [ADR-003](ADR-003-circuit-breaker-isolation.md)).

This ADR chooses option 2, with a specific rationale for why option 3 is reserved for the circuit breaker only.

## Decision

The execution engine runs as a **dedicated Tokio task within a new `trading` binary**, separate from ingestion. Strategy, risk, and execution all live in this binary; they communicate via channels and the event bus.

**Topology:**

```
┌──────────────┐       mpsc/IPC      ┌──────────────────────────────────────────┐
│  ingestion   │  ─────tick stream──▶│  trading (binary)                         │
│  (binary)    │                      │  ┌──────────┐ ┌──────┐ ┌──────────┐    │
│              │                      │  │ strategy │▶│ risk │▶│execution │───▶ Angel One
└──────┬───────┘                      │  └──────────┘ └──────┘ └──────────┘    │
       │                              │      ▲          │           │           │
       │                              │      │          ▼           ▼           │
       │   heartbeat (ZMQ)            │      └────[event bus / state engine]──   │
       │                              └──────────────────────────────────────────┘
       ▼
┌──────────────────┐
│ circuit_breaker  │  ◀─── separate OS process (ADR-003) ───
│  (binary)        │
└──────────────────┘
```

The execution engine is **not** in a separate OS process. It is a Tokio task within the trading binary, owning its own inbound queue and HTTP client.

## Alternatives Considered

| Alternative | Why Rejected |
|---|---|
| **All in one binary (ingestion + strategy + risk + execution)** | Combines I/O-bound ingestion with CPU-bound strategy in one binary. Hard to scale horizontally per concern; ingestion crashes take strategy with them. |
| **Execution as a separate OS process** | Adds IPC serialization (probably JSON or FlatBuffers over ZMQ) on the hot path of every order. Latency cost (~10–100µs per hop) is meaningful when budgets are sub-millisecond. The circuit breaker accepts this cost because **isolation is the point** for a kill-switch. The execution engine has no equivalent reason — co-locating with strategy/risk simplifies state sharing without compromising safety. |
| **Execution as a Python service** | Python adds GC pauses, GIL, slower HTTP, and breaks the language-uniformity principle. Rust HTTP via `reqwest` is fine. |
| **Microservices per concern** (separate strategy, risk, execution processes) | Premature distributed architecture. We have one developer, one strategy, one machine. Distribute when there's a real reason to. |
| **Same binary as ingestion** | Mixes timing-sensitive ingestion (no GC, no allocation in hot path) with strategy compute that may allocate freely. Better to keep the ingestion binary lean. |

## Why The Execution Engine Doesn't Need Process Isolation

Compare to the circuit breaker, which **does** need process isolation (per [ADR-003](ADR-003-circuit-breaker-isolation.md)):

| Property | Circuit Breaker | Execution Engine |
|---|---|---|
| Must survive ingestion process crash? | **Yes** (kill switch must work even if everything else is dead) | No (if ingestion is dead, strategies have no input — there are no orders to place) |
| Must function with a hung trading process? | **Yes** (last line of defense) | No (if the trading process hangs, no new orders are generated; the circuit breaker notices and kills) |
| Owns the right to override risk decisions? | **Yes** (it cancels positions unconditionally) | No (it asks risk for permission first) |
| Is its API a single, simple "panic" call? | **Yes** | No — full order lifecycle FSM with many transitions |

The execution engine is "load-bearing for normal operation." The circuit breaker is "load-bearing for failure modes." They have different isolation requirements.

## Tradeoffs

**Advantages:**
- Strategy → risk → execution is a single in-process pipeline. No serialization overhead per order.
- Shared `StateHandle` across strategy, risk, and execution avoids duplicated state across processes.
- One binary, one log, one process to attach a debugger to during development.
- Restart story is simple: kill the trading binary, restart it; on startup it reconciles in-flight orders with the broker (per [runtime/execution_engine.md](../runtime/execution_engine.md#restart-recovery)).

**Disadvantages:**
- A panic in strategy code can take down execution. Mitigation: strategies run in a `tokio::spawn` with `catch_unwind`-style panic catching at the task boundary; their panics emit `StrategyHalt` events but don't kill the binary. This is **not** a substitute for the circuit breaker; the circuit breaker still kills everything if the binary becomes unresponsive.
- Memory pressure from one strategy can affect execution. Mitigation: strategies have a memory budget enforced by allocation tracking (Phase 2; not Phase 1).
- Cannot independently version-upgrade execution without restarting the trading binary. Acceptable.

## Consequences

- **One new binary**: `trading` crate added in Phase 1. Contains strategy + risk + execution as Tokio tasks.
- **Channel topology** (in-process within `trading`):
  - `mpsc<MarketEvent>`: ingestion-relay → strategy
  - `mpsc<SignalEvent>`: strategy → risk
  - `mpsc<RiskApprovedOrder>`: risk → execution
  - `broadcast<SystemEvent>`: execution → state, audit, observability
- **Cross-binary** (between `ingestion` and `trading`):
  - Phase 1 starts with `mpsc` if both run in one process; if separated, ZMQ PUB/SUB on a market-data topic. The decision is deferred to a Phase 1 ADR pending profiling.
- **Each task has its own loop** with `tokio::select!` over its inbound channel + a shutdown signal. Graceful shutdown propagates: ingestion EOF → strategy drains → risk drains → execution finishes in-flight orders → state writes final snapshot.
- **Reconciliation on startup**: the execution engine queries the broker for in-flight `client_order_id`s before accepting new signals (see [execution_engine.md](../runtime/execution_engine.md#restart-recovery)). This is what makes the in-process design safe across restarts.
- **Crash-recovery boundary**: if the trading binary crashes, the circuit breaker's heartbeat-loss watchdog triggers within 50ms and cancels all open orders. The crashed binary is restarted by the operator (or a process supervisor); on restart, reconciliation sees no in-flight orders.

---

## See Also

- [ADR-001](ADR-001-event-bus-design.md) — channel topology and ZMQ choices
- [ADR-003](ADR-003-circuit-breaker-isolation.md) — why **the circuit breaker** is process-isolated
- [runtime/execution_engine.md](../runtime/execution_engine.md) — execution FSM and reconciliation
- [runtime/risk_engine.md](../runtime/risk_engine.md) — pre-trade gate
- [vision/system_philosophy.md](../vision/system_philosophy.md#7-risk-is-infrastructure-not-application-code) — risk-as-infrastructure axiom
- [vision/design_principles.md](../vision/design_principles.md#architecture-principles) — principle 7 (risk before execution), 8 (strategies don't touch broker)
