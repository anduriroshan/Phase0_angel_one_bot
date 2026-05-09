# Example Flow: Signal → Risk → Order → Fill (Phase 1)

> Concrete, step-by-step trace of one strategy decision flowing from
> a market tick all the way to an exchange fill. Phase 1 counterpart
> to [tick_ingestion_flow.md](tick_ingestion_flow.md).

**Status:** Phase 1 (planned). This is the spec the Phase 1 implementation must satisfy.

---

## Scenario

Active NSE F&O hours, May 9 2026 at 11:42:30 IST. The basis-arbitrage strategy is monitoring NIFTY May futures vs. NIFTY index. The futures-cash basis widens beyond the strategy's threshold. A signal is emitted. Risk approves. Execution submits a LIMIT sell on the futures. Broker acks. Fill arrives partially first, then completes.

This is a happy-path trace. Failure-mode variants follow at the end.

---

## Topology

```
┌──────────────┐    Tick     ┌───────────┐  BookUpdated  ┌──────────────┐
│  ingestion   │────────────▶│ orderbook │──────────────▶│   strategy   │
│   binary     │             │  cache    │               │   engine     │
└──────────────┘             └───────────┘               └──────┬───────┘
                                                                │ SignalEvent
                                                                ▼
                                                         ┌──────────────┐
                                                         │     risk     │
                                                         │    engine    │
                                                         └──────┬───────┘
                                                                │ RiskApprovedOrder
                                                                ▼
                                                         ┌──────────────┐    HTTP POST     ┌─────────────┐
                                                         │  execution   │─────────────────▶│  Angel One  │
                                                         │    engine    │◀───── ack ───────│  REST API   │
                                                         └──────┬───────┘                  └─────────────┘
                                                                │ events
                                                                ▼
                                                         ┌──────────────┐
                                                         │ state engine │
                                                         └──────────────┘
```

All components in the green box (`orderbook`, `strategy`, `risk`, `execution`, `state`) live in the `trading` binary. See [adr/ADR-006-execution-engine-isolation.md](../adr/ADR-006-execution-engine-isolation.md).

---

## Step-by-Step Trace

### Step 1 — Tick Arrives

A SnapQuote tick for `NIFTY26MAY24FUT` (token = 35001 — illustrative) arrives via the `mpsc` from the ingestion relay:

```rust
Tick {
    ts_ns: 1746790950_123_456_789,   // 11:42:30.123 IST
    inst_id: 35001,
    side: 0,
    price: 22148.55,                  // last traded
    qty: 25,                          // 1 lot just printed
    seq_no: 1_852_417,
}
```

The tick (with attached `SnapQuoteData`) updates the `OrderBookCache`:

```rust
on_tick(tick) {
    book = books.entry(35001).or_default();
    book.bids = parse_buy_levels(tick.snap.depth);
    book.asks = parse_sell_levels(tick.snap.depth);
    book.last_seq_no = 1_852_417;
    book.last_update_ns = tick.ts_ns;
    bus.emit(BookUpdated { inst_id: 35001, ts_ns: tick.ts_ns });
}
```

Span: `book.update`. Latency: ~3 µs. See [runtime/order_book.md](../runtime/order_book.md).

### Step 2 — Strategy Receives `BookUpdated`

The basis-arbitrage strategy is subscribed to `BookUpdated` for both legs (NIFTY futures token 35001 and NIFTY index token 26009). On every update for either leg, it recomputes the basis:

```rust
fn on_event(&mut self, ctx: &StrategyContext, event: &MarketEvent) -> Vec<SignalEvent> {
    let MarketEvent::BookUpdated { inst_id, ts_ns } = event else { return vec![]; };
    if !self.tracks(inst_id) { return vec![]; }

    let fut = ctx.book.get(self.fut_token)?;
    let idx = ctx.book.get(self.idx_token)?;

    let fut_mid = fut.mid_price_paise()? as f64 / 100.0;
    let idx_mid = idx.mid_price_paise()? as f64 / 100.0;

    let basis_now = fut_mid - idx_mid;
    self.basis_buffer.push(basis_now);

    let mean = self.basis_buffer.mean();
    let std  = self.basis_buffer.std();
    let z    = (basis_now - mean) / std;

    if z > self.params.entry_z {
        return vec![ self.make_sell_futures_signal(ctx, fut, basis_now, z) ];
    }
    if z < -self.params.entry_z {
        return vec![ self.make_buy_futures_signal(ctx, fut, basis_now, z) ];
    }
    vec![]
}
```

