# Observability

> Defines tracing span conventions, metric naming, log levels, and how to
> answer "what happened?" from logs and metrics alone — without attaching
> a debugger to a live trading session.

**Status:** Phase 0 uses `tracing` informally. Phase 1 formalizes spans, metrics, and the conventions below.

---

## Why This Matters

> If you can't explain why the system did something by reading logs,
> the logging is insufficient.
> — [vision/design_principles.md](../vision/design_principles.md) principle 18

Observability is not "log a few things." It's a structured, queryable record of every decision the system made. Two people debug a trade six weeks later from logs — not from re-running the system. If that's not possible, the observability is broken.

---

## Three Pillars

| Pillar | Tool | Purpose |
|---|---|---|
| **Logs** | `tracing` (structured) | Sequential narrative of events |
| **Metrics** | `metrics` crate + Prometheus | Aggregates over time (rates, latencies, counts) |
| **Traces** | `tracing` spans + OpenTelemetry export (Phase 2) | Causal chains across components |

Phase 1 ships logs + metrics. OpenTelemetry export is plumbed but not enabled until a Phase 2 backend exists. The convention below is forward-compatible.

---

## Log Levels

Phase 0 already established these in [standards/rust_patterns.md](rust_patterns.md#log-levels). Reproduced for clarity, with Phase 1 additions:

| Level | Use For |
|---|---|
| `error!` | System failures, circuit breaker triggers, data loss, broker rejects of risk-approved orders |
| `warn!` | Degraded operation, parse failures, gap detections, retried errors |
| `info!` | Startup/shutdown, periodic throughput, signal emissions, fills, position changes |
| `debug!` | Per-tick detail, frame-level decisions, intermediate feature values |
| `trace!` | Raw byte dumps, internal state snapshots |

Production runs at `info!`. Backtests can crank to `debug!` since latency doesn't matter. `trace!` is for one-off debugging only.

---

## Structured Fields, Not Format Strings

```rust
// GOOD — queryable, machine-readable
info!(
    inst_id = tick.inst_id,
    price_paise = tick.price_paise,
    seq_no = tick.seq_no,
    "tick received"
);

// BAD — opaque to log queries
info!("tick {}: {} at {}", tick.inst_id, tick.price_paise, tick.seq_no);
```

Fields are first-class. The message is the constant; data is fields. This makes `grep "tick received" | jq 'select(.inst_id==26009)'` trivial and `grep "tick 26009"` a fragile string match.

---

## Span Conventions

A `tracing` span represents a unit of causal work. Spans nest; the parent context is automatically attached to child events.

```rust
let span = info_span!("on_tick", inst_id = tick.inst_id, seq_no = tick.seq_no);
let _enter = span.enter();
// All log events emitted while this guard is alive carry inst_id and seq_no
process_tick(&tick).await?;
```

### Span Names (canonical list)

| Component | Span name | Required fields |
|---|---|---|
| Ingestion | `ws_recv` | `frame_size_bytes` |
| Ingestion | `parse_packet` | `mode`, `frame_size_bytes` |
| Ingestion | `normalize_tick` | `inst_id`, `seq_no` |
| Storage | `parquet_flush` | `inst_id`, `row_count`, `file_size_bytes` |
| Storage | `questdb_send` | `row_count` |
| Strategy | `on_event` | `strategy_id`, `event_type` |
| Strategy | `emit_signal` | `strategy_id`, `signal_id`, `inst_id`, `side` |
| Risk | `pre_trade_check` | `signal_id`, `check_name` |
| Execution | `submit_order` | `client_order_id`, `inst_id`, `attempt` |
| Execution | `await_ack` | `client_order_id` |
| State | `apply_event` | `event_type`, `event_id` |
| Replay | `replay_run` | `run_id`, `date_range`, `strategy_id`, `seed` |

These names are the **public API of observability**. Renaming them breaks dashboards and runbooks. Add new spans freely; renaming requires an ADR.

### Required Span Fields

Every span carries:

- `correlation_id` (when applicable) — chains across components
- `inst_id` (when applicable) — partitions queries by instrument

Optional but encouraged: `signal_id`, `order_id`, `strategy_id`.

---

## Metric Naming

Format: `<component>.<noun>.<unit_or_dimension>`. Lowercase, dot-separated.

| Metric | Type | Description |
|---|---|---|
| `ingestion.ticks.total` | counter | Total ticks ingested since process start |
| `ingestion.parse_errors.total` | counter | Binary parse failures |
| `ingestion.gap_size.distribution` | histogram | Sequence-number gaps when detected |
| `ingestion.ws_reconnects.total` | counter | WebSocket reconnect events |
| `tick.latency.ns` | histogram | WS frame arrival → tick handed to strategy |
| `signal.latency.ns` | histogram | Tick received → signal emitted |
| `order.submit.latency.ns` | histogram | Signal → broker HTTP send |
| `order.ack.latency.ns` | histogram | Submit → broker ack |
| `order.fills.total` | counter (labeled by `inst_id`, `strategy_id`) | Fills observed |
| `order.rejects.total` | counter (labeled by `reason`) | Order rejects |
| `book.crossed.total` | counter (labeled by `inst_id`) | Crossed-book observations |
| `state.position.qty` | gauge (labeled by `inst_id`) | Current open quantity |
| `state.pnl.realized_paise` | gauge | Realized PnL today |
| `state.pnl.unrealized_paise` | gauge | Unrealized PnL on open positions |
| `circuit_breaker.heartbeat.gap_ms` | histogram | Gap between received heartbeats |
| `circuit_breaker.triggered.total` | counter (labeled by `reason`) | Trigger events |

### Histogram Buckets

For latency: `[1µs, 10µs, 50µs, 100µs, 500µs, 1ms, 5ms, 50ms, 500ms]`. Tail is what matters; tight resolution at the microsecond end.

For sizes: `[64, 256, 1KB, 4KB, 16KB, 64KB, 256KB, 1MB]`.

Buckets are part of the metric contract. Changing them invalidates historical comparisons. Document in this file when they change.

---

## Latency Attribution

Each hop in the pipeline records its time-in-stage as a histogram. Together they reconstruct end-to-end latency:

```
tick.latency.ns       = ws_recv → strategy.on_event entered
signal.latency.ns     = strategy.on_event entered → signal emitted
order.submit.latency.ns = signal emitted → broker HTTP request sent
order.ack.latency.ns  = HTTP sent → ack received

end_to_end = sum of the above (within a single strategy decision path)
```

Compare against [domain/latency_budget.md](../domain/latency_budget.md). When a budget is breached, the offending span is in the trace; the question becomes "which sub-step in `submit_order` blew the budget?" — answered by adding finer spans, not by guessing.

---

## What to Log on Every Decision

For every significant decision (signal emit, risk approval/reject, order submit, fill apply), log enough that the decision can be reconstructed:

```rust
info!(
    signal_id = %sig.id,
    inst_id = sig.inst_id,
    side = ?sig.side,
    qty = sig.qty,
    rationale = ?sig.rationale,
    book_bid = best_bid_paise,
    book_ask = best_ask_paise,
    position_qty = current_pos.qty,
    "signal emitted"
);
```

This is verbose. That's the point. At ~100 signals/day, the verbosity cost is trivial; the audit value is high.

---

## What to NOT Log

| Don't log | Why |
|---|---|
| API keys, JWT tokens, secrets | Logs leak. Redact at source. |
| Raw broker responses larger than 1 KB at `info!` | Spam the log. Use `debug!`. |
| Per-tick events at `info!` (4/sec × 6h = 86k lines/day per inst) | Overwhelms log retention. Aggregate via metrics; log every 100th at `info!`. |
| User input PII | None in this system, but be aware. |

---

## Log Targets

| Environment | Sink |
|---|---|
| Local dev | stdout (human-readable formatter) |
| Live trading | stdout + file (JSON formatter) |
| Replay / backtest | file only (JSON formatter, named per `run_id`) |

The JSON formatter is required for any log destined for queries (replay analysis, post-mortem). Human formatter is fine for `cargo run`.

---

## Tracing → OpenTelemetry (Phase 2)

When OpenTelemetry support lands:

1. Add `tracing-opentelemetry` to `Cargo.toml`.
2. Configure an OTLP exporter to a local collector.
3. Spans automatically become OTel spans; metrics become OTLP metrics.
4. No code changes in components — the discipline above is forward-compatible.

Until then, `tracing` events stay local. Don't pre-build the OTel pipeline; defer per [vision/system_philosophy.md](../vision/system_philosophy.md#5-correctness-over-speed) (no premature complexity).

---

## Runbook Hooks

For each known failure mode, the runbook (`OPERATIONS.md`, future) names the log line that signals it. Examples:

| Symptom | Look for |
|---|---|
| Circuit breaker triggered | `error!` with target `circuit_breaker` and message starting `CIRCUIT BREAKER TRIGGERED` |
| Gap in market data | `warn!` with `gap_size` field > 0 |
| Order rejected by broker | `error!` with `order.rejects.total` increment + `reason` field |
| Replay determinism violation | Property test failure: `replay_determinism_property` |

Logs are searched by message + structured fields, not by free text. Every search target above is grep-able from JSON logs.

---

## See Also

- [vision/design_principles.md](../vision/design_principles.md#development-principles) — principle 18: observability is built in
- [standards/rust_patterns.md](rust_patterns.md#logging-standards) — `tracing` baseline (Phase 0)
- [domain/latency_budget.md](../domain/latency_budget.md) — what the latency metrics are measured against
- [agents/performance_auditor.md](../agents/performance_auditor.md) — agent that owns latency observability

**Last verified against commit:** _pending Phase 1 implementation_
