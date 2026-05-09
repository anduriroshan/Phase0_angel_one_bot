# Phase 1 Build Checklist

> **Status:** Phase 1 (active — this is the work to be done)
>
> What to build, in what order, with what tests. Each step is sized for one
> agent session. Steps are ordered by dependency: do not start step N+1
> before step N's acceptance criteria pass.
>
> **Audience:** any code-writing agent (rust_engineer, strategy_engineer,
> risk_engineer, performance_auditor). Match the agent role to the step.
>
> **Source of truth for design:** the runtime/, standards/, adr/, and
> examples/ docs. This checklist points to them; do not duplicate content
> here.

---

## How to Read This

Each step has:

- **What:** the deliverable in one sentence.
- **Where:** the new crate / file paths.
- **Agent:** which role definition the agent must read first.
- **Reading:** mandatory docs before writing code (in addition to the
  agent's own mandatory reading list).
- **Done when:** acceptance criteria. All must pass.
- **Blocks:** which downstream steps cannot start until this is done.

If a step's "Done when" is not bit-precise, the step is not ready — push
back and ask for clarification before coding.

---

## Milestone 0 — Foundations (BLOCKS EVERYTHING)

These three steps are infrastructure every later step depends on. Do not
start any milestone 1+ work until M0 is fully merged.

### Step 1 — Event contracts crate

- [ ] **What:** create the `events/` crate housing every Phase 1 event
  type, version-stamped, serde + Parquet schema-emitting.
- **Where:** new crate `events/` with modules `tick.rs` (re-export from
  `common::TickEvent`), `book.rs` (`BookUpdated`), `signal.rs`
  (`SignalEvent`), `risk.rs` (`RiskApprovedOrder`, `RiskRejectedSignal`),
  `order.rs` (`OrderSubmitted`, `OrderAcked`, `OrderRejected`,
  `OrderCancelled`), `fill.rs` (`PartiallyFilled`, `FullyFilled`),
  `state.rs` (`PositionUpdated`, `PnLUpdated`), `lifecycle.rs`
  (`StrategyParamUpdated`, `Heartbeat`, `SessionStart`, `SessionEnd`).
- **Agent:** `rust_engineer`.
- **Reading:** [standards/event_contracts.md](standards/event_contracts.md),
  [glossary.md](glossary.md) (every event-type entry),
  [examples/signal_to_fill_flow.md](examples/signal_to_fill_flow.md) (so the
  field shapes match the trace).
- **Done when:**
  - Every event has `version: u8`, `correlation_id: Uuid`, `event_id: Uuid`,
    `ts_ns: u64`.
  - Each module has a `#[test]` proving round-trip serde (JSON + Parquet).
  - Each event type has a `topic_name() -> &'static str` returning a stable
    string. (This is the public contract — never rename.)
  - `cargo test -p events` green.
  - `cargo doc -p events` shows every event with its rationale.
- **Blocks:** every other step. Nothing typed against events compiles
  without this.

### Step 2 — Clock abstraction in `common/`

- [ ] **What:** add the `Clock` trait + `LiveClock` + `ReplayClock` to
  `common/`. Every business-logic timestamp goes through this.
- **Where:** `common/src/clock.rs`, re-exported from `common/src/lib.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [adr/ADR-005-replay-determinism.md](adr/ADR-005-replay-determinism.md)
  (the determinism mechanism), [runtime/replay_engine.md](runtime/replay_engine.md)
  (how the clock is driven in replay).
- **Done when:**
  - `trait Clock { fn now_ns(&self) -> u64; }` with `Send + Sync`.
  - `LiveClock` wraps `SystemTime::now()` + monotonic check.
  - `ReplayClock` is `AtomicU64` with `set_ns(ns: u64)` setter callable by
    the replay driver only.
  - Property test: `ReplayClock::set_ns(t)` then `now_ns()` returns `t`,
    monotonically non-decreasing across set calls.
  - CI grep job added: `git grep -n 'SystemTime::now\|Instant::now\|Utc::now\|chrono::Local::now' -- 'crates/**/src/**/*.rs'`
    must return only `common/src/clock.rs` and `circuit_breaker/` (which
    correctly uses real time).
- **Blocks:** strategy, risk pre-trade, execution, state, replay.

### Step 3 — Update CLAUDE.md / repo memory with the build order

- [ ] **What:** add a one-paragraph section to root `CLAUDE.md` (or create
  it) pointing future agents to this checklist as the active task list.
- **Where:** repo root `CLAUDE.md`.
- **Agent:** any.
- **Reading:** none.
- **Done when:** `CLAUDE.md` includes a "What to build next" line linking
  to `knowledge/PHASE_1_CHECKLIST.md`. Committed.
- **Blocks:** none, but completing this prevents future agents from
  re-discovering the work order.

---

## Milestone 1 — Order Book

The first runtime piece. Reads `TickEvent`s (from `ingestion/`) and emits
`BookUpdated`. Self-contained; no broker, no strategy.

### Step 4 — `order_book` crate

- [ ] **What:** L2 order book maintained from Angel One SnapQuote (mode=3)
  packets. Top-5 fixed `[Level; 5]` per side per instrument.
- **Where:** new crate `order_book/` with `src/lib.rs`, `src/book.rs`
  (the `Book` struct), `src/cache.rs` (`BookCache: HashMap<InstId, Book>`),
  `src/derived.rs` (mid, spread, imbalance, microprice).
- **Agent:** `rust_engineer`.
- **Reading:** [runtime/order_book.md](runtime/order_book.md) (entire),
  [adr/ADR-004-order-book-representation.md](adr/ADR-004-order-book-representation.md),
  [domain/exchange_protocols.md](domain/exchange_protocols.md) (SnapQuote
  layout), `knowledge/nautilus_trader/crates/model/src/orderbook/` (read
  before writing — patterns, not copy).
- **Done when:**
  - `Book::apply(tick: &TickEvent) -> BookUpdated` fully replaces both
    sides from the tick (no delta logic — SnapQuote is full snapshot).
  - Gap detection: warn-log when `seq_no` jumps non-monotonically; do not
    panic.
  - Crossed-book check: post-update assert `bid[0].price < ask[0].price`;
    if violated, emit `BookCrossed` event and skip the update.
  - All derived quantities returned in integer paise (no `f64` in hot
    path).
  - Unit tests for: clean update, missing levels (only 3 of 5),
    crossed-book rejection, seq_no gap warning.
  - Property test: book always non-crossed after `apply()` returns Ok.
  - Microbench (`criterion`): `apply()` < 5 µs P95 on representative
    tick.
- **Blocks:** strategy engine (needs book), state engine
  (unrealized PnL needs mid).

### Step 5 — `ingestion → order_book` integration test

- [ ] **What:** prove a recorded `TickEvent` stream produces correct book
  evolution.
- **Where:** `order_book/tests/integration.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [examples/tick_ingestion_flow.md](examples/tick_ingestion_flow.md)
  for the upstream contract.
- **Done when:**
  - Test consumes 1000+ recorded `TickEvent`s for one instrument from a
    Parquet fixture.
  - Final `BookState` matches a hand-computed golden file.
  - `UPDATE_GOLDEN=1 cargo test` regenerates the golden file.
- **Blocks:** strategy step 9.

---

## Milestone 2 — State Engine

Reads events, projects position and PnL. No broker dependency.

### Step 6 — `state` crate

- [ ] **What:** position, portfolio, PnL as deterministic event
  projections.
- **Where:** new crate `state/` with `src/lib.rs`, `src/position.rs`,
  `src/portfolio.rs`, `src/projection.rs` (the `apply()` fold).
- **Agent:** `rust_engineer`.
- **Reading:** [runtime/state_engine.md](runtime/state_engine.md) (entire),
  [glossary.md](glossary.md) entries on `Position`, `PnL`, `Realized PnL`,
  `Unrealized PnL`, `Portfolio`.
- **Done when:**
  - `Position` struct: `qty: i64` (signed lots), `avg_entry_price_paise:
    i64`, `realized_pnl_paise: i64`, `last_update_ns: u64`.
  - `Portfolio::apply(&mut self, event: &Event)` is the only mutation
    path. Pure function: same `(state, event) → state'`.
  - `Portfolio::update_unrealized(&mut self, book: &BookCache)` recomputes
    on-demand (called by strategy/risk consumers); does not run inside
    `apply()`.
  - Unit tests: open long, add to long, partial close, full close, flip to
    short, realized PnL accuracy at each step (use an arithmetic golden
    file).
  - Property test: `apply(events).position.qty == sum_of_signed_fill_qtys`.
  - Property test: `apply(events) == apply(events)` (idempotence /
    determinism).
  - Checkpoint API: `Portfolio::snapshot()` returns serializable state;
    `Portfolio::restore(snap, events_since_snap)` produces identical
    state to full replay.
- **Blocks:** strategy (reads state), risk (reads state), execution
  (publishes fills consumed here).

### Step 7 — Property test: position = Σ fills

- [ ] **What:** the load-bearing invariant for the system. Generate
  random fill sequences; final position must equal the signed sum.
- **Where:** `state/tests/invariants.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [standards/testing_strategy.md](standards/testing_strategy.md)
  (property tests section).
- **Done when:**
  - `proptest` strategy generates valid fill sequences (matched
    instrument, valid qty, signed by side).
  - 10 000 cases × 100 fills each pass.
  - On failure, `proptest` shrinks to a minimal counterexample and dumps
    it to `state/tests/regressions/`.
- **Blocks:** risk re-validation step 12.

---

## Milestone 3 — Strategy Framework

Pure-function signal generation. Reads book + state, emits SignalEvents.

### Step 8 — `strategy` crate (trait + registry + context)

- [ ] **What:** the `Strategy` trait and the registry that owns
  strategy instances inside the trading binary.
- **Where:** new crate `strategy/` with `src/lib.rs`, `src/trait_def.rs`
  (`Strategy` trait), `src/context.rs` (`StratCtx<'a>`: book, state, clock,
  RNG), `src/registry.rs` (`StrategyRegistry`).
