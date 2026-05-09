# Knowledge Index

> Flat alphabetical index of every knowledge document. Agents read this
> first to know what exists. Update on every doc add.
>
> For organized navigation, see [README.md](README.md).
> For term definitions, see [glossary.md](glossary.md).

---

## ADRs (Architecture Decision Records)

- [ADR-001 — Event Bus Design (mpsc + ZMQ, Phase 0)](adr/ADR-001-event-bus-design.md) — channel topology choices
- [ADR-002 — Storage Architecture (dual-sink hot + cold)](adr/ADR-002-storage-architecture.md) — QuestDB + Parquet
- [ADR-003 — Circuit Breaker Process Isolation](adr/ADR-003-circuit-breaker-isolation.md) — why a separate OS process
- [ADR-004 — Order Book Representation (fixed top-5 arrays)](adr/ADR-004-order-book-representation.md) — Phase 1 book layout
- [ADR-005 — Replay Determinism](adr/ADR-005-replay-determinism.md) — what determinism guarantees and what it doesn't
- [ADR-006 — Execution Engine Isolation (dedicated task, same binary)](adr/ADR-006-execution-engine-isolation.md) — why execution is a Tokio task, not a process
- [ADR-007 — NautilusTrader as Foundation](adr/ADR-007-nautilus-trader-foundation.md) — use open-source trading engine; only write the Angel One adapter + NSE risk layer

## Agents (AI Agent Instruction Sets)

- [performance_auditor](agents/performance_auditor.md) — measure, profile, optimize only with evidence
- [reviewer](agents/reviewer.md) — adversarial code review, especially for money/data/determinism
- [risk_engineer](agents/risk_engineer.md) — kill switch, pre-trade checks, paranoid by default
- [rust_engineer](agents/rust_engineer.md) — general Rust systems engineering for the trading runtime
- [strategy_engineer](agents/strategy_engineer.md) — pure-function signal generation; no I/O, no wall-clock

## Domain (Trading Domain Knowledge)

- [exchange_protocols](domain/exchange_protocols.md) — Angel One SmartAPI wire protocol (REST + WebSocket binary)
- [latency_budget](domain/latency_budget.md) — per-stage latency targets and what we are/aren't optimizing
- [market_microstructure](domain/market_microstructure.md) — order book, spread, liquidity, matching, slippage
- [nse_fo_specifics](domain/nse_fo_specifics.md) — NSE F&O lot sizes, expiry, STT, freeze qty, margin, settlement

## Examples (End-to-End Concrete Flows)

- [circuit_breaker_lifecycle](examples/circuit_breaker_lifecycle.md) — startup → trigger → shutdown scenarios
- [replay_session](examples/replay_session.md) — replaying a recorded day with verification
- [signal_to_fill_flow](examples/signal_to_fill_flow.md) — Phase 1 trace: tick → signal → risk → order → fill
- [tick_ingestion_flow](examples/tick_ingestion_flow.md) — Phase 0 trace: WS frame → Tick → storage + heartbeat

## Runtime (How the System Works at Runtime)

- [event_bus](runtime/event_bus.md) — message routing, ordering guarantees, replay semantics
- [execution_engine](runtime/execution_engine.md) — order lifecycle FSM, idempotency, retry semantics
- [order_book](runtime/order_book.md) — L2 book maintenance from SnapQuote; gap recovery
- [replay_engine](runtime/replay_engine.md) — Parquet → event stream, simulated clock, fill model
- [risk_engine](runtime/risk_engine.md) — circuit breaker, panic sequence, pre-trade risk
- [state_engine](runtime/state_engine.md) — position, PnL, exposure as event-log projections
- [strategy_engine](runtime/strategy_engine.md) — strategy contract, hot/cold path split, lifecycle hooks

## Standards (How to Write Code)

- [event_contracts](standards/event_contracts.md) — schema versioning, evolution rules, ID discipline
- [observability](standards/observability.md) — `tracing` spans, metric naming, log levels
- [rust_patterns](standards/rust_patterns.md) — crate org, ownership, async, errors, dependencies
- [testing_strategy](standards/testing_strategy.md) — unit / integration / golden / property / replay levels

## Vision (Why the System Exists)

- [design_principles](vision/design_principles.md) — operational rules derived from philosophy
- [system_philosophy](vision/system_philosophy.md) — constitutional axioms; the source of all rules

## Top-Level

- [glossary](glossary.md) — canonical term definitions; if a term isn't here, it has no agreed-upon meaning
- [PHASE_1_CHECKLIST](PHASE_1_CHECKLIST.md) — **active build order**; the next 22 steps to ship Phase 1 (read this if you are about to write code)
- [README](README.md) — directory tour and reading order

---

## Reference Material (Vendored, Not Documents)

These live in `knowledge/references/`. **Do NOT load wholesale** — use `Grep` with a specific pattern and subpath.

- `knowledge/references/disruptor/` — LMAX Disruptor source. Reference for event-sourcing, lock-free messaging, ring-buffer architecture. Useful for [runtime/state_engine.md](runtime/state_engine.md), [runtime/event_bus.md](runtime/event_bus.md).
- `knowledge/references/nautilus_trader/` — NautilusTrader source (Rust-native production trading engine). Reference for nearly every runtime component. Specific subpaths cited in individual docs.
- `knowledge/references/1312.0563v2.pdf` — Queue-reactive limit-order-book models. Phase 2 reading for fill model C and microstructure features.
- `knowledge/references/1808.03668v6.pdf` — DeepLOB (Zhang, Zohren, Roberts). Phase 2 reading for ML on order book tensors.
- `knowledge/references/1909.12926v1.pdf` — Order-book / microstructure paper. Phase 2 reference.
- `knowledge/references/2102.10925v1.pdf` — Microstructure features paper. Phase 2 reference.

The Phase 2 ML knowledge folder (`knowledge/ml/`) does not yet exist. Distillation of the PDFs into focused md files is deferred until Phase 1 reveals which features are actually computable from our SnapQuote feed (we have L2 top-5, not L3 TBT — many DeepLOB features are not directly applicable).
