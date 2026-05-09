# Example Flow: Replay Session

> Concrete, step-by-step trace of replaying a single trading day's recorded
> Parquet data through the full pipeline and verifying the result.

**Status:** Phase 1 (planned). Replay infrastructure is the deliverable; this document specifies what running it looks like.

---

## Scenario

You want to replay May 7, 2026 — a recorded day of NIFTY/BANKNIFTY tick data — through the basis-arbitrage strategy with a specific parameter set. You want a tearsheet (PnL, signal counts, latency stats) and confidence that the result is reproducible.

---

## Invocation

```bash
cargo run --release -p replay -- \
  --date 2026-05-07 \
  --strategy basis_arb_v1 \
  --params ./configs/basis_arb_default.toml \
  --fill-model crossed_spread \
  --seed 42 \
  --out ./replay/2026-05-07_basis_arb_default
```

Every flag is required. Defaults are forbidden — a replay run with implicit defaults is reproducible only by accident.

---

## Step-by-Step

### Step 0 — Build Manifest

Before any data is read, the runner writes `./replay/{run_id}/config.json`:

```json
{
  "run_id": "2026-05-07_basis_arb_default",
  "started_at": "2026-05-09T18:30:12Z",
  "git_commit": "d2dd302",
  "binary_sha256": "...",
  "date": "2026-05-07",
  "strategy_id": "basis_arb_v1",
  "strategy_params_version": 7,
  "strategy_params": { "entry_z": 2.5, "exit_z": 0.5, "window": 200 },
  "fill_model": "crossed_spread",
  "seed": 42,
  "data_paths": [
    "./data/raw/2026/05/07/26009.parquet",
    "./data/raw/2026/05/07/26000.parquet",
    "./data/raw/2026/05/07/35001.parquet"
  ]
}
```

This manifest is the audit record. Two replay runs with identical manifests must produce identical results — that's the determinism contract from [adr/ADR-005-replay-determinism.md](../adr/ADR-005-replay-determinism.md).

### Step 1 — Load Event Sources

The replay driver opens the listed Parquet files. Each becomes an `EventSource` iterator:

```rust
let sources: Vec<Box<dyn EventSource>> = data_paths.iter()
    .map(|p| Box::new(ParquetTickSource::open(p).unwrap()))
    .collect();
```

Each iterator can `peek_next_ts()` and `pop()`. The driver merges them by next-timestamp.

### Step 2 — Initialize Pipeline

The pipeline is constructed exactly as it would be in live, with **only the data source and clock differing**:

```rust
let clock = Arc::new(ReplayClock::new(/* set on first event */));
let book_cache = OrderBookCache::new();
let state = StateHandle::new();
let risk = RiskEngine::new(risk_params);
let exec = MockExecutionEngine::new(fill_model, seed);  // simulated fills, real FSM
let mut strategy: Box<dyn Strategy> = BasisArbV1::new(strategy_params);
strategy.on_start(&StrategyContext { clock: &*clock, book: &book_cache, state: &state, params: ... });
```