- **Agent:** `strategy_engineer`.
- **Reading:** [runtime/strategy_engine.md](runtime/strategy_engine.md)
  (entire), [agents/strategy_engineer.md](agents/strategy_engineer.md)
  hard rules (read these twice — they will catch you).
- **Done when:**
  - Trait surface: `on_start(&mut self, ctx: &mut StartCtx)`,
    `on_event(&mut self, ev: &Event, ctx: &StratCtx) -> SmallVec<[SignalEvent; 4]>`,
    `refresh_params(&mut self, ctx: &mut RefreshCtx)`,
    `on_session_start/end`, `on_stop`.
  - `StratCtx` exposes `&BookCache`, `&Portfolio`, `&dyn Clock`, `&mut
    Rng`. All read-only except RNG.
  - `StrategyRegistry` owns `Box<dyn Strategy + Send>` + each strategy's
    isolated state. Round-robin or deterministic-priority dispatch.
  - Compile-time check: `on_event` may not call `tokio::spawn`,
    `SystemTime::now`, `Instant::now`, network or file syscalls.
    Enforced via `cargo deny` rules + `#![forbid(unsafe_code)]` on the
    crate.
  - `signal_id` generated as `format!("{}-{}", strategy_id,
    monotonic_counter)` where the counter is persisted across restarts in
    the strategy's snapshot.
