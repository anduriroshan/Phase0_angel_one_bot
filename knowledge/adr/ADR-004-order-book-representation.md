# ADR-004: Order Book Representation — Fixed Top-5 Arrays

**Status:** Accepted
**Date:** 2026-05-09
**Decision makers:** rosha
**Supersedes:** —

---

## Context

Phase 1 introduces an in-memory order book for each subscribed instrument. The book is updated from Angel One SnapQuote (mode 3) packets, which contain best 5 buy + best 5 sell levels — a **full snapshot** every tick, never deltas.

Strategies and risk checks query the book on every event for derived quantities: best bid/ask, mid, spread, microprice, imbalance, depth at level. These reads are on the hot path; the data structure must be cache-friendly and allocation-free.

We need to choose the representation.

## Decision

Use a fixed-size, stack-allocated `[Level; 5]` per side, stored inside a per-instrument `OrderBook` struct. The cache of all instruments is a `HashMap<i32, OrderBook>` keyed by `inst_id`.

```rust
pub struct Level { price_paise: i64, qty: i64, num_orders: u16 }
pub struct OrderBook {
    inst_id: i32,
    bids: [Level; 5],
    asks: [Level; 5],
    last_seq_no: i64,
    last_update_ns: i64,
}
```

Prices and quantities are stored as integer paise / shares — never `f64` rupees. Conversion to rupees happens only at the display/serialization boundary.

## Alternatives Considered

| Alternative | Why Rejected |
|---|---|
| **`Vec<Level>` per side** | Allocation on every update. We always have exactly 5 levels — variable-length is dishonest about the data shape. |
| **`BTreeMap<i64, Level>` per side** | Designed for L3 add/cancel/modify deltas. We replace top-5 wholesale every tick — no delta operations. ~10× slower iteration than an array for our use case. |
| **`SmallVec<[Level; 5]>`** | Hybrid: stack until you exceed 5, then heap. We never exceed 5 in this feed; the runtime branch is wasted code. |
| **Lock-free flat-combining ring** | Premature. We have a single writer (the ingestion task) and many readers (strategies, risk). `RwLock<HashMap<i32, OrderBook>>` is sufficient at our event rate. |
| **Separate `bid_prices: [i64; 5]`, `bid_qtys: [i64; 5]`** (struct-of-arrays) | Marginal cache benefit; loses readability; complicates analytics that need both fields together. Revisit only if profiling shows AoS is the bottleneck. |
| **Floating-point rupees for `price`** | Equality comparisons, sums, and tick-size alignment break with `f64` rounding. NSE prices are exactly representable as integer paise; preserve that. |

## Tradeoffs

**Advantages:**
- Stack-allocated. Zero heap traffic on book updates.
- Cache-line-friendly: `[Level; 5]` = ~120 bytes; both sides fit in two cache lines.
- Simple, auditable: no generic depth handling; the data shape matches what Angel One sends.
- Integer paise gives exact equality, exact sums, exact tick-size alignment.

**Disadvantages:**
- If Angel One adds a "mode 4 = top 20" subscription later, the structure must grow. Mitigated: it would be a deliberate schema change, gated by a new ADR.
- Strategies that want deeper-than-5 depth cannot use this directly. Acceptable for Phase 1; the data feed doesn't provide it.
- Iteration over `Vec` collections (e.g., for analytics across all instruments) is slightly more code-noisy with fixed arrays. Negligible.

## Consequences

- The `OrderBook` and `Level` types live in the `runtime` crate (TBD; alternatively `common` if shared with replay/storage).
- Strategy code that walks levels uses `for level in &book.bids` rather than dynamic depth assumptions.
- A future "deep-book" feed (Angel One mode 4, or a different broker with TBT) requires a new representation. This is a deliberate fork point — write a new ADR and a new struct (`DeepOrderBook`); do not generalize the existing one.
- The HashMap lookup is per-event but O(1); not a hot-path concern at ≤20 instruments.
- The existing principle of "schema is the contract" ([design_principles.md](../vision/design_principles.md) #14) applies: any change to `Level`'s field set requires updating glossary and every consumer.

---

## See Also

- [runtime/order_book.md](../runtime/order_book.md) — full design
- [domain/exchange_protocols.md](../domain/exchange_protocols.md#snapquote-extension-mode-3-bytes-123378) — SnapQuote layout
- [standards/rust_patterns.md](../standards/rust_patterns.md) — ownership, allocation patterns