Computed: `basis_now = 8.40`, `mean = 4.15`, `std = 1.20`, `z = 3.54`. The strategy threshold is `entry_z = 2.5`. **Signal: SELL futures.**

Span: `strategy.on_event` (strategy_id=basis_arb_v1). Latency: ~30 µs. See [runtime/strategy_engine.md](../runtime/strategy_engine.md).

### Step 3 — `SignalEvent` Emitted

```rust
SignalEvent {
    signal_id: SignalId("basis_arb_v1-04219"),
    strategy_id: "basis_arb_v1",
    ts_ns: 1746790950_123_487_200,   // ctx.clock.now_ns() at emission
    inst_id: 35001,
    side: Side::Sell,
    qty: 25,                          // 1 NIFTY lot
    order_type: OrderType::Limit,
    limit_price_paise: Some(22_148_50),  // 22148.50 — at best bid (cross spread for fill)
    time_in_force: TimeInForce::Ioc,     // immediate or cancel
    rationale: SignalRationale::Structured {
        feature_name: "basis_zscore",
        feature_value: 3.54,
        threshold: 2.5,
        book_snapshot: BookSnapshot {
            best_bid: 22_148_50, best_bid_qty: 75,
            best_ask: 22_148_55, best_ask_qty: 50,
        },
    },
    params_version: 7,
}
```

Sent on `mpsc<SignalEvent>` from strategy → risk.

Field discipline per [standards/event_contracts.md](../standards/event_contracts.md). The `rationale` is non-optional and includes the threshold value and the book snapshot at decision time.

### Step 4 — Risk Pre-Trade Checks

