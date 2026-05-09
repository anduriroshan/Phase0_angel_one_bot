# Phase 1 Build Checklist

> **Status:** Phase 1 (active)
>
> What to build, in what order, with what tests. Each step is sized for one
> agent session. Steps are ordered by dependency: do not start step N+1
> before step N's acceptance criteria pass.
>
> **Architecture decision:** we use NautilusTrader's Rust crates as the
> trading engine foundation. We do NOT rebuild order book, execution FSM,
> clock abstraction, strategy trait, replay engine, or portfolio state from
> scratch. See [adr/ADR-007-nautilus-trader-foundation.md](adr/ADR-007-nautilus-trader-foundation.md).
>
> **What we write:** an Angel One adapter (`DataClient` + `ExecutionClient`),
> NSE-specific risk checks, and strategies implementing NautilusTrader's
> `Actor` trait.

---

## How to Read This

Each step has:

- **What:** the deliverable in one sentence.
- **Where:** the crate / file paths.
- **Agent:** which role file to read first.
- **Reading:** mandatory docs before writing code.
- **Done when:** acceptance criteria — all must pass before moving on.
- **Blocks:** which steps cannot start until this is done.

---

## Milestone 0 — Workspace Setup (BLOCKS EVERYTHING)

### Step 1 — Add NautilusTrader crates to the workspace

- [ ] **What:** wire NautilusTrader as a pinned git dependency across the
  workspace; verify it compiles.
- **Where:** root `Cargo.toml` and `Cargo.lock`.
- **Agent:** `rust_engineer`.
- **Reading:** [adr/ADR-007-nautilus-trader-foundation.md](adr/ADR-007-nautilus-trader-foundation.md)
  (dependency pin section), [standards/rust_patterns.md](standards/rust_patterns.md).
- **Done when:**
  - Find the HEAD SHA of the vendored copy:
    `git -C knowledge/references/nautilus_trader log --oneline -1`
  - Add the following crates as git dependencies pinned to that SHA:
    `nautilus-model`, `nautilus-common`, `nautilus-data`, `nautilus-execution`,
    `nautilus-portfolio`, `nautilus-risk`, `nautilus-trading`,
    `nautilus-backtest`, `nautilus-persistence`.
  - `cargo build --workspace` compiles with no errors (warnings acceptable).
  - `cargo test --workspace` passes (existing Phase 0 tests still green).
  - The SHA is recorded as a comment in `Cargo.toml`:
    `# nautilus-trader pinned to: <sha> (<date>)`.
- **Blocks:** everything. Nothing can import NautilusTrader types until this
  is merged.

---

## Milestone 1 — Angel One Adapter (the core custom work)

This is the only component that has no NautilusTrader equivalent. Everything
else in Phase 1 is configuration + thin domain logic on top of NautilusTrader.

### Step 2 — `adapter_angelone` data client

- [ ] **What:** implement NautilusTrader's `DataClient` trait for Angel One
  SnapQuote WebSocket feed; decode binary frames into `QuoteTick` and
  `OrderBookDeltas`.
- **Where:** new crate `adapter_angelone/` with `src/lib.rs`,
  `src/data.rs` (`AngelOneDataClient`), `src/decode.rs` (binary frame parser),
  `src/auth.rs` (reuse `ingestion/src/auth.rs` — do not duplicate).
- **Agent:** `rust_engineer`.
- **Reading:** [domain/exchange_protocols.md](domain/exchange_protocols.md)
  (SnapQuote binary layout — this is the wire format we decode),
  [runtime/order_book.md](runtime/order_book.md) (what the book update
  should look like after decode; NautilusTrader will maintain the book from
  our deltas), `knowledge/references/nautilus_trader/crates/adapters/` (read
  an existing adapter as a pattern — e.g., `architect_ax` — before writing).
