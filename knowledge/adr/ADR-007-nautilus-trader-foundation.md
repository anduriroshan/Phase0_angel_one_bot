# ADR-007: Use NautilusTrader as the Trading Engine Foundation

**Status:** Accepted
**Phase:** 1
**Supersedes:** The "build from scratch" assumptions in ADR-001 through ADR-006.

---

## Context

The original Phase 1 plan (PHASE_1_CHECKLIST v1) called for building ~8 Rust
crates from scratch: event types, clock abstraction, order book, execution FSM,
state engine, strategy trait, replay engine, and trading binary. This would have
taken months and reproduced problems already solved by production-grade open
source systems.

[NautilusTrader](https://github.com/nautilus-group/nautilus-trader) is a
high-performance trading platform with a pure-Rust core
(`nautilus_core`, `nautilus_model`, `nautilus_common`, `nautilus_execution`,
`nautilus_data`, `nautilus_portfolio`, `nautilus_risk`, `nautilus_trading`,
`nautilus_backtest`). It is battle-tested, actively maintained, and the
vendored source is already present at `knowledge/references/nautilus_trader/`.

The only thing NautilusTrader does not provide is an Angel One adapter. NSE
F&O domain-specific risk rules (freeze qty, STT trap, physical settlement) are
also not in any generic framework.

## Decision

Use NautilusTrader's Rust crates as the trading engine foundation. Write one
`adapter_angelone/` crate that bridges Angel One's protocols into NautilusTrader's
type system. Write NSE-specific risk checks as an extension layer. Write
strategies implementing NautilusTrader's `Actor`/`Strategy` traits.

## What NautilusTrader Provides (we do NOT reimplement)

| Component | NautilusTrader crate | What it gives us |
|---|---|---|
| Event / data types | `nautilus_model` | `OrderBook`, `QuoteTick`, `Order`, `Position`, `Instrument`, `InstrumentId` |
| Clock abstraction | `nautilus_common` | `LiveClock`, `TestClock` (replay-safe, injected) |
| Order book | `nautilus_model::orderbook` | L2 book maintenance, crossed-book detection, derived quantities |
| Execution FSM | `nautilus_execution` | Order lifecycle, `ExecutionClient` trait, idempotency |
| Portfolio / state | `nautilus_portfolio` | Position projection, PnL, exposure tracking |
| Pre-trade risk | `nautilus_risk` | `RiskEngine`, pre-trade gate, limit checks |
| Strategy framework | `nautilus_trading` | `Actor` + `Strategy` traits, registry, hot/cold path |
| Backtest / replay | `nautilus_backtest` | `BacktestEngine`, `BacktestNode`, fill models |
| Persistence | `nautilus_persistence` | Parquet catalog read/write (`ParquetDataCatalog`) |

## What We Build (Angel One + NSE specifics)

| Component | Our crate | What it does |
|---|---|---|
| Market data | `adapter_angelone` | Implements `DataClient` — connects SnapQuote WebSocket, decodes binary frames, feeds `QuoteTick` / `OrderBookDeltas` into NautilusTrader's `DataEngine` |
| Execution | `adapter_angelone` | Implements `ExecutionClient` — translates NautilusTrader `SubmitOrder` / `CancelOrder` into Angel One REST calls; feeds `OrderStatusReport` back |
| NSE risk | `risk_nse` | Extends `nautilus_risk::RiskEngine` with NSE F&O constraints: freeze qty, lot-size validation, STT trap warning, physical-settlement flag |
| Strategies | `strategy_basis_arb` (etc.) | Implements `Actor` trait; reads `OrderBook` via `DataEngine` cache; emits `OrderList` signals via `ExecutionEngine` |
| Kill switch | `circuit_breaker` | Unchanged from Phase 0 — separate OS process, heartbeat-based |
| Live wire-up | `trading/` binary | `LiveTradingNode` configured with our adapter + risk + strategy |

## Consequences

**Positive:**
- Replay determinism, clock injection, order book maintenance, execution FSM —
  already solved. No wheel reinvention.
- Phase 1 checklist shrinks from 22 steps to ~15.
- NautilusTrader's backtest engine gives us fill model A (mid), B
  (crossed-spread), and a framework for model C (queue-position, Phase 2) for free.
- Community maintains the core; we maintain the adapter and strategy layer.

**Negative / risks:**
- We are coupled to NautilusTrader's API. Breaking changes require adapter
  updates. Mitigation: pin to a specific commit SHA in `Cargo.toml`; upgrade
  deliberately.
- NautilusTrader's Rust crates are still maturing — some APIs are not yet
  stable. Mitigation: vendor the crates (already done in `knowledge/references/`);
  add as a path or git dependency.
- NautilusTrader uses a Python layer for configuration and live node setup.
  We use only the Rust crates directly, bypassing Python. Some NautilusTrader
  documentation assumes Python — read the Rust crate source, not the Python docs.

## Dependency Pin

Add to workspace `Cargo.toml`:

```toml
# Pin to a specific commit — do not use a floating branch ref.
# Upgrade requires a new ADR entry or at minimum a `// reason: upgraded to <sha>` comment.
[dependencies]
nautilus-model = { git = "https://github.com/nautilus-group/nautilus-trader", rev = "<sha>" }
nautilus-common = { git = "...", rev = "<sha>" }
nautilus-execution = { git = "...", rev = "<sha>" }
nautilus-data = { git = "...", rev = "<sha>" }
nautilus-portfolio = { git = "...", rev = "<sha>" }
nautilus-risk = { git = "...", rev = "<sha>" }
nautilus-trading = { git = "...", rev = "<sha>" }
nautilus-backtest = { git = "...", rev = "<sha>" }
nautilus-persistence = { git = "...", rev = "<sha>" }
```

Find the latest stable commit SHA from the vendored copy:
`git -C knowledge/references/nautilus_trader log --oneline -1`

## Rejected Alternatives

- **Build from scratch:** rejected — months of work reproducing what NautilusTrader
  already provides correctly. The only justification would be if NautilusTrader's
  abstractions were a poor fit for our data model, which they are not.
- **Use NautilusTrader via Python:** rejected — adds a Python runtime, GIL,
  serialization overhead between Python and Rust. We want a pure-Rust binary.
- **Use Zipline / Backtrader / QuantConnect:** rejected — Python-first, not
  Rust-native, not low-latency.
- **Use a forked NautilusTrader:** rejected — fork maintenance burden. Using
  it as a dependency with a pinned SHA is lower maintenance.

## Related

- [ADR-001](ADR-001-event-bus-design.md) — event bus topology still applies;
  NautilusTrader's `MessageBus` replaces our custom mpsc channels for intra-engine
  messaging. ZMQ to circuit breaker is unchanged.
- [ADR-005](ADR-005-replay-determinism.md) — NautilusTrader's `TestClock` is the
  clock injection mechanism described in that ADR. The determinism guarantee
  holds; the mechanism is now NautilusTrader's.
- [PHASE_1_CHECKLIST.md](../PHASE_1_CHECKLIST.md) — updated to reflect this decision.
