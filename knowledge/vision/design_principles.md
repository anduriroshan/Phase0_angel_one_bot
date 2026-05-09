# Design Principles

> Operational rules derived from the [System Philosophy](../vision/system_philosophy.md).
> These are concrete, actionable constraints that govern every implementation decision.

---

## Data Flow Principles

1. **Everything is an event.** Market data, signals, orders, fills, risk alerts, system health — all are events with timestamps and causal ordering.

2. **State changes are projections.** No mutable state is "the truth." The event log is truth. State is derived by folding events.

3. **Events are immutable.** Once an event is emitted and committed, it is never modified or deleted. Corrections are new events that reference the original.

4. **No hidden mutable state.** If a component's behavior depends on some internal state, that state must be reconstructable from events. No secret counters, no invisible caches that affect decisions.

5. **Data flows in one direction.** The pipeline is a directed acyclic graph: `Ingestion → Normalization → Storage/Strategy → Risk → Execution`. No cycles. No callbacks that reverse the flow.

---

## Architecture Principles

6. **Exchange adapters are isolated.** Broker-specific wire formats, auth flows, and API quirks live in adapter modules. The core system never sees raw broker data.

7. **Risk sits before execution.** Every order passes through the risk engine before it reaches the exchange adapter. This is structural, not optional.

8. **Strategies never directly touch exchange APIs.** A strategy emits `SignalEvent`s. It does not know how to place an order. It does not know which broker is connected.

9. **Components communicate through typed channels.** No shared memory. No global singletons. Components exchange messages through `mpsc`, `watch`, `broadcast`, or ZMQ channels.

10. **Fail loudly, degrade gracefully.** Errors are logged at `ERROR` level and propagated. A failing storage sink does not crash ingestion. A failing strategy does not disable risk.

---

## Data Integrity Principles

11. **Sequence numbers are sacred.** Every tick carries a `seq_no`. Gaps must be detected and logged. Missing sequences invalidate backtest results.

12. **Timestamps come from the exchange.** System clock timestamps are for logging. Exchange timestamps are for ordering. Never mix them.

13. **Price precision is explicit.** Angel One transmits prices as integers in paise (₹245.50 → 24550). The division by 100 happens exactly once, at the normalization boundary. Downstream code works with `f64` rupees.

14. **Schema is the contract.** The `Tick` struct in `common::schema` is the single source of truth for the data model. All storage sinks, strategies, and analytics must consume this schema — not raw broker payloads.

---

## Development Principles

15. **Profile before optimizing.** No `unsafe`, no `Box::leak`, no hand-rolled allocators unless a profiler shows the bottleneck. Measure → Identify → Fix.

16. **Tests replay real data.** Unit tests use synthetic ticks. Integration tests replay recorded market sessions. "It compiles" is not a test.

17. **Configuration is environment, not code.** Instrument tokens, exchange types, thresholds, and endpoints live in `.env` files or environment variables. Changing a subscription does not require recompilation.

18. **Observability is built in.** Every component emits structured logs via `tracing`. Every decision point is traceable. If you can't explain why the system did something by reading logs, the logging is insufficient.

---

## Phase-Specific Constraints

### Phase 0 — Data Substrate (Current)

- **Read-only.** No orders are placed. No positions are opened.
- The circuit breaker operates in **dry-run mode** by default.
- Focus: data collection quality, gap detection, storage correctness.
- Success metric: Can we replay a full trading day from stored Parquet files and reconstruct every tick?

### Phase 1 — Strategy & Execution (Future)

- Strategies emit signals. Execution engine places orders.
- Circuit breaker switches to **live mode** (`CIRCUIT_BREAKER_DRY_RUN=false`).
- Risk engine enforces pre-trade position limits.
- Success metric: Live execution matches backtest within slippage tolerance.

### Phase 2 — Multi-Strategy (Future)

- Multiple strategies share the same data feed.
- Portfolio-level risk aggregation across strategies.
- Success metric: Adding a strategy does not require modifying the runtime.