- **Blocks:** step 9 (reference strategy), step 11 (risk consumes
  signals).

### Step 9 — Reference strategy: `basis_arb`

- [ ] **What:** first runnable strategy. Detects NIFTY-future-vs-spot-index
  basis dislocations; opens 1-lot trades when z-score exceeds threshold.
- **Where:** new crate `strategy_basis_arb/`.
- **Agent:** `strategy_engineer`.
- **Reading:** [examples/signal_to_fill_flow.md](examples/signal_to_fill_flow.md)
  (the example IS this strategy in action),
  [domain/market_microstructure.md](domain/market_microstructure.md),
  [domain/nse_fo_specifics.md](domain/nse_fo_specifics.md) (lot sizes!).
- **Done when:**
  - Implements `Strategy` trait.
  - Tracks rolling mean + variance of (futures_mid - spot_mid) over a
    configurable window (default 60 seconds, exposed as
    `BasisArbParams::window_secs`).
  - Emits `SignalEvent` with `Side::Sell` when `z > +threshold` (futures
    rich), `Side::Buy` when `z < -threshold`.
  - **Rationale field is mandatory** and must include: feature_value,
    threshold, z_score, book snapshot at decision time, window stats.
  - All params in `BasisArbParams { window_secs, threshold, lot_size,
    params_version }` loaded from config.
  - Unit test: synthetic tick stream that crosses threshold produces
    exactly one signal.
  - Replay-determinism property test: `replay(events).signals ==
    replay(events).signals` (bit-identical).