The risk engine receives the signal. It runs the pre-trade checks listed in [runtime/risk_engine.md](../runtime/risk_engine.md#pre-trade-checks-planned):

| Check | Result |
|---|---|
| Position limit (max 5 lots NIFTY futures) | Current = 0 lots; signal would set it to -1 (short 1 lot). Within limit. |
| Order size (max 10 lots/order) | 1 lot. Within limit. |
| Daily loss limit (₹8,000 soft cap) | Current realized PnL = ₹420. Within limit. |
| Duplicate order (no identical order in last 500ms) | No prior orders. OK. |
| Stale market (last tick < 1s old) | `now - last_update_ns = 31µs`. Fresh. |
| Max open orders (max 4 simultaneously) | Current = 0. OK. |
| Margin check (sufficient cash + buffer) | Required = ~₹73,000. Available = ₹245,000. OK. |
| Circuit limit (limit price within band) | Bid 22,148.50 is between lower and upper circuits. OK. |
| Tick alignment (price aligned to ₹0.05) | 22148.50 is aligned. OK. |

All pass. Risk emits:

```rust
RiskApprovedOrder {
    signal_id: SignalId("basis_arb_v1-04219"),
    client_order_id: "basis_arb_v1-04219-00",
    inst_id: 35001,
    side: Side::Sell,
    qty: 25,
    order_type: OrderType::Limit,
    limit_price_paise: 22_148_50,
    time_in_force: TimeInForce::Ioc,
    correlation_id: signal_id.into(),
    ts_ns: clock.now_ns(),
}
```

Span: `risk.pre_trade_check` per check. Latency: ~25 µs total. The risk engine emits one event per check at `debug!` for traceability.

### Step 5 — Execution: Build Order Payload

The execution engine builds the Angel One order payload from `RiskApprovedOrder`:

```json
{
  "variety": "NORMAL",
  "tradingsymbol": "NIFTY26MAY24FUT",
  "symboltoken": "35001",
  "transactiontype": "SELL",
  "exchange": "NFO",
  "ordertype": "LIMIT",
  "producttype": "INTRADAY",
  "duration": "IOC",
  "price": "22148.50",
  "quantity": "25",
  "ordertag": "basis_arb_v1-04219-00"
}
```

`ordertag` is the `client_order_id`. This is the **idempotency key** — if the network drops the response and we retry, the broker recognizes the same `ordertag` and returns the existing order rather than creating a duplicate. See [runtime/execution_engine.md](../runtime/execution_engine.md#order-identity--idempotency).

Order state: `NEW` → `SUBMITTED`. The execution engine logs:

```
INFO target=execution span=submit_order client_order_id="basis_arb_v1-04219-00" inst_id=35001 side=Sell qty=25 limit_price_paise=22148_50
```

### Step 6 — HTTP Submit

`reqwest` posts the payload to `https://apiconnect.angelone.in/rest/secure/angelbroking/order/v1/placeOrder` with the standard auth headers.

Span: `submit_order.http`. Latency budget: < 100 µs request setup, ~15 ms wall (network).

### Step 7 — Ack Received

The broker responds (HTTP 200, ~14 ms later):

```json
{
  "status": true,
  "message": "SUCCESS",
  "errorcode": "",
  "data": {
    "script": "NIFTY26MAY24FUT",
    "orderid": "230509000147219",
    "uniqueorderid": "abc-def-...",
    "ordertag": "basis_arb_v1-04219-00"
  }
}
```

Order state: `SUBMITTED` → `ACKNOWLEDGED`. The execution engine maps `client_order_id → broker_order_id` in its journal:

```rust
journal.insert(
    "basis_arb_v1-04219-00".into(),
    InflightOrder {
        broker_order_id: "230509000147219".into(),
        state: OrderState::Acknowledged,
        last_known_at_ns: clock.now_ns(),
    }
);
```

Emits `OrderAcknowledged`. Latency from submit to ack: ~14.2 ms (network-dominated).

### Step 8 — Partial Fill

200 ms later, an `OrderUpdate` arrives via Angel One's order WebSocket (or via REST poll, depending on Angel One's push capabilities — Phase 1 handles both):

```json
{
  "orderid": "230509000147219",
  "status": "PARTIALLY_FILLED",
  "filledqty": 15,
  "averageprice": "22148.50",
  ...
}
```

State: `ACKNOWLEDGED` → `PARTIALLY_FILLED`. The execution engine emits:

```rust
OrderPartiallyFilled(FillEvent {
    order_id: "230509000147219",
    client_order_id: "basis_arb_v1-04219-00",
    inst_id: 35001,
    side: Side::Sell,
    fill_qty: 15,
    fill_price_paise: 22_148_50,
    cumulative_qty: 15,
    leaves_qty: 10,
    correlation_id: signal_id.into(),
    ts_ns: ...,
});
```

The state engine ([runtime/state_engine.md](../runtime/state_engine.md)) folds the fill into position:

```
Position(NIFTY26MAY24FUT) {
    qty: -15,                        // short 15 (out of 25 ordered)
    avg_entry_price_paise: 22_148_50,
    realized_pnl_paise: 0,
}
```

### Step 9 — Full Fill

100 ms later, another `OrderUpdate`:

```json
{
  "orderid": "230509000147219",
  "status": "FULLY_FILLED",
  "filledqty": 25,
  "averageprice": "22148.50",
  ...
}
```

State: `PARTIALLY_FILLED` → `FILLED` (terminal). The execution engine emits `OrderFilled` with the final qty. State engine updates:

```
Position(NIFTY26MAY24FUT) {
    qty: -25,
    avg_entry_price_paise: 22_148_50,
    realized_pnl_paise: 0,
    last_update_ns: ...
}
Portfolio {
    cash_paise: ...,
    margin_used_paise: ~73_00_000,   // ₹73,000 SPAN+Exp
    realized_pnl_paise: 420_00,
    unrealized_pnl_paise: 0,         // book is at our entry price
}
```

The execution engine drops the entry from its journal (terminal state). The strategy may now act on the position (set up an exit, scale out, etc.) — that's a separate signal, separate flow.

### Step 10 — Audit Trail

By correlation_id, the entire causal chain is now traversable in the event log:

```
SignalEvent(basis_arb_v1-04219)
  └─▶ RiskApprovedOrder(corr=basis_arb_v1-04219)
        └─▶ OrderSubmitted(client_order_id=basis_arb_v1-04219-00)
              ├─▶ OrderAcknowledged(broker_order_id=230509000147219)
              ├─▶ OrderPartiallyFilled(qty=15)
              └─▶ OrderFilled(qty=25)
                    └─▶ PositionUpdated(qty=-25)
                          └─▶ PnlUpdated
```

A backtest replays the event stream up to step 5; from there, the simulated fill model takes over. A live audit traces the same chain to answer "why did we short NIFTY at 22148.50 at 11:42:30?"

---

## Timing Budget (this trace)

| Stage | Elapsed | Cumulative |
|---|---|---|
| Tick → BookUpdated emitted | 3 µs | 3 µs |
| BookUpdated → SignalEvent | 30 µs | 33 µs |
| Risk pre-trade checks | 25 µs | 58 µs |
| Order build + serialize | 8 µs | 66 µs |
| HTTP send (in-process) | 80 µs | 146 µs |
| **Tick → submit (in-process)** | | **~150 µs** |
| Network RTT to broker | 14 ms | 14.15 ms |
| **Tick → ack (wall)** | | **~14.2 ms** |
| Time to first fill | 200 ms | 214 ms |
| Time to full fill | 100 ms | 314 ms |

Compare against [domain/latency_budget.md](../domain/latency_budget.md). In-process budget P95 < 250 µs; we hit 150 µs. End-to-end wall is dominated by network and broker queue dynamics.

---

## What Could Go Wrong

| Failure point | Symptom | Handling |
|---|---|---|
| Strategy needs `ctx.clock.now_ns()` but mistakenly uses `Instant::now()` | Replay produces different signals than live | Caught by replay determinism tests + code review (see [ADR-005](../adr/ADR-005-replay-determinism.md)) |
| Pre-trade risk rejects | Signal emits `RiskRejectedSignal`; no order sent | Strategy logs and may try a different signal; nothing leaks to the broker |
| HTTP submit times out | Order in `SUBMITTED` for > `ACK_TIMEOUT_MS` | Execution **queries broker** for status before retrying. See [execution_engine.md](../runtime/execution_engine.md#retry-semantics) |
| Broker returns `errorcode = "AB1018" margin shortfall` | `OrderRejected` emitted; state unchanged | Strategy receives the rejection event and may downscale or stop |
| Network drop after ack but before any fill | `OrderUpdate` push misses; status diverges from broker | Execution polls REST `/getOrderBook` periodically as a backstop. Reconciliation on restart catches anything missed. |
| Broker order WebSocket disconnects | New fills miss our journal | Reconnect + reconcile (call `/getOrderBook`); apply any state transitions found |
| Two `OrderFilled` events arriving for the same order | Double-counting in state engine | State engine deduplicates by (`order_id`, `cumulative_qty`); duplicates are logged and dropped |

---

## See Also

- [examples/tick_ingestion_flow.md](tick_ingestion_flow.md) — Phase 0 ingestion trace
- [examples/replay_session.md](replay_session.md) — same flow, replayed
- [runtime/order_book.md](../runtime/order_book.md), [runtime/strategy_engine.md](../runtime/strategy_engine.md), [runtime/risk_engine.md](../runtime/risk_engine.md), [runtime/execution_engine.md](../runtime/execution_engine.md), [runtime/state_engine.md](../runtime/state_engine.md)
- [standards/event_contracts.md](../standards/event_contracts.md) — event schemas
- [standards/observability.md](../standards/observability.md) — span / metric names referenced
- [domain/nse_fo_specifics.md](../domain/nse_fo_specifics.md) — lot sizes, freeze qty, margin

**Last verified against commit:** _pending Phase 1 implementation_
