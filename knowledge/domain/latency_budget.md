# Latency Budget

> Per-stage latency targets for the live trading pipeline. Defines what
> "fast enough" means at each component and how it's measured.

**Status:** Phase 1 reference. Phase 0 has measured ~15µs for ingestion-only (see [examples/tick_ingestion_flow.md](../examples/tick_ingestion_flow.md#timing-budget-typical)).

---

## Honest Framing

> Our target is **sub-millisecond** ingestion-to-signal latency. This is achievable
> with clean async Rust without heroic optimization. We are not a colocation HFT
> shop. We are building a correct, measurable, research-grade system.
> — [vision/system_philosophy.md](../vision/system_philosophy.md#5-correctness-over-speed)

We are **retail latency**. We don't have:

- Colocation at NSE's BSE Bandra-Kurla data center
- Direct exchange membership / FIX protocol
- Kernel bypass NICs (Solarflare, Mellanox)
- FPGA tick-to-trade
- L3 (per-order) tick-by-tick (TBT) data feed

We **do** have:

- Indian internet (~15–30ms RTT to Mumbai NSE servers from typical retail ISPs)
- Angel One WebSocket conflated to ~4 ticks/sec for indices, faster for liquid F&O
- Standard cloud compute (we're not on a dedicated HFT box)

This places us in a **slow microstructure** regime where strategy edge does not come from raw speed; it comes from correct features and disciplined execution. Latency still matters — for not missing fills and not crossing wide spreads — but optimizing past the budgets below is **explicitly out of scope** until measurement proves the bottleneck.

---

## Hop-by-Hop Targets

Measured from "WebSocket frame arrives" to "order HTTP request sent." Each row is the budget for **that stage alone**.

| Stage | Target P95 | Target P99 | Component | Phase 0 measured |
|---|---|---|---|---|
| WebSocket frame deframe + binary parse | < 10 µs | < 50 µs | Ingestion | ~1 µs |
| Tick normalization (paise → rupees, struct build) | < 1 µs | < 5 µs | Ingestion | ~100 ns |
| Channel send + receive | < 5 µs | < 50 µs | Ingestion → Consumer | ~100 ns |
| Order book update | < 5 µs | < 20 µs | OrderBook cache | n/a |
| Strategy `on_event` (basis arb) | < 50 µs | < 200 µs | Strategy | n/a |
| Risk pre-trade check | < 50 µs | < 200 µs | Risk | n/a |
| Order build + serialize | < 20 µs | < 100 µs | Execution | n/a |
| HTTP request setup (reqwest) | < 100 µs | < 500 µs | Execution | n/a |
| **End-to-end tick-to-send (in-process)** | **< 250 µs** | **< 1 ms** | All | n/a |
| Network RTT to broker (Mumbai retail) | 5 – 30 ms | up to 100 ms | OS / Network | ~15 ms |
| **End-to-end tick-to-ack (wall)** | **< 50 ms** | **< 200 ms** | All + network | n/a |

The wall-time target is dominated by **network RTT**, which we do not control. The in-process target is what we own and optimize against.

---

## What We Are Not Optimizing For

| Optimization | Why we skip |
|---|---|
| Sub-microsecond signal generation | Retail data feed is ~250 ms tick spacing; sub-µs gain is irrelevant |
| Zero-copy serialization | JSON for ZMQ heartbeat is fine; Parquet writer already columnar |
| Kernel bypass / DPDK | Adds Linux-only constraint, complex deployment, no benefit at retail RTT |
| FPGA / hardware acceleration | Out of scope for solo project |
| NUMA pinning | Single CPU socket on a laptop; no cross-socket traffic |
| Custom allocators (jemalloc, mimalloc) | System malloc is fine; only investigate if profiling shows allocator contention |

Decision rule: a future doc named `systems/why_we_dont_need_kernel_bypass.md` (or similar) is added if and when an agent persistently suggests these optimizations. Until then, this section is the answer.

---

## How Each Stage Is Measured

Every stage is wrapped in a `tracing` span (see [standards/observability.md](../standards/observability.md#span-conventions)). The span emits a histogram metric named `<stage>.latency.ns`.

```rust
let start = Instant::now();
let span = info_span!("on_event", strategy_id = self.id);
let signals = span.in_scope(|| self.strategy.on_event(ctx, event));
let elapsed_ns = start.elapsed().as_nanos() as u64;
metrics::histogram!("signal.latency.ns").record(elapsed_ns as f64);
```

Histograms use the buckets in [standards/observability.md](../standards/observability.md#histogram-buckets). Aggregation by P50, P95, P99, P99.9.

**Critical rule:** stage timings come from span enter/exit, not from `tracing::info!` log lines. Logs are slow; spans are sampled. Don't confuse "how long the log shows it took" with "actual latency."

---

## Detection: Budget Breach

The performance auditor agent (`agents/performance_auditor.md`) defines:

- **Warning threshold**: P95 > target → log `warn!`, increment counter `latency.budget_breach.total{stage}`.
- **Critical threshold**: P99 > 2× target sustained for 5 minutes → `error!`, dispatch to alerting.

The system continues to trade through a budget breach (it's not a circuit-breaker condition). The breach signals that something has changed (load, regression, broker latency) and warrants investigation.

---

## Common Budget Breaches and What They Indicate

| Breach | Likely cause |
|---|---|
| `parse_packet.latency` rises | Frame size growing (more depth levels), CPU contention, or new packet type unparsed |
| `signal.latency` rises | Strategy doing more work; check if a feature was added without being benchmarked |
| `order.submit.latency` rises | HTTP client / TLS handshake reuse broken; check `reqwest` connection pool |
| `order.ack.latency` rises (network-bound) | Broker-side issue, ISP issue, or daytime congestion. Out of our control. |
| All stages rise | OS scheduling pressure (noisy neighbor, swap, GC) |

The auditor traces hop-by-hop to identify which stage caused an end-to-end regression. See [agents/performance_auditor.md](../agents/performance_auditor.md).

---

## Phase 1 Verification

A replay smoke test asserts:

```rust
#[test]
fn p95_latency_within_budget_on_2026_05_07_replay() {
    let result = replay_session("2026-05-07");
    assert!(result.p95_signal_latency_ns < 50_000);
    assert!(result.p95_order_submit_latency_ns < 100_000);
    // ... per-stage assertions
}
```

This test is gated behind `cargo test --release` because debug builds are 5–10× slower and would always fail. Optimization of the test itself is forbidden — if the test is slow it means the system is slow, which is the data.

---

## When To Optimize

Per [vision/design_principles.md](../vision/design_principles.md#development-principles) principle 15 (profile before optimizing):

1. **Measure**: hop-by-hop histograms show which stage is slow.
2. **Identify**: drill in with finer spans inside the slow stage.
3. **Fix**: with the smallest change that brings the budget into range.
4. **Verify**: the same metric shows the fix; the fix doesn't regress others.

Forbidden: starting with `unsafe`, custom allocators, lock-free data structures, or `Box::leak` to "make it fast." Any of those requires a profiler trace showing the **specific** bottleneck and an ADR justifying the choice.

---

## Reference

| Source | What it teaches |
|---|---|
| `knowledge/disruptor/` | LMAX latency philosophy: predictable, lock-free, cache-friendly |
| `knowledge/nautilus_trader/BENCHMARKING.md` | NautilusTrader's latency methodology |
| Jane Street tech talks (esp. "Safe at Any Speed") | Determinism > raw speed |

---

## See Also

- [vision/system_philosophy.md](../vision/system_philosophy.md#5-correctness-over-speed) — axiom: correctness over speed
- [vision/design_principles.md](../vision/design_principles.md#development-principles) — profile before optimizing
- [standards/observability.md](../standards/observability.md) — how latency is captured
- [examples/tick_ingestion_flow.md](../examples/tick_ingestion_flow.md#timing-budget-typical) — Phase 0 measured numbers
- [agents/performance_auditor.md](../agents/performance_auditor.md) — agent that owns this

**Last verified against commit:** _NA — reference document; targets revised when measurement justifies_