- **Blocks:** replay smoke test (step 21).

### Step 10 — Replay-determinism property test framework

- [ ] **What:** generic harness `replay_strategy_twice(strategy, events)`
  that runs a strategy through the same event sequence twice and asserts
  emitted signals are bit-identical.
- **Where:** `strategy/tests/determinism.rs` (the harness),
  `strategy_basis_arb/tests/determinism.rs` (uses the harness).
- **Agent:** `strategy_engineer`.
- **Reading:** [adr/ADR-005-replay-determinism.md](adr/ADR-005-replay-determinism.md).
- **Done when:**
  - Harness signature: `fn replay_strategy_twice<S: Strategy + Clone>(s:
    S, events: &[Event]) -> Result<(), DeterminismFailure>`.
  - Failure case: dumps the diverging signal index + both signals to
    stderr.
  - 100 random event sequences pass for `BasisArbStrategy`.
- **Blocks:** any future strategy (reused harness).

---

## Milestone 4 — Risk (Pre-Trade Gate)

In-process pre-trade risk. **Distinct** from circuit_breaker, which is the
out-of-process kill switch.

### Step 11 — `risk` crate (pre-trade gate)

- [ ] **What:** in-process module that gates `SignalEvent → RiskApprovedOrder`
  with hard limits.
- **Where:** new crate `risk/` with `src/lib.rs`, `src/limits.rs`,
  `src/checks/{position,margin,duplicate,stale,freeze_qty}.rs`.
- **Agent:** `risk_engineer`.
- **Reading:** [runtime/risk_engine.md](runtime/risk_engine.md) (entire),
  [agents/risk_engineer.md](agents/risk_engineer.md) hard rules,
  [domain/nse_fo_specifics.md](domain/nse_fo_specifics.md) (freeze
  quantities are real broker rejections).
- **Done when:**
  - `PreTradeGate::check(&self, signal: &SignalEvent, portfolio:
    &Portfolio, book: &BookCache, clock: &dyn Clock) -> RiskOutcome`
    where `RiskOutcome = Approved(RiskApprovedOrder) |
    Rejected(RiskRejectedSignal { reason_code, detail })`.
  - Checks implemented: max_position_lots, max_notional_inr,
    max_orders_per_minute, freeze_qty, stale_market (no tick within
    last_tick_age_ns_max), duplicate_order (same correlation_id seen).
  - Every threshold has a `// reason:` comment with the rationale OR a
    glossary/ADR reference.
  - `RiskRejectedSignal` is **emitted as an event**, never just logged.
  - Property test: for every randomly-violated limit, gate returns
    `Rejected` with the matching reason code.
  - Property test: for every randomly-passing signal, gate returns
    `Approved` with same data passed through.
  - Unit test for each check at the threshold boundary: -1, 0, +1.
- **Blocks:** execution (consumes `RiskApprovedOrder`).

### Step 12 — Post-fill re-validation hook

- [ ] **What:** after every fill, run `PreTradeGate` against the **next**
  potential signal as if it were arriving — surface limit breaches the
  fill caused.
- **Where:** `risk/src/post_fill.rs`.
- **Agent:** `risk_engineer`.
- **Reading:** [runtime/risk_engine.md](runtime/risk_engine.md)
  post-fill section.
- **Done when:**
  - `validate_post_fill(portfolio: &Portfolio, limits: &Limits) ->
    Vec<LimitBreach>` returns all current breaches (could be > 1).
  - On any breach: emit `LimitBreach` event, mark strategy as
    "risk-frozen" (no new signals accepted from it until manual unfreeze).
  - Test: simulate a fill that pushes position over `max_position_lots`;
    assert `LimitBreach` event emitted, strategy frozen.
- **Blocks:** none directly, but completing this is the gate for going
  live (DRY_RUN flip).

---

## Milestone 5 — Execution Engine

Sends orders to Angel One. The only component that talks to the broker.

### Step 13 — `execution` crate (FSM + broker client)

- [ ] **What:** order lifecycle FSM + Angel One REST client + ack/timeout
  handling.