- **Done when:**
  - `AngelOneDataClient` implements `nautilus_execution::DataClient`
    (or the equivalent live client trait).
  - `subscribe_quote_ticks(instrument_id)` opens the SnapQuote WS stream for
    that instrument; each frame produces a `QuoteTick` published to the
    `DataEngine`.
  - `subscribe_order_book_deltas(instrument_id, depth=5)` produces
    `OrderBookDeltas` from SnapQuote mode-3 full snapshots. Since SnapQuote
    sends full snapshots (not deltas), emit a clear + replace sequence on
    every packet.
  - `InstrumentId` mapping: maintain a `HashMap<u32, InstrumentId>` from
    Angel One token to NautilusTrader instrument ID. Loaded from a config
    file (`config/instruments.toml`).
  - Gap detection: if `seq_no` is non-monotonic, log a warning and continue
    (SnapQuote full-snapshot nature means gaps don't corrupt the book, but
    we still want the warning).
  - Unit tests with fixture binary frames (record a few real frames; commit
    them as `adapter_angelone/tests/fixtures/`): decode produces correct
    `QuoteTick` fields, prices in integer paise, qty in lots.
  - Integration test: spin up a mock WS server sending fixture frames;
    assert `DataEngine` book cache reflects expected bid/ask after N frames.
- **Blocks:** step 3 (execution client in same crate), step 4 (live node).

### Step 3 — `adapter_angelone` execution client

- [ ] **What:** implement NautilusTrader's `ExecutionClient` trait — translate
  `SubmitOrder` / `CancelOrder` commands into Angel One REST calls; feed
  `OrderStatusReport` back into the `ExecutionEngine`.
- **Where:** `adapter_angelone/src/execution.rs` (`AngelOneExecutionClient`).
- **Agent:** `rust_engineer` (cross-check with `risk_engineer` for the dry-run
  gate).
- **Reading:** [runtime/execution_engine.md](runtime/execution_engine.md)
  (order lifecycle — NautilusTrader enforces this FSM; we feed it reports),
  [domain/exchange_protocols.md](domain/exchange_protocols.md) (REST order
  endpoints, response schema), [adr/ADR-006-execution-engine-isolation.md](adr/ADR-006-execution-engine-isolation.md).
- **Done when:**
  - `AngelOneExecutionClient` implements the NautilusTrader `ExecutionClient`
    (or `LiveExecutionClient`) trait.
  - `submit_order(cmd: SubmitOrder)` builds the Angel One REST payload from
    NautilusTrader's `Order` type; sends via `reqwest`; parses the response
    into `OrderAccepted` / `OrderRejected` reports.
  - `client_order_id` strategy: use NautilusTrader's `ClientOrderId` which
    is already deterministic from the strategy's order factory.
  - Ack timeout: if no `OrderAccepted` / `OrderRejected` within
    `ACK_TIMEOUT_MS` (default 5000), emit `OrderExpired`.
  - **Dry-run gate:** when `ANGEL_ONE_DRY_RUN=true` (default), log the REST
    payload but do not send. The `DataClient` still connects to the live feed
    (read-only is safe).
  - Reconciliation on startup: call Angel One `GET /orders`, emit
    `OrderStatusReport` for each in-flight order so NautilusTrader's
    `ExecutionEngine` catches up.
  - Mock broker for tests: a `MockAngelOneServer` (axum or warp) that returns
    controllable ack/fill/reject responses with configurable latency.
  - Integration test (mock broker): place → ack → partial fill → full fill
    happy path; rejection path; ack timeout path.
- **Blocks:** step 4 (live node needs both clients).

### Step 4 — End-to-end adapter integration test

- [ ] **What:** prove the full data → book → strategy → order → fill pipeline
  works against mock infrastructure before wiring a live node.
