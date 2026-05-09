# Example Flow: Tick Ingestion Pipeline (Phase 0)

> Concrete, step-by-step trace of a single tick flowing through the system.
> Use this as a reference when implementing or debugging any pipeline component.

---

## Scenario

A NIFTY 50 price update arrives from the Angel One WebSocket during active market hours.

## Flow

```
┌─────────────┐    Binary Frame     ┌──────────────┐
│  Angel One  │ ──────────────────▶ │  WebSocket   │
│  WS Server  │    (379 bytes,      │  Task        │
│             │     SnapQuote)      │              │
└─────────────┘                     └──────┬───────┘
                                           │
                                    parse_binary_packet()
                                           │
                                    ParsedPacket::to_tick()
                                           │
                                    ┌──────▼───────┐
                                    │  mpsc::send  │
                                    │  (Tick)      │
                                    └──────┬───────┘
                                           │
                                    ┌──────▼───────┐
                                    │  Consumer    │
                                    │  Task        │
                                    └──────┬───────┘
                                           │
                                    watch::send(pnl)
                                           │
                              ┌────────────┼────────────┐
                              │            │            │
                       ┌──────▼──┐  ┌──────▼──┐  ┌─────▼──────┐
                       │ Parquet │  │ QuestDB │  │ Heartbeat  │
                       │  Sink   │  │  Sink   │  │ Timer Task │
                       └─────────┘  └─────────┘  └─────┬──────┘
                                                        │
                                                  ZMQ PUB (20ms)
                                                        │
                                                  ┌─────▼──────┐
                                                  │  Circuit   │
                                                  │  Breaker   │
                                                  └────────────┘
```

## Step-by-Step Trace

### Step 1 — Binary Frame Arrives

The Angel One WebSocket server sends a 379-byte binary frame (SnapQuote mode).

**Raw bytes (first 51 — common header):**
```
03 01 32 36 30 30 39 00 ... 00   mode=3(SnapQuote), exchange=1(NSE_CM), token="26009"
[8 bytes: sequence_number = 1163404]
[8 bytes: exchange_timestamp = 1746583245195]  (ms since epoch)
[8 bytes: last_traded_price = 5590555]         (paise → ₹55905.55)
```

**Location:** [`ingestion/src/websocket.rs`](../../ingestion/src/websocket.rs), `Message::Binary(data)` arm of the `select!` loop.

### Step 2 — Binary Parsing

`common::parse_binary_packet(&data)` parses the raw bytes into a `ParsedPacket`:

```rust
ParsedPacket {
    mode: SnapQuote,
    exchange: NseCm,
    token: "26009",
    sequence_number: 1163404,
    exchange_timestamp: 1746583245195,
    last_traded_price: 5590555,
    quote: Some(QuoteData { ... }),
    snap: Some(SnapQuoteData { ... }),
}
```

**Location:** [`common/src/parser.rs`](../../common/src/parser.rs)

### Step 3 — Normalization to Tick

`ParsedPacket::to_tick()` converts to the canonical `Tick` struct:

```rust
Tick {
    ts_ns: 1746583245195 * 1_000_000,  // ms → ns
    inst_id: 26009,
    side: 0,                            // indices have no trade side
    price: 5590555.0 / 100.0,           // paise → ₹55905.55
    qty: 0,                             // indices have no traded qty
    seq_no: 1163404,
}
```

**Key transformations:**
- `exchange_timestamp` × 1,000,000 → `ts_ns` (milliseconds to nanoseconds)
- `last_traded_price` ÷ 100 → `price` (paise to rupees)
- `token` parsed as `i32` → `inst_id`

**Location:** [`common/src/schema.rs`](../../common/src/schema.rs), `ParsedPacket::to_tick()`

### Step 4 — Channel Send

The tick is sent into the `mpsc` channel:

```rust
tx.send(tick).await  // blocks if buffer (8192) is full
```

If the consumer has dropped (channel closed), this returns `Err` and the WebSocket task exits.

### Step 5 — Consumer Receives Tick

The consumer task receives the tick:

```rust
while let Some(tick) = rx.recv().await {
    count += 1;
    // Log every 100th tick
    // Update PnL via watch channel
}
```

**Logging:** Every 100th tick is logged at INFO level with instrument ID, price, and sequence number.

### Step 6 — PnL Update

The consumer sends the current PnL to the heartbeat task via a `watch` channel:

```rust
let _ = pnl_tx.send(0.0);  // TODO: real PnL calculation
```

This is non-blocking. The heartbeat task reads the latest value whenever it fires.

### Step 7 — Heartbeat Publication (Independent Timer)

Every 20ms, the heartbeat task:

1. Reads the latest PnL from the `watch` channel
2. Constructs a `PnlMessage`
3. Serializes to JSON
4. Publishes on ZMQ PUB socket

```json
{"heartbeat": true, "pnl": 0.0, "timestamp": 1746583245}
```

**This happens regardless of whether any ticks arrived.** The timer is independent.

### Step 8 — Circuit Breaker Receives Heartbeat

The circuit breaker's `select!` loop:

1. Receives the JSON on the ZMQ SUB socket
2. Deserializes to `PnlMessage`
3. Updates `last_heartbeat = Instant::now()`
4. Checks `pnl.abs() >= max_loss` (currently 0.0 < 10000.0, so no trigger)

If the heartbeat stops for >50ms (after the grace period), the panic sequence fires.

---

## What Could Go Wrong

| Failure Point | Symptom | Handling |
|---|---|---|
| WebSocket disconnects | `stream.next()` returns `None` or `Err` | Reconnect with exponential backoff (max 5 attempts) |
| Binary parse fails | `parse_binary_packet()` returns `Err` | Log warning with hex dump, continue to next frame |
| Channel full (8192) | `tx.send()` blocks | WebSocket task pauses. Backpressure propagates to network buffers |
| QuestDB down | `write_tick()` returns `Err` | Log error, continue in Parquet-only mode |
| Parquet write fails | `flush_all()` returns `Err` | Log error. Data in buffer is lost for that flush window |
| ZMQ send fails | `zmq_socket.send()` returns `Err` | Log warning. Circuit breaker has no subscriber yet |
| Circuit breaker timeout | `last_heartbeat.elapsed() > 50ms` | Panic sequence → cancel all → exit all → hard exit |

---

## Timing Budget (Typical)

| Step | Duration |
|---|---|
| Binary parse | ~1μs |
| Tick normalization | ~100ns |
| Channel send | ~100ns (unbounded) |
| QuestDB ILP buffer | ~500ns |
| Parquet buffer push | ~200ns |
| ZMQ publish | ~10μs |
| **Total ingestion latency** | **~15μs per tick** |

These are order-of-magnitude estimates. Actual latency depends on system load, channel contention, and ZMQ socket state. The bottleneck is ZMQ publication, not computation.