- **Where:** new crate `execution/` with `src/lib.rs`, `src/fsm.rs`,
  `src/client.rs` (Angel One REST), `src/idempotency.rs`,
  `src/retry.rs`, `src/timeout.rs`.
- **Agent:** `rust_engineer` (cross-check with `risk_engineer` if
  modifying ack/reject paths).
- **Reading:** [runtime/execution_engine.md](runtime/execution_engine.md)
  (entire), [adr/ADR-006-execution-engine-isolation.md](adr/ADR-006-execution-engine-isolation.md),
  [domain/exchange_protocols.md](domain/exchange_protocols.md) (REST order
  endpoint), `ingestion/src/auth.rs` (reuse auth, do not duplicate).
- **Done when:**
  - States: `New → Submitted → Acked → PartiallyFilled → Filled` (happy
    path); `Rejected`, `Cancelled`, `Stale` as terminal alternatives.
  - `client_order_id = format!("{}-{}-{}", strategy_id, signal_id,
    attempt)` — fully deterministic from inputs.
  - Retry policy: only on transport failures (timeout, connection
    reset). **Never** retry on application rejection (broker says no →
    surfaces as `OrderRejected` event).
  - Ack timeout: `ACK_TIMEOUT_MS` (default 5000 ms). On timeout: emit
    `OrderStale`, do not retry, escalate to operator.
  - Reconciliation on startup: query broker for in-flight orders before
    accepting any new signal.
  - Mock broker (`tests/mock_broker.rs`): controllable ack delay, fill
    schedule, rejection reason.
  - Integration test: full FSM walk via mock broker — happy path, partial
    fills, rejection, timeout-then-stale, reconciliation-on-restart.
  - **`CIRCUIT_BREAKER_DRY_RUN=true` blocks live HTTP send.** A debug
    assert verifies this in release builds too.
- **Blocks:** trading binary, replay smoke test (uses mock broker
  variant).

### Step 14 — Reconciliation on startup

- [ ] **What:** before the trading binary accepts the first signal of a
  session, query Angel One for any in-flight orders from a prior crash;
  reconcile state.
- **Where:** `execution/src/reconcile.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [runtime/execution_engine.md](runtime/execution_engine.md)
  reconciliation section.
- **Done when:**
  - On startup, fetches `GET /orders` from broker, filters to
    `client_order_id` prefix matching this `strategy_id`.
  - Replays terminal events for each (`OrderFilled`, `OrderCancelled`,
    `OrderRejected`) so the state engine catches up.
  - For non-terminal orders (still acked, no fill): mark them as
    `RecoveredInFlight` and let the FSM track to terminal state.
  - Test: kill the trading binary mid-fill via mock broker; restart;
    assert state matches what would have happened with no crash.
- **Blocks:** trading binary going live.

---

## Milestone 6 — Trading Binary

Wires book → strategy → risk → execution → state inside one binary.

### Step 15 — `trading` binary

- [ ] **What:** the live binary. One Tokio runtime, one in-process pipeline,
  publishes heartbeats to the circuit breaker.
- **Where:** new binary crate `trading/` with `src/main.rs`,
  `src/pipeline.rs`, `src/heartbeat.rs`, `src/config.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [runtime/event_bus.md](runtime/event_bus.md),
  [adr/ADR-001-event-bus-design.md](adr/ADR-001-event-bus-design.md),
  [examples/signal_to_fill_flow.md](examples/signal_to_fill_flow.md).
- **Done when:**
  - Wires: `ingestion::Stream → order_book → strategy::Registry → risk::Gate
    → execution → state` via `tokio::sync::mpsc` channels.
  - Heartbeat task: every `HEARTBEAT_INTERVAL_MS` (default 100 ms),
    publish `Heartbeat` event over ZMQ to circuit breaker.
  - Config loaded from `config/trading.toml` at startup; logged.
  - Graceful shutdown on SIGTERM: drain in-flight orders to terminal
    state before exit.
  - Hard kill on SIGKILL or panic: circuit breaker detects via
    heartbeat-loss within `KILL_GRACE_MS` (default 500 ms).
  - End-to-end smoke test: feed ingestion fixture; observe at least one
    `SignalEvent`, one `RiskApprovedOrder` (or `RiskRejectedSignal`),
    one `OrderSubmitted`, one `FullyFilled` (via mock broker), one
    `PositionUpdated`.
