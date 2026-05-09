# Order Book Maintenance

> Defines how the system maintains a live, queryable representation of the
> NSE order book from Angel One SnapQuote packets, and how it detects and
> recovers from missing data.

**Status:** Phase 1 (planned). Phase 0 ingests SnapQuote packets but does not maintain a book.

---

## What Our Data Contains (and Does Not)

The system subscribes to Angel One SnapQuote (mode 3). Each tick gives us:

- `last_traded_price`, `last_traded_qty`, `volume`, `open_interest`
- **Best 5 buy levels** + **best 5 sell levels** (price, qty, num_orders per level)
- Circuit limits and 52-week range

What we do **not** have:

- L3 (per-order) data — we don't see who is in the queue
- Order book deltas — every SnapQuote is a **full snapshot**, not an incremental update
- Trade-by-trade prints — we see aggregated snapshots, not every match

This means our book representation is **L2, top-5, snapshot-based**. We never need to apply add/cancel/modify deltas. We just replace the top of the book on each tick. See [domain/market_microstructure.md](../domain/market_microstructure.md#what-our-data-does-not-contain) for the full implications on strategy design.

---

## Book Representation

### `OrderBook` (per instrument)

```rust
pub struct OrderBook {
    inst_id: i32,
    bids: [Level; 5],   // index 0 = best (highest) bid
    asks: [Level; 5],   // index 0 = best (lowest) ask
    last_seq_no: i64,
    last_update_ns: i64, // exchange timestamp
}

pub struct Level {
    price_paise: i64,    // store as integer paise; convert to f64 only at display
    qty: i64,
    num_orders: u16,
}
```

**Why fixed-size arrays** (not `Vec` or `BTreeMap`): the SnapQuote payload is fixed at 5 levels per side. A `[Level; 5]` is stack-allocated, cache-line-friendly, and forbids accidental allocation in the hot path. See [adr/ADR-004-order-book-representation.md](../adr/ADR-004-order-book-representation.md) for rejected alternatives.

**Why integer paise** (not `f64` rupees) for prices in the book: equality, ordering, and aggregation must be exact. Rupees-as-`f64` introduces rounding errors that compound across ticks. The single division by 100 happens at the display/serialization boundary, never in the book itself. Mirrors design principle 13 in [vision/design_principles.md](../vision/design_principles.md#data-integrity-principles).

### Multi-instrument storage

```rust
pub struct OrderBookCache {
    books: HashMap<i32, OrderBook>,  // keyed by inst_id
}
```

Keyed by `inst_id` (i32). Lookup is O(1). One book per subscribed token. For Phase 1 with ≤20 instruments, this is more than sufficient.

---

## Update Algorithm

On each `Tick` (with `SnapQuoteData` populated):

```text
on_tick(tick):
    book = books.entry(tick.inst_id).or_default()
    if tick.seq_no <= book.last_seq_no:
        # Out-of-order or duplicate → drop. Exchange seq_no is authoritative.
        warn!(...); return
    if tick.seq_no != book.last_seq_no + 1 and book.last_seq_no != 0:
        # Gap detected → log, but still apply (we have the latest snapshot)
        warn!(gap = tick.seq_no - book.last_seq_no - 1, ...)
    book.bids = parse_buy_levels(tick.snap.depth)
    book.asks = parse_sell_levels(tick.snap.depth)
    book.last_seq_no = tick.seq_no
    book.last_update_ns = tick.ts_ns
    emit BookUpdated(inst_id, ts_ns)
```

Because each SnapQuote is a full snapshot, gap recovery is automatic — we don't need to replay deltas. A gap is logged for observability but is not a correctness issue.

This is **fundamentally different** from a tick-by-tick (TBT) feed (NASDAQ ITCH, NSE TBT). With TBT, a gap means the book is permanently desynchronized until a snapshot rebuild. With SnapQuote, every tick is a snapshot.

---

## Derived Book Quantities

Strategies and risk checks query the book through pure functions. None of these mutate state:

```rust
impl OrderBook {
    pub fn best_bid(&self) -> Option<&Level>;
    pub fn best_ask(&self) -> Option<&Level>;
    pub fn mid_price_paise(&self) -> Option<i64>;
    pub fn spread_paise(&self) -> Option<i64>;
    pub fn microprice_paise(&self) -> Option<i64>;       // qty-weighted mid
    pub fn imbalance(&self, levels: u8) -> Option<f64>;  // (bid_qty - ask_qty) / (bid_qty + ask_qty)
    pub fn depth_at_or_better(&self, side: Side, price_paise: i64) -> i64;
    pub fn is_crossed(&self) -> bool;                    // best_bid >= best_ask (data error)
    pub fn is_locked(&self) -> bool;                     // best_bid == best_ask
}
```

See [domain/market_microstructure.md](../domain/market_microstructure.md) for the meaning of these quantities. All formulas operate on integer paise; conversion to rupees happens only at the display boundary.

---

## Book Validity Checks

Before any strategy consumes a book, the system asserts:

| Check | Failure mode |
|---|---|
| **Best bid < best ask** | Crossed book → log error, mark book stale, do not generate signals |
| **All bid prices monotonically decreasing** | Malformed packet → log warning, drop the tick |
| **All ask prices monotonically increasing** | Malformed packet → log warning, drop the tick |
| **Last update within freshness window** | If `now_ns - last_update_ns > stale_threshold_ns`, mark stale (default: 1s) |
| **`seq_no` strictly increasing** | Out-of-order tick → drop |

A stale book disables signal generation but does **not** trigger the circuit breaker. The circuit breaker reacts to heartbeat loss and PnL, not to data freshness. Stale-data response is a strategy-level concern, not a system-level kill condition.

---

## Reference Architecture: NautilusTrader

The vendored NautilusTrader codebase implements a more general L3 order book for crypto exchanges. Read these files to understand patterns we can adapt for our L2-only NSE feed:

| Concept | NautilusTrader path |
|---|---|
| Top-level book API | `knowledge/nautilus_trader/crates/model/src/orderbook/book.rs` |
| Per-side ladder | `knowledge/nautilus_trader/crates/model/src/orderbook/ladder.rs` |
| Single price level | `knowledge/nautilus_trader/crates/model/src/orderbook/level.rs` |
| Top-N aggregation | `knowledge/nautilus_trader/crates/model/src/orderbook/aggregation.rs` |
| Analytics (microprice, etc.) | `knowledge/nautilus_trader/crates/model/src/orderbook/analysis.rs` |

**What to steal:** the `BookLevel` and `Ladder` shape, the analytics formulas, the validity checks.

**What NOT to copy:** their L3 delta engine (`apply_delta`, `apply_deltas`). NSE SnapQuote does not give us deltas; we replace the top-5 wholesale.

---

## Phase 1 Scope

For Phase 1, the order book module:

1. Builds and maintains an `OrderBookCache` keyed by `inst_id`.
2. Updates on every `Tick` with `SnapQuoteData`.
3. Exposes the derived-quantities API above to the strategy and risk engines.
4. Emits a `BookUpdated(inst_id, ts_ns)` event on each successful update (subscribed by strategies that want push updates rather than polling).
5. Logs gap detections at `warn!` level. Logs crossed/locked books at `error!` level.

**Not in Phase 1:** L3 reconstruction, queue position estimation, hidden-order inference. These require TBT data we do not have.

---

## Implementation Location (planned)

- `runtime/src/order_book/mod.rs` — public API
- `runtime/src/order_book/book.rs` — `OrderBook` struct + update logic
- `runtime/src/order_book/level.rs` — `Level`, `Side`
- `runtime/src/order_book/cache.rs` — `OrderBookCache` (multi-instrument)
- `runtime/src/order_book/analytics.rs` — pure-function derived quantities

The `runtime` crate does not yet exist. Phase 1 will add it.

---

## See Also

- [domain/market_microstructure.md](../domain/market_microstructure.md) — order book theory
- [domain/exchange_protocols.md](../domain/exchange_protocols.md#snapquote-extension-mode-3-bytes-123378) — SnapQuote binary layout
- [runtime/event_bus.md](event_bus.md) — `BookUpdated` event routing
- [runtime/strategy_engine.md](strategy_engine.md) — how strategies consume the book
- [adr/ADR-004-order-book-representation.md](../adr/ADR-004-order-book-representation.md) — representation tradeoffs
- [glossary.md](../glossary.md) — `Order Book`, `Depth`, `Spread`, `SnapQuote` definitions

**Last verified against commit:** _pending Phase 1 implementation_