- **Where:** `adapter_angelone/tests/e2e.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [examples/signal_to_fill_flow.md](examples/signal_to_fill_flow.md).
- **Done when:**
  - Test uses mock WS server (fixture frames) + mock broker.
  - `DataEngine` book cache is populated after N ticks.
  - A stub `Actor` that reads the book and immediately emits a buy order
    sees that order transit `Initialized → Submitted → Accepted → Filled`.
  - `Portfolio` reflects the correct position after fill.
  - All events have populated `correlation_id` chaining tick → order → fill.
- **Blocks:** risk step 5, strategy step 6.

---

## Milestone 2 — NSE Risk Extension

NautilusTrader's `RiskEngine` handles generic pre-trade checks (notional
limits, position limits). We extend it with NSE F&O constraints that are
not in any generic framework.

### Step 5 — `risk_nse` crate

- [ ] **What:** NSE-specific pre-trade checks as a layer on top of
  NautilusTrader's `RiskEngine`.
- **Where:** new crate `risk_nse/` with `src/lib.rs`,
  `src/checks/{freeze_qty, lot_size, stt_trap, physical_settlement}.rs`.
- **Agent:** `risk_engineer`.
- **Reading:** [runtime/risk_engine.md](runtime/risk_engine.md),
  [agents/risk_engineer.md](agents/risk_engineer.md) hard rules,
  [domain/nse_fo_specifics.md](domain/nse_fo_specifics.md) (the source of
  truth for every number in this crate — verify against current NSE circulars
  before hardening any value).
- **Done when:**
  - `NseRiskCheck::validate(order: &Order, instrument: &Instrument) -> RiskCheckResult`
    runs before NautilusTrader's own pre-trade gate.
  - Checks: (1) `order.qty` is a whole multiple of `instrument.lot_size`; (2)
    `order.qty <= freeze_qty` for this instrument; (3) if instrument is a stock
    option expiring this week, emit `SttTrapWarning` event (do not block, but
    log loud); (4) if instrument has physical settlement and position would go
    short into expiry week, emit `PhysicalSettlementRisk` and reject.
  - Every threshold is loaded from `config/nse_risk.toml`, not hardcoded.
  - Property test: any order with `qty % lot_size != 0` returns
    `RiskCheckResult::Rejected`.
  - Property test: any order with `qty > freeze_qty` returns `Rejected`.
  - Unit test for each check at boundary: -1 lot, 0, +1 lot relative to limit.
- **Blocks:** trading binary step 7.

---

## Milestone 3 — Strategy

### Step 6 — `strategy_basis_arb` — first runnable strategy

- [ ] **What:** basis-arb strategy implementing NautilusTrader's `Actor` trait.
  Detects NIFTY-futures-vs-spot-index basis dislocations; trades 1 lot when
  z-score exceeds threshold.
- **Where:** new crate `strategy_basis_arb/`.
- **Agent:** `strategy_engineer`.
- **Reading:** [runtime/strategy_engine.md](runtime/strategy_engine.md),
  [agents/strategy_engineer.md](agents/strategy_engineer.md) hard rules,
  [examples/signal_to_fill_flow.md](examples/signal_to_fill_flow.md),
  [domain/nse_fo_specifics.md](domain/nse_fo_specifics.md) (lot sizes).
- **Done when:**
  - Implements `Actor` trait: `on_start`, `on_quote_tick`, `on_order_book_delta`
    (hot path), `on_stop`.
  - State: rolling mean + variance of `(futures_mid - spot_mid)` over a
    configurable window (`BasisArbConfig::window_secs`, default 60).
  - Uses `self.clock().timestamp_ns()` (NautilusTrader's injected clock) — **never**
    `SystemTime::now()`.
  - Submits a `MarketOrder` (buy or sell) via `self.submit_order()` when
    z-score crosses threshold.
  - Every submitted order has a populated `tags` or `notes` field with the
    rationale: feature value, z-score, threshold, window stats. This is the
    signal rationale rule from the strategy agent.
  - All config in `BasisArbConfig` loaded from `config/strategy_basis_arb.toml`.
  - Unit test: synthetic `QuoteTick` stream crossing the threshold produces
    exactly one order.
  - CI grep: `grep -rn 'SystemTime::now\|Instant::now' strategy_basis_arb/` must
    return empty.
- **Blocks:** live node step 7, backtest step 9.

### Step 7 — Replay-determinism test for the strategy

- [ ] **What:** run the strategy through the same `QuoteTick` sequence twice
  using NautilusTrader's `TestClock`; assert bit-identical orders emitted.
- **Where:** `strategy_basis_arb/tests/determinism.rs`.
- **Agent:** `strategy_engineer`.
- **Reading:** [adr/ADR-005-replay-determinism.md](adr/ADR-005-replay-determinism.md).
- **Done when:**
  - Uses NautilusTrader's `BacktestEngine` (or a minimal test harness with
    `TestClock`) to drive the strategy twice with the same synthetic tick stream.
  - Asserts `run1.orders == run2.orders` (same `ClientOrderId`, same qty, same
    side, same timestamp).
  - 100 randomly-generated tick sequences all pass.
- **Blocks:** backtest step 9.

---

## Milestone 4 — Live Trading Node

### Step 8 — `trading` binary with `LiveTradingNode`

- [ ] **What:** production binary that wires `AngelOneDataClient` +
  `AngelOneExecutionClient` + `NseRiskCheck` + `BasisArbStrategy` into a
  NautilusTrader `LiveTradingNode`.
- **Where:** new binary crate `trading/src/main.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [runtime/event_bus.md](runtime/event_bus.md),
  [examples/signal_to_fill_flow.md](examples/signal_to_fill_flow.md),
  `knowledge/references/nautilus_trader/crates/live/` (how `LiveNode`/
  `TradingNode` is configured — read the source before writing).