`MockExecutionEngine` shares the same `Order` FSM as the live engine — only its broker is the `FillModel`, not Angel One. This is the live-replay parity invariant. See [runtime/replay_engine.md](../runtime/replay_engine.md#core-invariant-identical-code-paths).

### Step 3 — Drive the Loop

```rust
loop {
    let next_ts = sources.iter().filter_map(|s| s.peek_next_ts()).min();
    let Some(ts) = next_ts else { break };

    clock.set(ts);                              // simulated clock advances

    let event = sources.iter_mut().find(|s| s.peek_next_ts() == Some(ts))
        .unwrap().pop().unwrap();

    // Same code path as live for the rest:
    book_cache.on_tick(&event);                                       // book update
    let market_event = MarketEvent::BookUpdated { inst_id: ..., ts_ns: ts };
    let signals = strategy.on_event(&ctx, &market_event);              // strategy
    for sig in signals {
        if let Some(approved) = risk.check(&sig, &state) {              // risk
            exec.submit(approved);                                       // submit
        }
    }
    exec.process_pending(ts, &book_cache, &state);                     // simulated fills
    while let Some(evt) = exec.drain_events() {
        state.apply(&evt);                                              // state
    }
}
```

The driver processes ~9.5 million market ticks (a typical NSE day) in seconds-to-minutes, depending on strategy compute.

### Step 4 — Emit Outputs

After the loop terminates:

```
./replay/2026-05-07_basis_arb_default/
├── config.json              ← manifest (written at start)
├── result.json              ← summary stats
├── signals.parquet          ← every SignalEvent
├── orders.parquet           ← every order (state transitions over time)
├── fills.parquet            ← every simulated fill
├── positions.parquet        ← position snapshots over time
├── pnl_curve.parquet        ← cumulative PnL minute-by-minute
├── latency_histograms.json  ← per-stage latency distributions
└── tearsheet.html           ← human-readable summary
```

`result.json` contains the canonical summary:

```json
{
  "signal_count": 47,
  "fills_count": 41,
  "rejects_count": 6,
  "realized_pnl_paise": 12_300,
  "max_dd_paise": -8_400,
  "max_position_qty": 25,
  "p95_signal_latency_ns": 38_500,
  "p95_order_submit_latency_ns": 78_200,
  "result_hash": "sha256:9c1a..."
}
```

`result_hash` is a deterministic hash of the canonical signals + orders + state snapshots. Same inputs → same hash.

---

## Verification

### Determinism Check

Re-run the same command. The new `result.json` must have the same `result_hash`:

```bash
cargo run --release -p replay -- ...   # second time, identical args
diff ./replay/2026-05-07_basis_arb_default/result.json ./replay/2026-05-07_basis_arb_default_run2/result.json
# Expected: only `started_at` differs.
# `result_hash` MUST match. If not — non-determinism leaked in. STOP.
```

This is the smoke test enforced by CI per [standards/testing_strategy.md](../standards/testing_strategy.md#level-5--replay-tests-phase-1).

### Causal-Chain Integrity

Every fill must trace back to a signal:

```python
# Pseudo-Python for analysis (Phase 1 ships a Rust analyzer too)
import polars as pl
fills = pl.read_parquet("fills.parquet")
signals = pl.read_parquet("signals.parquet")
orphans = fills.join(signals, left_on="causal_signal_id", right_on="signal_id", how="anti")
assert orphans.is_empty(), f"orphan fills: {orphans}"
```

If this fails, the event log has a hole. Either the strategy emitted phantom signals, or fills are arriving without provenance.

### State Reconstruction

The position state at end-of-replay must equal the result of folding the fill events from scratch:

```rust
let final_state_from_replay = result.final_position_qty;
let final_state_from_fold: i64 = fills
    .iter()
    .map(|f| match f.side { Side::Buy => f.fill_qty, Side::Sell => -f.fill_qty })
    .sum();
assert_eq!(final_state_from_replay, final_state_from_fold);
```

If this fails, the state engine has a bug — its projection diverges from the event log. Per [vision/system_philosophy.md](../vision/system_philosophy.md#2-event-sourcing), the event log wins; debug the state engine.

### Latency Within Budget

```rust
assert!(result.p95_signal_latency_ns < 50_000);          // 50µs
assert!(result.p95_order_submit_latency_ns < 100_000);   // 100µs
```

Targets per [domain/latency_budget.md](../domain/latency_budget.md). A regression here indicates strategy or risk got slower; investigate via the per-stage histograms.

### No Wall-Clock Leakage

Replay traces are scanned for any `tracing` field named `system_clock`, `wall_clock`, or any helper that calls `SystemTime::now()`. A non-empty result indicates a determinism leak. The convention: any code path that needs real time uses an injected `Clock`. Detection: a CI grep:

```bash
grep -rE "SystemTime::now|Instant::now|Utc::now" crates/strategy crates/risk crates/execution crates/state \
    | grep -v "//.*allow:wall_clock"
# Expected: empty.
```

---

## Comparing Two Runs (Parameter Sweep)

The use case the replay engine actually supports:

```bash
# Run with entry_z = 2.5
cargo run --release -p replay -- ... --params ./configs/basis_arb_z25.toml --out ./replay/z25
# Run with entry_z = 3.0
cargo run --release -p replay -- ... --params ./configs/basis_arb_z30.toml --out ./replay/z30

cargo run --release -p replay-compare ./replay/z25 ./replay/z30
```

Output:

```
Parameter sweep: basis_arb_v1
  entry_z=2.5: signals=47 fills=41 PnL=₹12,300 maxDD=-₹84
  entry_z=3.0: signals=22 fills=20 PnL=₹ 8,150 maxDD=-₹52

Same data. Same code. Different parameters. Result hashes differ
(expected) but each run is individually reproducible.
```

This is the working mode for research: tune one knob, replay, compare. The whole point of replay engine determinism is that the **comparison is meaningful** — any difference in outcome is attributable to the parameter, not to noise.

---

## What Could Go Wrong

| Symptom | Diagnosis | Fix |
|---|---|---|
| Two runs produce different `result_hash` | Determinism leak (wall-clock, unseeded RNG, `HashMap` order) | Find the leaking call site; replace with injected dependency |
| Signal count is 0 | Strategy never triggered. Either thresholds too strict, or upstream wiring broken | Drill into `signals.parquet` (empty?) and per-stage event counts |
| `fills_count > signal_count` | Bug — every fill should trace to a signal | State engine emitted spurious fills; check `causal_signal_id` integrity |
| Replay PnL ≫ live PnL on the same day | Fill model is too optimistic (fills at limit price always) | Switch fill model from `mid_price` to `crossed_spread` or `queue_position`; treat the gap as a feature of the strategy, not a bug |
| Replay PnL ≪ live PnL | Fill model is too pessimistic, OR strategy code has changed since the live run | Check `git log` since the live run; rerun with `git checkout <live-sha>` to baseline |
| Latency P95 in replay is 10× live | Replay binary built in debug; or pipeline has unintended `await_with_pause` | Always use `--release`; check for `tokio::time::sleep` left over from debugging |
| Loop never terminates | A source has events with the same `ts_ns` and the merge isn't progressing | Add tiebreaker (source ID) to merge ordering; this is a known edge case |

---

## See Also

- [runtime/replay_engine.md](../runtime/replay_engine.md) — full design
- [adr/ADR-005-replay-determinism.md](../adr/ADR-005-replay-determinism.md) — guarantees and limits
- [standards/testing_strategy.md](../standards/testing_strategy.md#level-5--replay-tests-phase-1) — replay-test discipline
- [examples/signal_to_fill_flow.md](signal_to_fill_flow.md) — same flow, live
- [domain/latency_budget.md](../domain/latency_budget.md) — latency targets enforced in replay
- [vision/system_philosophy.md](../vision/system_philosophy.md#3-replayability-as-a-first-class-requirement) — axiom

**Last verified against commit:** _pending Phase 1 implementation_
