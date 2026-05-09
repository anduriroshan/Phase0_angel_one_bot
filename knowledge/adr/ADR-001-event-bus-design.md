# ADR-001: Event Bus Design — `mpsc` + ZMQ (Phase 0)

**Status:** Accepted  
**Date:** 2026-05-07  
**Decision makers:** rosha  

---

## Context

Phase 0 needs two communication channels:

1. **In-process:** WebSocket task → Consumer task (ticks)
2. **Cross-process:** Ingestion node → Circuit breaker (heartbeat/PnL)

We need to choose the messaging primitives for each.

## Decision

### In-Process: `tokio::sync::mpsc`

Use a bounded `mpsc` channel (capacity 8192) for tick transport within the ingestion process.

### Cross-Process: ZeroMQ PUB/SUB over TCP

Use ZeroMQ PUB/SUB (`tcp://127.0.0.1:5555`) for heartbeat messages from ingestion to the circuit breaker.

### Heartbeat/PnL Sharing: `tokio::sync::watch`

Use a `watch` channel to pass the latest PnL from the consumer task to the heartbeat timer task. The timer reads `borrow_and_update()` and publishes on ZMQ every 20ms.

## Alternatives Considered

| Alternative | Why Rejected |
|---|---|
| **`crossbeam` channel** | No async support. Would require `spawn_blocking` wrappers. |
| **`tokio::broadcast`** | Designed for multi-consumer. We have 1 producer, 1 consumer for ticks. Adds unnecessary overhead. |
| **Shared memory / `mmap`** | Overkill for ~4 ticks/sec. Adds unsafe code. |
| **TCP socket (raw)** | Have to reinvent framing, buffering, reconnection. ZMQ does this for free. |
| **gRPC** | Massive dependency for a simple heartbeat message. |
| **Unix IPC** | Not supported on Windows. ZMQ TCP works cross-platform. |

## Tradeoffs

**Advantages:**
- `mpsc` is zero-copy within a process, with backpressure built in.
- ZMQ handles connection/reconnection, framing, and message boundaries automatically.
- `watch` is lock-free for latest-value semantics (perfect for PnL).

**Disadvantages:**
- ZMQ adds a C library dependency (via the `zeromq` crate's pure-Rust impl, so actually no C dep).
- The 8192 `mpsc` buffer is a guess. If market bursts exceed this, the WebSocket task blocks. May need tuning.
- ZMQ PUB silently drops messages when no subscriber is connected (by design, but means we can't "catch up" on missed heartbeats).

## Consequences

- The circuit breaker must be started before or shortly after the ingestion node. Heartbeats sent during the gap are lost.
- The startup grace period (10s) mitigates this: even if heartbeats are lost initially, the watchdog won't trigger.
- When Phase 1 introduces multiple consumers (storage + strategy), we may need `broadcast` or a custom fan-out. This ADR should be revisited then.
