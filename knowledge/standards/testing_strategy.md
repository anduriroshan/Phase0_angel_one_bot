# Testing Strategy

> Defines what we test, at which level, and what each level catches.
> "It compiles" is not a test. "It works in production" is not a test.

**Status:** Phase 0 has unit tests for parsers. Phase 1 adds property, replay, and golden tests.

---

## Test Pyramid

```
        ┌─────────────────────────┐
        │   End-to-end / Replay   │  ~10s of tests, slowest, highest fidelity
        ├─────────────────────────┤
        │   Property / Fuzz       │  ~10s of properties, find unexpected inputs
        ├─────────────────────────┤
        │   Golden Files          │  ~10s, lock down current behavior
        ├─────────────────────────┤
        │   Integration           │  ~10s, multi-component
        ├─────────────────────────┤
        │   Unit                  │  ~100s, fast, narrow
        └─────────────────────────┘
```

Each level catches a different class of bug. None replaces another.

---

## Level 1 — Unit Tests

Fast, narrow, deterministic. Synthetic data. Co-located with the code (`#[cfg(test)] mod tests`).

**What they catch:** logic errors in pure functions; off-by-one; misuse of stdlib.

**What they don't catch:** integration issues, race conditions, schema mismatches.

```rust
#[test]
fn parse_ltp_packet_extracts_token_and_price() {
    let data = make_ltp_packet(token = "26009", price_paise = 5_590_555);
    let pkt = parse_binary_packet(&data).unwrap();
    assert_eq!(pkt.token, "26009");
    assert_eq!(pkt.last_traded_price, 5_590_555);
}
```

**Rules:**
- One assertion per concept (a test can have multiple `assert_eq!` if they test the same property).
- Names describe the property tested, not the function called: `parse_ltp_packet_extracts_token_and_price`, not `test_parse_packet`.
- Use builder helpers (`make_ltp_packet`) — never copy 80 bytes of hex literal across tests.
- No network, no file I/O, no time. Tests run in parallel by default; flaky tests fail this property.

NautilusTrader has good examples of this style in `knowledge/nautilus_trader/crates/*/tests/`.

---

## Level 2 — Integration Tests

Multi-component, still in-process. `tests/` directory at the crate root, run with `cargo test --test`.

**What they catch:** wiring bugs between components; channel topology errors; serialization round-trips.

```rust
#[tokio::test]
async fn tick_flows_from_parser_through_channel_to_storage() {
    let (tx, mut rx) = mpsc::channel(8);
    let consumer = spawn_consumer(rx);
    
    let raw = make_snapquote_packet(/* ... */);
    let pkt = parse_binary_packet(&raw).unwrap();
    let tick = pkt.to_tick();
    tx.send(tick).await.unwrap();
    drop(tx); // signal end-of-stream
    
    let received = consumer.await.unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].inst_id, 26009);
}
```

**Rules:**
- Use `#[tokio::test]` for async. Set `flavor = "current_thread"` when ordering matters.
- Use bounded channels with capacity 1-8 to surface backpressure issues.
- Inject a test `Clock` if any code path needs `now`.

---

## Level 3 — Golden File Tests

Lock down current behavior of complex transformations. Output is checked into git.

**What they catch:** unintended behavior changes; "harmless" refactors that aren't.

```rust
#[test]
fn snapquote_to_tick_matches_golden() {
    let raw = std::fs::read("tests/golden/snapquote_nifty_1.bin").unwrap();
    let tick = parse_binary_packet(&raw).unwrap().to_tick();
    
    let actual = serde_json::to_string_pretty(&tick).unwrap();
    let expected = std::fs::read_to_string("tests/golden/snapquote_nifty_1.json").unwrap();
    
    if actual != expected {
        // Bless mode: UPDATE_GOLDEN=1 cargo test
        if std::env::var("UPDATE_GOLDEN").is_ok() {
            std::fs::write("tests/golden/snapquote_nifty_1.json", &actual).unwrap();
        } else {
            panic!("Golden mismatch.\nExpected:\n{expected}\nActual:\n{actual}");
        }
    }
}
```

**Rules:**
- Never edit golden files by hand. Re-bless via `UPDATE_GOLDEN=1`.
- Review every golden diff in PR — a changed golden is a behavior change requiring justification.
- Keep goldens small (< 5 KB). Large goldens are a sign you're testing too much in one test.

---

## Level 4 — Property Tests

Express invariants; the framework finds counterexamples. Use `proptest`.

**What they catch:** edge cases the author didn't think of; off-by-one at boundaries; numeric overflow.

```rust
proptest! {
    #[test]
    fn tick_price_paise_round_trips_through_rupees(
        price_paise in 1_i64..1_000_000_000_i64
    ) {
        let rupees = price_paise as f64 / 100.0;
        let back = (rupees * 100.0).round() as i64;
        prop_assert_eq!(price_paise, back);
    }

    #[test]
    fn order_book_best_bid_never_above_best_ask(
        levels in any::<[(i64, i64); 10]>()
    ) {
        let book = build_book(levels);
        if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
            prop_assert!(bid.price_paise <= ask.price_paise,
                "Crossed book detected for input {levels:?}");
        }
    }
}
```

**Rules:**
- Express invariants from [vision/design_principles.md](../vision/design_principles.md). Each principle is a candidate property.
- `prop_assert_eq!` / `prop_assert!` give shrunk counterexamples on failure — much more useful than `assert_eq!`.
- Cap input ranges to realistic NSE values (prices up to ₹10M, qty up to 100K). Don't test `f64::MAX`.

