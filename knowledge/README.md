# Knowledge Base — System Documentation

> Machine-readable architectural cognition for AI agents and human engineers.
>
> This is NOT random documentation. It is a structured specification that
> ensures every component, every agent, and every code change obeys the
> same invariants.

---

## Directory Structure

```
knowledge/
├── README.md                          ← you are here
├── INDEX.md                           ← flat alphabetical doc list (agents read this first)
├── glossary.md                        ← MOST IMPORTANT: term definitions
│
├── vision/                            ← WHY the system exists
│   ├── system_philosophy.md           ← core axioms (constitution)
│   └── design_principles.md           ← operational rules
│
├── domain/                            ← trading domain knowledge
│   ├── market_microstructure.md       ← bids, asks, spread, matching engines
│   ├── exchange_protocols.md          ← Angel One wire protocol reference
│   ├── nse_fo_specifics.md            ← NSE F&O: lot sizes, STT, expiry, margin
│   └── latency_budget.md              ← per-stage latency targets
│
├── runtime/                           ← how the system works at runtime
│   ├── event_bus.md                   ← messaging topology & semantics
│   ├── risk_engine.md                 ← circuit breaker & risk architecture
│   ├── order_book.md                  ← L2 book maintenance from SnapQuote
│   ├── execution_engine.md            ← order lifecycle FSM, idempotency
│   ├── replay_engine.md               ← Parquet → event stream, simulated clock
│   ├── state_engine.md                ← position/PnL as event projections
│   └── strategy_engine.md             ← strategy contract, hot/cold path
│
├── standards/                         ← how to write code
│   ├── rust_patterns.md               ← crate org, ownership, async, errors
│   ├── event_contracts.md             ← schema versioning rules
│   ├── testing_strategy.md            ← unit/property/replay/golden tests
│   └── observability.md               ← tracing spans, metric naming, log levels
│
├── adr/                               ← architecture decision records
│   ├── ADR-001-event-bus-design.md    ← why mpsc + ZMQ
│   ├── ADR-002-storage-architecture.md ← why dual-sink (QuestDB + Parquet)
│   ├── ADR-003-circuit-breaker-isolation.md ← why separate process
│   ├── ADR-004-order-book-representation.md ← why fixed top-5 arrays
│   ├── ADR-005-replay-determinism.md  ← what determinism guarantees
│   └── ADR-006-execution-engine-isolation.md ← why execution is a task, not a process
│
├── examples/                          ← concrete end-to-end flows
│   ├── tick_ingestion_flow.md         ← Phase 0 single tick through the pipeline
│   ├── circuit_breaker_lifecycle.md   ← startup → trigger → shutdown
│   ├── signal_to_fill_flow.md         ← Phase 1 tick → signal → order → fill
│   └── replay_session.md              ← replaying a recorded day with verification
│
├── agents/                            ← AI agent instruction sets
│   ├── rust_engineer.md               ← general Rust systems engineering
│   ├── reviewer.md                    ← adversarial code review
│   ├── strategy_engineer.md           ← pure-function signal generation
│   ├── risk_engineer.md               ← kill switch + pre-trade checks
│   └── performance_auditor.md         ← measurement-driven optimization
│
└── references/                        ← vendored sources (DO NOT load wholesale)
    ├── disruptor/                     ← LMAX Disruptor source
    ├── nautilus_trader/               ← NautilusTrader source (Rust trading engine)
    ├── 1312.0563v2.pdf                ← queue-reactive LOB models
    ├── 1808.03668v6.pdf               ← DeepLOB (Zhang/Zohren/Roberts)
    ├── 1909.12926v1.pdf               ← LOB / microstructure
    └── 2102.10925v1.pdf               ← microstructure features
```

## How to Use This

### For AI Agents

1. **Always start with `INDEX.md`** to know what exists, then `glossary.md` to ensure consistent terminology.
2. **If you are about to write code, read [PHASE_1_CHECKLIST.md](PHASE_1_CHECKLIST.md) first** — it tells you which step is next, in what order, with concrete acceptance criteria.
3. Read `vision/system_philosophy.md` to understand the system's axioms.
4. Read the agent file matching your role (`agents/rust_engineer.md`, `agents/strategy_engineer.md`, etc.).
5. Read `standards/rust_patterns.md` before writing any code.
6. Check `adr/` before making architectural decisions — the decision may already be made.
7. Read `examples/` to understand end-to-end behavior before modifying a component.

### For Humans

1. **New to the project?** Read `vision/system_philosophy.md` → `glossary.md` → `examples/tick_ingestion_flow.md` → `examples/signal_to_fill_flow.md`.
2. **Making an architecture decision?** Write a new ADR in `adr/`.
3. **Adding a new term or concept?** Add it to `glossary.md` first, then reference it elsewhere.
4. **Adding a new doc?** Update `INDEX.md` in the same commit.

## Reference Material

Vendored sources live in `knowledge/references/`. They are **not architectural docs** — they are external sources to consult for specific targeted lookups only.

> **Agents: do NOT glob or read `references/` wholesale.** It contains a full
> Rust trading engine repo and academic PDFs. Loading it will consume your
> entire context window. Use `Grep` with a specific pattern and path instead.

- **`references/nautilus_trader/`** — A production Rust trading engine. Specific subpaths are cited in individual runtime docs (e.g., `runtime/order_book.md` → `references/nautilus_trader/crates/model/src/orderbook/`). Use it as a "stand on shoulders" reference, not a copy-paste source.
- **`references/disruptor/`** — LMAX Disruptor source. Reference for event-sourcing, lock-free messaging, ring-buffer architecture. Useful for `runtime/state_engine.md`, `runtime/event_bus.md`.
- **`references/*.pdf`** — Academic papers, queued for Phase 2 ML work (DeepLOB, queue-reactive models, microstructure features). Distillation into focused md files is deferred until Phase 1 ships.

These files are not committed to git (too large, separate licensing). See [INDEX.md](INDEX.md) for the full reference list with subpath hints.

## Maintenance Rules

1. **The glossary is always updated first.** If you introduce a new term, define it in `glossary.md` before using it in code or other docs.
2. **ADRs are append-only.** Don't modify an accepted ADR. Write a new ADR that supersedes it.
3. **Examples must match code.** If the code changes, the example flows must be updated to match.
4. **Agent instructions reference principles.** Don't repeat rules in agent files — reference the source document.
5. **Phase-tagging:** every doc opens with a `Status:` line indicating which phase it applies to. Update when phase status changes.
6. **Code-mirroring docs include `Last verified against commit: <sha>` at the bottom.** PRs touching the corresponding code path must update the SHA.
7. **No doc may describe code that doesn't exist.** Future-state belongs in ADRs (forward-looking by definition) or in clearly-labeled "Phase 1+ planned" sections.
8. **Keep INDEX.md current.** Adding a doc without indexing it makes it invisible to agents.
