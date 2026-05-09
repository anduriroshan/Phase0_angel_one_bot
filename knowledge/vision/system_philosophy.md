# System Philosophy

> This is the constitutional document of the trading system.
> Every component, every agent, every line of code must obey these axioms.
> If a design decision conflicts with these principles, the principles win.

---

## Core Axioms

### 1. Deterministic Runtime

The system must produce **identical outputs given identical inputs**, regardless of when or where it runs.

- All state transitions are driven by **events with monotonic timestamps**.
- No component may depend on wall-clock time for logic (only for human-facing logs).
- Random number generators, if any, must be seedable and reproducible.
- System clock is injected, never called directly in business logic.

**Why:** A non-deterministic system cannot be debugged, backtested, or trusted. If you cannot replay a trading session and get the same fills, your backtest is fiction.

### 2. Event Sourcing

**State is never mutated directly.** State is a derived projection of an ordered event log.

- The event log is the single source of truth.
- Current state can always be reconstructed by replaying events from genesis.
- Events are immutable once committed. No updates. No deletes.
- Every state change emits an event. No silent mutations.

**Why:** This gives us replayability, auditability, and the ability to answer "why did the system do X at time T?" with certainty.

### 3. Replayability as a First-Class Requirement

Every trading session must be replayable from stored data:

- Market data events (ticks) are stored with exchange timestamps.
- System events (signals, orders, fills) are stored with causal ordering.
- Replay must produce the same decisions as the live session.

**Why:** This is the foundation of all backtesting. If your live system and your backtest system are different codepaths, your research is worthless. Research-live parity is non-negotiable.

### 4. Research-Live Parity

The same strategy code runs in both backtest and live environments. The **only** difference is the data source:

- **Backtest:** events are read from the event log / Parquet files.
- **Live:** events arrive from the WebSocket feed.

The strategy, risk engine, and portfolio state machine are **identical** in both modes.

**Why:** "It worked in backtest" is the most dangerous sentence in quantitative finance. If the backtest runs different code, different assumptions, or different timing semantics, the results are meaningless.

### 5. Correctness Over Speed

We optimize for **correctness first**, then latency.

- No unsafe Rust without a documented safety proof.
- No lock-free data structures unless the lock-contended path is measured and proven to be the bottleneck.
- No premature optimization. Profile first. Measure second. Optimize third.
- Clarity of intent in code is more valuable than saving 50 nanoseconds.

**Latency budget:** Our target is **sub-millisecond** ingestion-to-signal latency. This is achievable with clean async Rust without heroic optimization. We are not a colocation HFT shop. We are building a correct, measurable, research-grade system.

### 6. Modular Actor Architecture

The system is composed of **isolated, message-passing actors** (Rust crates / Tokio tasks):

```
┌──────────┐    ┌──────────┐    ┌──────────┐    ┌──────────┐
│ Ingestion│───▶│ Storage  │    │ Strategy │───▶│Execution │
│  Node    │    │  Node    │    │  Engine  │    │  Engine  │
└────┬─────┘    └──────────┘    └────▲─────┘    └──────────┘
     │                               │
     │    ┌──────────┐              │
     └───▶│ Circuit  │    ┌────────┘
          │ Breaker  │    │ Event Bus
          └──────────┘    └──────────
```

- Actors communicate **only** through typed channels or the event bus.
- No shared mutable state between actors.
- Each actor can be tested, replaced, or scaled independently.
- Failure in one actor does not cascade (graceful degradation).

**Why:** Monolithic trading systems are untestable, unreplayable, and undebugable. Actor isolation is the only sane architecture for a system that handles real money.

### 7. Risk Is Infrastructure, Not Application Code

Risk management is **not a feature** — it is part of the runtime infrastructure:

- The circuit breaker is a separate binary process.
- Pre-trade risk checks sit **between** strategy and execution, not inside either.
- Risk limits are enforced by the system, not by the strategy.
- A strategy cannot bypass risk controls, even if it wants to.

**Why:** Every strategy developer believes their strategy is correct. The system must protect capital regardless of what any individual strategy does.

### 8. Exchange Adapters Are Isolated

All broker-specific logic is encapsulated in adapter modules:

- The core system speaks a **normalized data model** (the Unified Tick Schema).
- Adapters translate between broker-specific formats and the internal model.
- Switching brokers means writing a new adapter, not rewriting the system.
- Currently: Angel One SmartAPI. Future: Zerodha Kite, Interactive Brokers, etc.

**Why:** Broker APIs change, break, and have quirks. The core system must be insulated from these realities.

---

## The Hierarchy of Concerns

When making any design decision, apply this priority order:

1. **Correctness** — Does it produce the right answer?
2. **Replayability** — Can I reproduce this result?
3. **Observability** — Can I understand what happened?
4. **Reliability** — Does it handle failures gracefully?
5. **Performance** — Is it fast enough? (not "is it the fastest possible?")

---

## Anti-Patterns (Explicitly Forbidden)

| Anti-Pattern | Why It's Forbidden |
|---|---|
| Strategy directly calls broker API | Violates actor isolation and risk layering |
| Wall-clock time in business logic | Breaks determinism and replayability |
| Shared mutable global state | Race conditions and untestable code |
| "It works in production" as validation | Not reproducible; not scientific |
| Optimizing before profiling | Wastes time and introduces complexity |
| Combining backtest and live codepaths | Guarantees research-live divergence |
| Swallowing errors silently | Masks bugs that lose money |
