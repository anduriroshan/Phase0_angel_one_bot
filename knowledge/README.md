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
├── glossary.md                        ← MOST IMPORTANT: term definitions
│
├── vision/                            ← WHY the system exists
│   ├── system_philosophy.md           ← core axioms (constitution)
│   └── design_principles.md           ← operational rules
│
├── domain/                            ← trading domain knowledge
│   ├── market_microstructure.md       ← bids, asks, spread, matching engines
│   └── exchange_protocols.md          ← Angel One wire protocol reference
│
├── runtime/                           ← how the system works at runtime
│   ├── event_bus.md                   ← messaging topology & semantics
│   └── risk_engine.md                 ← circuit breaker & risk architecture
│
├── standards/                         ← how to write code
│   └── rust_patterns.md               ← crate org, ownership, async, errors
│
├── adr/                               ← architecture decision records
│   ├── ADR-001-event-bus-design.md    ← why mpsc + ZMQ
│   ├── ADR-002-storage-architecture.md ← why dual-sink (QuestDB + Parquet)
│   └── ADR-003-circuit-breaker-isolation.md ← why separate process
│
├── examples/                          ← concrete end-to-end flows
│   ├── tick_ingestion_flow.md         ← single tick through the pipeline
│   └── circuit_breaker_lifecycle.md   ← startup → trigger → shutdown
│
└── agents/                            ← AI agent instruction sets
    ├── rust_engineer.md               ← instructions for coding agents
    └── reviewer.md                    ← instructions for review agents
```

## How to Use This

### For AI Agents

1. **Always start with `glossary.md`** to ensure consistent terminology.
2. Read `vision/system_philosophy.md` to understand the system's axioms.
3. Read `standards/rust_patterns.md` before writing any code.
4. Check `adr/` before making architectural decisions — the decision may already be made.
5. Read `examples/` to understand end-to-end behavior before modifying a component.

### For Humans

1. **New to the project?** Read `vision/system_philosophy.md` → `glossary.md` → `examples/tick_ingestion_flow.md`.
2. **Making an architecture decision?** Write a new ADR in `adr/`.
3. **Adding a new term or concept?** Add it to `glossary.md` first, then reference it elsewhere.

## Maintenance Rules

1. **The glossary is always updated first.** If you introduce a new term, define it in `glossary.md` before using it in code or other docs.
2. **ADRs are append-only.** Don't modify an accepted ADR. Write a new ADR that supersedes it.
3. **Examples must match code.** If the code changes, the example flows must be updated to match.
4. **Agent instructions reference principles.** Don't repeat rules in agent files — reference the source document.