Phase 1 minimum properties (must hold or system is broken):
- Tick parse round-trip: serialize → deserialize → equal
- Order book: never crossed after a valid SnapQuote
- Position: sum of fills equals current `qty`
- Replay determinism: replay(events) → state₁; replay(events) → state₂; state₁ == state₂

---

## Level 5 — Replay Tests (Phase 1+)

Highest fidelity. Run a recorded session through the full pipeline. Compare against an expected manifest.

**What they catch:** schema-evolution breakage; cross-component drift; non-determinism.

```rust
#[tokio::test(flavor = "current_thread")]
async fn replay_2026_05_07_basis_arb_produces_expected_signals() {
    let result = ReplayDriver::new()
        .with_data("./tests/replay_data/2026-05-07/")
        .with_strategy(BasisArbV1::new(default_params()))
        .with_fill_model(FillModel::CrossedSpread)
        .with_seed(42)
        .run().await.unwrap();
    
    assert_eq!(result.signal_count, 47);
    assert_eq!(result.fill_count, 41);
    assert!((result.realized_pnl_paise - 12_300).abs() < 100); // ±₹1 tolerance
    
    // Cross-check: every fill traces back to a signal via correlation_id
    for fill in &result.fills {
        let signal = result.signals.iter()
            .find(|s| s.id == fill.causal_signal_id)
            .expect("orphan fill — broken causal chain");
        assert!(signal.ts_ns <= fill.ts_ns);
    }
}
```

**Rules:**
- Replay tests use **real recorded data** from `./data/raw/` partitions. Never synthesize a full day of ticks; that's not the same problem.
- Replay tests are slow (seconds to minutes). They live in `tests/replay/` and are gated behind `cargo test --test replay --release`.
- Determinism is part of the contract: run the same replay twice; results must be byte-identical.

The replay test is the **only** test that validates the research-live parity axiom ([system_philosophy.md axiom 4](../vision/system_philosophy.md#4-research-live-parity)). All others test pieces.

---

## What Each Level Catches

| Bug class | Unit | Integration | Golden | Property | Replay |
|---|:---:|:---:|:---:|:---:|:---:|
| Off-by-one in pure function | ✅ | | | ✅ | |
| Wrong field copied during normalization | ✅ | | ✅ | ✅ | ✅ |
| Channel capacity too small (deadlock) | | ✅ | | | ✅ |
| Strategy uses wall-clock | | | | | ✅ |
| Schema change breaks old logs | | | ✅ | | ✅ |
| Crossed-book edge case | ✅ | | | ✅ | |
| Race between consumer and shutdown | | ✅ | | ✅ | |
| Fill model differs from broker | | | | | ✅ (vs live PnL) |
| `i64` overflow at large qty | | | | ✅ | |
| Component A logs at wrong level | (manual review) | | | | |

If a bug class has no checkmark, it's caught only by code review or production. That's the explicit gap — not every bug can be tested cheaply.

---

## Test Data Conventions

| Type | Location | Lifetime |
|---|---|---|
| Synthetic builders (`make_ltp_packet`) | `common/tests/builders.rs` | Live with code |
| Golden files (small, hand-curated) | `<crate>/tests/golden/` | Reviewed in PRs |
| Replay sessions (real, recorded) | `./tests/replay_data/{YYYY-MM-DD}/` | Big — git-LFS or external storage; checked-in metadata only |
| Golden replay manifests | `./tests/replay_manifests/{YYYY-MM-DD}.json` | Reviewed in PRs |

Replay data is too large for git. The convention is: a small manifest in git names the data, and CI fetches the data from a fixed S3 bucket / local archive before running.

---

## CI Gating

| Stage | Tests run |
|---|---|
| PR check | Unit + Integration + Golden + Property |
| Pre-merge | + Replay against last-known-good session |
| Nightly | + Full replay matrix (last 30 sessions × all strategies) |
| Release | + Full replay matrix + clippy + fmt + cargo-deny |

A flaky test fails the pyramid. Disabling a flaky test without a fix-it ticket is forbidden — see [agents/reviewer.md](../agents/reviewer.md).

---

## What We Don't Test

These are explicitly out of scope for automated testing:

- **Live broker behavior under emergency conditions.** We do not script "fake exchange outage" tests against the live Angel One sandbox. Manual fire drills only, before each phase rollout.
- **Hardware failure modes.** Disk full, OOM, NIC drop. Documented in runbooks; tested manually.
- **Adversarial inputs from the broker.** We trust Angel One to send well-formed packets. Malformed packets are logged and skipped (not tested).

---

## Reference

- `knowledge/nautilus_trader/crates/*/tests/` — examples of unit + integration test style
- `knowledge/nautilus_trader/crates/testkit/` — shared test fixtures and builders

---

## See Also

- [vision/design_principles.md](../vision/design_principles.md) — principle 16: tests replay real data
- [standards/rust_patterns.md](rust_patterns.md#testing-strategy) — Phase 0 baseline
- [standards/event_contracts.md](event_contracts.md) — schema-evolution tests
- [runtime/replay_engine.md](../runtime/replay_engine.md) — replay-test infrastructure
- [examples/replay_session.md](../examples/replay_session.md) — concrete replay walkthrough

**Last verified against commit:** _pending Phase 1 implementation_
