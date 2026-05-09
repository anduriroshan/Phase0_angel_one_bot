# Event Bus Design

> Defines the event routing, message types, and ordering guarantees
> for the system's internal communication layer.

---

## Current Architecture (Phase 0)

Phase 0 uses a simple two-layer messaging system:

### Layer 1 — Tokio `mpsc` Channel (In-Process)

**Path:** Ingestion → Consumer / Storage

```
WebSocket Task ──[mpsc::Sender<Tick>]──▶ Consumer Task
                                              │
                                              ├──▶ Storage (Parquet + QuestDB)
                                              └──▶ PnL update (watch channel)
```

- **Type:** `tokio::sync::mpsc::channel::<Tick>(8192)`
- **Ordering:** FIFO within a single producer. Ticks arrive in exchange timestamp order.
- **Backpressure:** Bounded channel (8192 capacity). If the consumer falls behind, the WebSocket task blocks on `send()`.
- **Failure mode:** If the consumer drops, `send()` returns `Err`, and the WebSocket task logs and exits.

### Layer 2 — ZeroMQ PUB/SUB (Cross-Process)

**Path:** Ingestion → Circuit Breaker

```
Heartbeat Timer ──[zmq PUB]──▶ tcp://127.0.0.1:5555 ──▶ [zmq SUB] Circuit Breaker
```

- **Protocol:** ZeroMQ PUB/SUB over TCP (IPC not supported on Windows).
- **Message format:** JSON-encoded `PnlMessage`:
  ```json
  {"heartbeat": true, "pnl": 0.0, "timestamp": 1700000000}
  ```
- **Frequency:** Every 20ms (independent timer, not tied to tick arrival).
- **Ordering:** Single publisher, single subscriber. Messages arrive in order.
- **Failure mode:** If no subscriber is connected, messages are silently dropped (PUB/SUB semantics).

### Layer 3 — `watch` Channel (In-Process, Latest-Value)

**Path:** Consumer → Heartbeat Timer

```
Consumer Task ──[watch::Sender<f64>]──▶ Heartbeat Task (reads latest PnL)
```

- **Type:** `tokio::sync::watch::channel::<f64>(0.0)`
- **Semantics:** Latest-value only. The heartbeat task always reads the most recent PnL. Old values are overwritten.
- **Use:** Decouples PnL computation (happens per-tick in consumer) from heartbeat publishing (happens on a fixed 20ms timer).

---

## Future Architecture (Phase 1+)

### Event Types

When the strategy and execution engines are introduced, the event bus will carry these typed events:

```rust
enum SystemEvent {
    // Market data
    MarketTick(Tick),              // Raw normalized tick
    
    // Strategy signals
    Signal(SignalEvent),           // Strategy wants to enter/exit
    
    // Risk decisions
    RiskApproval(RiskEvent),       // Risk engine approves/rejects signal
    
    // Execution
    OrderSubmitted(OrderEvent),    // Order sent to exchange
    OrderAcknowledged(OrderEvent), // Exchange confirmed receipt
    OrderFilled(FillEvent),        // Full or partial fill
    OrderCancelled(OrderEvent),    // Order withdrawn
    OrderRejected(OrderEvent),     // Exchange refused order
    
    // Portfolio
    PositionUpdate(PositionEvent), // Position changed
    PnlUpdate(PnlEvent),          // PnL recalculated
    
    // System
    Heartbeat(HeartbeatEvent),     // Health check
    CircuitBreak(CircuitEvent),    // Emergency shutdown
}
```

### Ordering Guarantees

1. **Causal ordering:** Events that are causally related (e.g., `Signal` → `RiskApproval` → `OrderSubmitted`) must be processed in causal order.
2. **Timestamp monotonicity:** Within a single event stream, timestamps are strictly non-decreasing.
3. **No global ordering across independent streams:** Market ticks and system events are independent streams. Their relative ordering is defined by timestamp comparison, not by a global sequence.

### Replay Semantics

For backtesting, the event bus reads from stored event logs instead of live sources:

- All events are tagged with a `source_timestamp` (exchange time) and `system_timestamp` (when we processed it).
- Replay uses `source_timestamp` for ordering.
- The simulation clock advances to the next event's timestamp, not wall-clock time.
- Non-deterministic inputs (e.g., actual fill latency) are either recorded and replayed, or modeled with configurable assumptions.

---

## Design Decisions

### Why Not a Full Event Bus Framework in Phase 0?

Phase 0 is data collection only. A full event bus (e.g., based on `tokio::broadcast` or a custom sequencer) adds complexity without benefit when there's only one producer and one consumer.

The current `mpsc` + `zmq` design is deliberately simple. The event bus will be formalized when strategies are introduced in Phase 1, because that's when multiple consumers with different processing speeds will exist.

### Why ZMQ Instead of In-Process for the Circuit Breaker?

The circuit breaker runs as a **separate OS process**. This is a deliberate safety decision:

- If the ingestion process panics, hangs, or OOMs, the circuit breaker is unaffected.
- The circuit breaker can be started, stopped, and restarted independently.
- In an emergency, killing the ingestion process doesn't kill the circuit breaker's ability to cancel orders.

ZeroMQ PUB/SUB provides the cross-process communication with minimal overhead.