- **Done when:**
  - Config loaded from `config/trading.toml` (instruments, strategy params,
    risk limits, Angel One credentials via env vars).
  - `LiveTradingNode` configured with:
    - `AngelOneDataClient` for market data
    - `AngelOneExecutionClient` for order routing
    - `NseRiskCheck` wired before NautilusTrader's own `RiskEngine`
    - `BasisArbStrategy` registered as an actor
  - On startup: reconciles in-flight orders before accepting new signals.
  - Graceful shutdown: on `SIGTERM`, calls `node.stop()` which drains
    in-flight orders to terminal state before exit.
  - Heartbeat publisher task: every 100 ms, publishes `Heartbeat` via ZMQ
    to `circuit_breaker`. On `SIGTERM`, publishes a `GracefulShutdown` event
    before stopping.
  - Smoke test (dry-run, mock broker): start node, inject 100 fixture ticks,
    observe at least one `OrderInitialized`, one `OrderSubmitted`, one
    `OrderFilled`, one `PositionOpened` in the NautilusTrader event log.
- **Blocks:** recording a fixture day (step 10), dry-run flip (step 13).

---

## Milestone 5 — Backtest (THE acceptance gate)

### Step 9 — Record a fixture day (prerequisite for backtest)

- [ ] **What:** run Phase 0 ingestion for one full trading session and archive
  the resulting Parquet files as the canonical backtest fixture.
- **Where:** `data/fixtures/YYYY-MM-DD/` (use a real trading day after step 8
  is running in dry-run mode).
- **Agent:** any (operational, not code).
- **Done when:**
  - Files: `quote_ticks_v1.parquet` for each subscribed instrument, plus
    `heartbeats_v1.parquet`.
  - NautilusTrader `ParquetDataCatalog` can read the files without error.
  - SHA-256 of each file recorded in `data/fixtures/YYYY-MM-DD/MANIFEST.txt`.
- **Blocks:** backtest step 10.

### Step 10 — Backtest smoke test

- [ ] **What:** replay the fixture day through NautilusTrader's
  `BacktestEngine`; assert determinism, causal integrity, latency budget.
- **Where:** `backtest/tests/smoke.rs` or a standalone binary
  `backtest/src/main.rs`.
- **Agent:** `rust_engineer` + `performance_auditor`.
- **Reading:** [examples/replay_session.md](examples/replay_session.md),
  [domain/latency_budget.md](domain/latency_budget.md).