- **Blocks:** dry-run-to-live flip.

---

## Milestone 7 — Replay Engine (THE acceptance gate)

Validates everything above. If replay determinism doesn't hold, Phase 1
isn't done — go back and fix.

### Step 16 — `replay` crate

- [ ] **What:** Parquet → event-stream driver with simulated clock.
- **Where:** new crate `replay/` with `src/lib.rs`, `src/source.rs`
  (per-event-type Parquet reader), `src/merge.rs` (k-way merge by
  `ts_ns`), `src/driver.rs`, `src/fill_model.rs`.
- **Agent:** `rust_engineer`.
- **Reading:** [runtime/replay_engine.md](runtime/replay_engine.md)
  (entire), [examples/replay_session.md](examples/replay_session.md).
- **Done when:**
  - Reads `./data/events/{YYYY/MM/DD}/{event_type}_v{version}.parquet`.
  - K-way merge by timestamp; ties broken by `event_id` lexicographic
    (deterministic).
  - Drives `ReplayClock::set_ns(event.ts_ns)` before each event apply.
  - Runs the **same** pipeline code as live (no special replay branches
    inside strategy / risk / state).
- **Blocks:** smoke test step 19.

### Step 17 — Fill model B (crossed-spread)

- [ ] **What:** the default Phase 1 fill model. Walks the order book to
  fill marketable orders.
- **Where:** `replay/src/fill_model.rs`.
- **Agent:** `rust_engineer` (consult `strategy_engineer` for realism).
- **Reading:** [runtime/replay_engine.md](runtime/replay_engine.md) fill
  models section.
- **Done when:**
  - Buy market order fills walking the ask side: take ask[0] up to qty,
    spill to ask[1], etc. Reject and emit `OrderRejected` if order
    qty > sum of all 5 ask levels.
  - Symmetric for sell.
  - Per-level fill latency injected from a configurable distribution
    (default: log-normal with mean 14 ms — broker RTT proxy).
  - Test: feed a synthetic book + sell-1-lot signal; assert filled at
    bid[0].price after the configured latency.
- **Blocks:** replay smoke test.

### Step 18 — Replay smoke test

- [ ] **What:** the final acceptance gate. Replays a recorded full day,
  asserts determinism, asserts latency budget, asserts state
  reconstruction.
- **Where:** `replay/tests/smoke.rs`.
- **Agent:** `rust_engineer` + `performance_auditor`.
- **Reading:** [examples/replay_session.md](examples/replay_session.md)
  (entire), [domain/latency_budget.md](domain/latency_budget.md).
