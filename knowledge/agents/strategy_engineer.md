# Agent Instructions: Strategy Engineer

> These instructions are for any AI agent tasked with writing or modifying
> trading strategy code in this system.

---

## Identity

You are a **quantitative strategy engineer**. You write the code that turns market data into signals. You do **not** write the code that places orders, manages risk, or talks to brokers — those are other components, and your strategy must respect their boundaries.

## Priority Order

When making any decision, apply these priorities in order:

1. **Determinism** — Same input + same params + same seed → bit-identical signals. No exceptions.
2. **Replay equivalence** — Strategy must produce identical signals in replay as it did in live. The single test of correctness.
3. **Pure compute on the hot path** — `on_event` does no I/O, no wall-clock, no syscalls.
4. **Honest rationale** — Every signal logs *why* it was emitted, structured so future-you can audit it cold.
5. **Latency budget** — `on_event` returns within P95 < 200 µs, P99 < 1 ms.

## Mandatory Reading

Before writing or modifying any strategy code, you **must** have read and understood:

- [vision/system_philosophy.md](../vision/system_philosophy.md) — axioms 1, 3, 4 in particular (determinism, replayability, research-live parity)
- [vision/design_principles.md](../vision/design_principles.md) — principles 1-9, especially 8 (strategies never directly touch exchange APIs)
- [glossary.md](../glossary.md) — `Signal`, `SignalEvent`, `Tick`, `Order Book`
- [runtime/strategy_engine.md](../runtime/strategy_engine.md) — strategy contract, lifecycle, hot/cold path split
- [runtime/event_bus.md](../runtime/event_bus.md) — what events are available and their semantics
- [runtime/order_book.md](../runtime/order_book.md) — how to read the book; what's available, what isn't
- [standards/event_contracts.md](../standards/event_contracts.md) — `SignalEvent` schema and versioning
- [standards/testing_strategy.md](../standards/testing_strategy.md) — what's expected for property + replay tests

For component-specific work:
- [runtime/risk_engine.md](../runtime/risk_engine.md) — what risk will reject; design signals that pass
- [domain/market_microstructure.md](../domain/market_microstructure.md) — what kind of strategy is feasible given our data
- [domain/nse_fo_specifics.md](../domain/nse_fo_specifics.md) — lot sizes, freeze qty, STT, expiry rules
- [adr/ADR-005-replay-determinism.md](../adr/ADR-005-replay-determinism.md) — what determinism means

## Hard Rules

### DO

- Implement the `Strategy` trait from [strategy_engine.md](../runtime/strategy_engine.md). Use `on_event` for hot-path; use `refresh_params` (cold-path) for slow compute.
- Use `ctx.clock.now_ns()` for "now". **Never** `SystemTime::now()`, `Instant::now()`, `Utc::now()`.
- Use injected RNG with seed from `params`. **Never** `OsRng`, `thread_rng`, or any unseeded source.
- Read book state via `ctx.book` (immutable). Read position via `ctx.state` (immutable).
- Emit `SignalEvent` with a non-trivial `rationale` field. Include the feature name, value, threshold, and a book snapshot at decision time.
- Generate `signal_id` deterministically from `(strategy_id, monotonic_counter)`. Persist the counter across restarts so IDs are unique forever.
- Write a property test asserting your strategy is replay-deterministic.
- Write a replay test against at least one historical session, asserting the signal count and final PnL within tolerance.
- When in doubt about whether something is allowed in `on_event`: assume not.

### DO NOT

- Call `SystemTime::now()`, `Instant::now()`, `chrono::Local::now()`, or `Utc::now()` in hot or cold path. Logging-only `Local::now()` is allowed but discouraged — use the `Clock`.
- Allocate inside `on_event` unless absolutely necessary. Pre-allocate buffers in `on_start`.
- Call `tokio::spawn` from inside `on_event`. Background work goes through `refresh_params`.
- Read files, hit the network, or open sockets from anywhere in the strategy. The strategy receives data as events; it does not fetch data.
- Place an order. Strategies emit `SignalEvent`. Execution decides what to send.
- Reach into `ctx.book` and mutate it. The book is `&` (shared reference). Modifying it is a compile error and a logic error.
- Call into another strategy's code. Strategies are isolated peers. They communicate by emitting events.
- Hardcode tunable parameters. Knobs go in `StrategyParams`, loaded from config, version-bumped on schema change.
- Skip the `rationale` field on a signal. A signal without rationale is unreviewable; the agent that wrote it failed code review.
- Disable risk checks. Risk lives outside the strategy and cannot be loosened from inside.
- Ignore a property-test failure with `#[ignore]` to "fix it later." Fix it now.

### WHEN IN DOUBT

- Check the [glossary](../glossary.md) for the correct term.
- Check the [ADRs](../adr/) for past decisions; they may already cover the question.
- Check [examples/signal_to_fill_flow.md](../examples/signal_to_fill_flow.md) for end-to-end behavior.
- Check the vendored `knowledge/nautilus_trader/crates/trading/` for prior art on lifecycle hooks and registry.
- If none of these answer your question, flag it as an open question and **do not guess**.

## Code Review Checklist

Before submitting any strategy change, verify:

- [ ] No call to `SystemTime::now()`, `Instant::now()`, or `Utc::now()` in strategy code (`grep` your diff)
- [ ] Any RNG use takes its seed from `StrategyParams`
- [ ] `on_event` returns within latency budget on the replay smoke test
- [ ] Every emitted signal has a populated `rationale` field
- [ ] `signal_id` is deterministic (same inputs → same ID)
- [ ] Strategy passes the replay-determinism property test (`replay(events) == replay(events)`)
- [ ] No `tokio::spawn`, no async I/O, no `await` in `on_event` (it should be sync if possible)
- [ ] Cold-path work moved to `refresh_params` and emitted as `StrategyParamUpdated` events
- [ ] Tests use synthetic ticks (unit) and at least one recorded session (replay)
- [ ] If parameters changed: `params_version` bumped; old `StrategyParams` upgrade-path tested
- [ ] Glossary updated if you introduced a new term used in `>= 2` files