- **Done when:**
  - `BacktestEngine` configured with the fixture catalog + `TestClock` + same
    `BasisArbStrategy` + `NseRiskCheck` + fill model B (crossed-spread, default
    in NautilusTrader's backtest engine).
  - Run the backtest twice with the same seed. Assert:
    - `run1.result_hash == run2.result_hash` (bit-identical orders + fills).
    - Every `OrderFilled` has a `ClientOrderId` that traces back to a
      strategy decision (causal chain intact).
    - `portfolio.positions.total_qty == sum_of_signed_fill_qtys` (state
      reconstruction invariant).
  - Latency assertions on strategy `on_quote_tick` P95 (use NautilusTrader's
    built-in stats or `tracing` histograms): must be < 50 µs.
  - Test fails loudly (dumps first-diverging event) if determinism breaks.
- **Blocks:** dry-run flip (step 13).

---

## Milestone 6 — Hardening (before going live)

### Step 11 — Observability wiring

- [ ] **What:** every metric named in
  [standards/observability.md](standards/observability.md) emitted and
  exported.
- **Where:** `trading/src/telemetry.rs`.
- **Agent:** `performance_auditor`.
- **Reading:** [standards/observability.md](standards/observability.md).
- **Done when:**
  - `tracing-subscriber` configured with JSON output in production, pretty
    in dev.
  - Per-stage latency histograms (decode, book update, strategy tick,
    risk check, order submit) populated in a dry-run session.
  - `OTEL_EXPORT_ENABLED=false` by default; env-var to enable (backend not
    yet deployed).
- **Blocks:** latency budget CI (step 12).

### Step 12 — Latency budget CI assertion

- [ ] **What:** backtest smoke test runs in CI; P95 latency must stay within
  budget or the PR fails.
- **Where:** `.github/workflows/backtest.yml` or `scripts/ci-backtest.sh`.
- **Agent:** `performance_auditor`.
- **Reading:** [domain/latency_budget.md](domain/latency_budget.md).
- **Done when:**
  - CI runs `cargo test --release -p backtest smoke`.
  - On failure, prints per-stage P95 diff vs. last-known-good baseline.
  - Baseline committed to `data/baselines/backtest_p95.json`.
- **Blocks:** dry-run flip.

### Step 13 — Dry-run-to-live flip (operator gate, human required)

- [ ] **What:** flip `ANGEL_ONE_DRY_RUN=false`; run one paper-money trade
  against the live broker; verify the full event chain.
- **Where:** new `runbooks/go_live.md` documents the procedure.
- **Agent:** `risk_engineer` writes the runbook; human executes it.
- **Reading:** [agents/risk_engineer.md](agents/risk_engineer.md),
  [examples/circuit_breaker_lifecycle.md](examples/circuit_breaker_lifecycle.md).
- **Done when:**
  - Steps 1–12 above are all green.
  - `runbooks/go_live.md` written and reviewed.
  - One live trade executed on a low-risk instrument with a 1-lot position
    limit; manually verified that `OrderFilled` event + `PositionOpened` +
    `PnLUpdated` all appear in the event log.
  - Circuit breaker triggered and recovered correctly in a test drill
    (kill `trading` process manually; verify CB fires within grace period).
  - Operator sets `ANGEL_ONE_DRY_RUN=false` in production config. **Phase 1
    ships.**
- **Blocks:** Phase 2.

---

## What's NOT in This Checklist

- **Multiple strategies.** The framework is proven with `basis_arb`. More
  strategies are post-Phase-1.
- **ML / fill model C / DeepLOB.** Phase 2.
- **Alternative data / agentic graphs.** Phase 3.
- **Kernel bypass / DPDK / NUMA.** Out of scope by ADR (see
  [domain/latency_budget.md](domain/latency_budget.md#what-we-are-not-optimizing-for)).

---

## Dependency Map

```
Step 1 (NautilusTrader deps)
  └─ Step 2 (data client)
       └─ Step 3 (exec client)
            └─ Step 4 (e2e adapter test)
                 ├─ Step 5 (NSE risk)
                 └─ Step 6 (strategy)
                      ├─ Step 7 (determinism test)
                      └─ Step 8 (live node)
                           └─ Step 9 (record fixture)
                                └─ Step 10 (backtest smoke)
                                     ├─ Step 11 (observability)
                                     ├─ Step 12 (latency CI)
                                     └─ Step 13 (go live)
```

---

## Done Definition

Phase 1 is done when:

1. Steps 1–13 all checked.
2. `cargo test --release --workspace` passes.
3. Backtest smoke test (step 10) passes deterministically across 5 consecutive
   runs.
4. `trading` binary has run for one full session in dry-run mode against the
   live Angel One feed with no panics, heartbeat loss, or unexpected state.
5. One real-money trade has executed and reconciled correctly.

Until all five are true, Phase 1 is not done.