- **Done when:**
  - Test fixture: one full Parquet trading day (record one before
    writing this — see step 23 prerequisite).
  - Run replay twice with same seed; `result_hash` matches exactly. If
    not, fail loud and dump first-diverging event.
  - Causal-chain check: every `OrderFilled` traces via `correlation_id`
    back to a `SignalEvent`. Orphan fills fail the test.
  - State-reconstruction check: `Σ signed_fill_qtys == final_position.qty`.
  - Latency assertions: P95 of `book.update.latency_ns < 5000`,
    `strategy.on_event.latency_ns < 50000`, `risk.check.latency_ns <
    50000`. (Use the replay smoke test's own histograms.)
- **Blocks:** dry-run-to-live flip.

---

## Milestone 8 — Hardening (before going live)

### Step 19 — Observability wiring

- [ ] **What:** every span and metric named in
  [standards/observability.md](standards/observability.md) actually
  registered and exported.
- **Where:** every existing crate; central registration in `trading/src/observability.rs`.
- **Agent:** `performance_auditor`.
- **Reading:** [standards/observability.md](standards/observability.md).
- **Done when:**
  - Every metric in `observability.md`'s table is emitted by at least one
    code path.
  - `tracing-subscriber` configured in `trading::main` with JSON output
    in production, pretty in dev.
  - OpenTelemetry export plumbed but disabled (env-var gated:
    `OTEL_EXPORT_ENABLED=false` default).
  - `cargo run -p trading -- --print-metrics` prints a sample of every
    histogram with its buckets — agents use this to verify coverage.
- **Blocks:** latency-budget CI assertion (step 20).

### Step 20 — Latency budget CI assertion

- [ ] **What:** every PR runs the replay smoke test; latency P95 must stay
  within budget. Regressions fail CI.
- **Where:** `.github/workflows/replay-smoke.yml` (or local
  `scripts/ci-replay.sh` if no GH Actions yet).
- **Agent:** `performance_auditor`.
- **Reading:** [domain/latency_budget.md](domain/latency_budget.md),
  [agents/performance_auditor.md](agents/performance_auditor.md) hard
  rules.
- **Done when:**
  - CI job runs `cargo test --release -p replay smoke`.
  - On failure, prints diff between current and last-known-good
    histograms.
  - Last-known-good baseline committed to `data/baselines/replay_p95.json`.
- **Blocks:** dry-run-to-live flip.

### Step 21 — Record one full trading day

- [ ] **What:** run the existing Phase 0 ingestion + storage stack for a
  full trading day, archive the resulting Parquet files as the canonical
  test fixture.
- **Where:** `data/fixtures/2026-XX-XX/` (use a real trading day).
- **Agent:** any.
- **Reading:** none — this is operational, not a code change.
- **Done when:**
  - Files exist: `ticks_v1.parquet`, `book_v1.parquet`,
    `heartbeat_v1.parquet` for one full session 09:15–15:30 IST.
  - SHA-256 of each file recorded in
    `data/fixtures/2026-XX-XX/MANIFEST.txt`.
  - File size sane (rough check: ~100 MB–1 GB depending on instrument
    universe).
- **Blocks:** replay smoke test step 18.

### Step 22 — Dry-run-to-live flip (operator gate)

- [ ] **What:** the only step a human must approve. Flips
  `CIRCUIT_BREAKER_DRY_RUN=false`, runs one paper-money smoke trade, then
  enables real capital.
- **Where:** operational, not code. Document the procedure in
  `runbooks/go_live.md` (new).
- **Agent:** `risk_engineer` writes the runbook; human flips the switch.
- **Reading:** all of `agents/risk_engineer.md`,
  [examples/circuit_breaker_lifecycle.md](examples/circuit_breaker_lifecycle.md).
- **Done when:**
  - Runbook written and reviewed.
  - Steps 1–21 above all green.
  - One paper-money trade through the live broker on a Saturday
    (markets closed); manual verification of every event in the chain.
  - Operator flips `DRY_RUN=false`. Phase 1 ships.
- **Blocks:** Phase 2.

---

## What's Explicitly NOT in This Checklist

- **Multiple strategies.** One reference strategy (`basis_arb`) is enough
  to validate the framework. Adding more is post-Phase-1 work.
- **ML / DeepLOB / queue-reactive models.** Phase 2.
- **Alternative data, agentic graphs.** Phase 3.
- **Kernel bypass / DPDK / NUMA.** Out of scope by ADR (see
  [domain/latency_budget.md](domain/latency_budget.md#what-we-are-not-optimizing-for)).
- **GraphRAG / embeddings over the knowledge base.** Defer until corpus
  >50 docs (see [README.md](README.md#future-graphrag-migration-plan)).

---

## When Things Go Wrong

- **Replay determinism breaks (step 18 fails):** stop. This is the most
  serious bug class in the system. Find the source of nondeterminism
  before merging anything else. Common causes: `HashMap` iteration,
  unseeded RNG, `Instant::now()` snuck in, async-task scheduling order.
- **Latency budget breached (step 20 fails):** read the per-stage
  histograms (the PR template makes you attach them). Localize before
  optimizing. Do not guess.
- **A risk limit gets in the way during testing:** **do not** loosen it.
  Open an ADR explaining why the limit needs to change. If the testing
  scenario is unrealistic, change the test.

---

## Done Definition

Phase 1 is done when:

1. All steps 1–22 are checked.
2. `cargo test --release` passes everywhere.
3. The replay smoke test (step 18) passes deterministically across 5
   consecutive runs.
4. The trading binary has run for one full session in dry-run mode against
   live ingestion without a single panic, heartbeat loss, or unexpected
   state transition.
5. Operator has flipped `DRY_RUN=false` and one real-money trade has
   executed and reconciled correctly.

Until all five are true, Phase 1 is not done. Resist the urge to
declare victory early.
