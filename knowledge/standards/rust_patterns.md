# Rust Patterns & Standards

> Coding standards for this project. All Rust code must follow these conventions.
> These are not style preferences — they are architectural constraints that
> ensure determinism, replayability, and maintainability.

---

## Crate Organization

```
Phase0_angel_one_bot/
├── common/          # Shared types, schemas, binary parser
│   └── src/
│       ├── lib.rs        # Re-exports
│       ├── schema.rs     # Tick, PnlMessage, ExchangeType, etc.
│       └── parser.rs     # Binary packet parser
├── ingestion/       # WebSocket feed + ZMQ publisher
│   └── src/
│       ├── main.rs       # Orchestrator
│       ├── auth.rs       # REST authentication
│       └── websocket.rs  # WS client + binary frame handling
├── storage/         # Parquet files + QuestDB
│   └── src/
│       ├── lib.rs          # storage_consumer() entry point
│       ├── parquet_sink.rs # Cold sink (Parquet/Arrow)
│       └── questdb_sink.rs # Hot sink (QuestDB ILP)
├── circuit_breaker/ # Separate risk binary
│   └── src/
│       └── main.rs       # ZMQ subscriber + panic sequence
└── knowledge/       # System documentation (this directory)
```

### Rules

1. **`common` has zero runtime dependencies.** It defines types only. It must never depend on `tokio`, `reqwest`, or any I/O framework.

2. **Each crate has a single responsibility.** `ingestion` does not write Parquet files. `storage` does not connect to WebSockets. `circuit_breaker` does not generate signals.

3. **Cross-crate communication is through `common` types.** The `Tick` struct is defined in `common` and used by `ingestion`, `storage`, and (eventually) `strategy`. No crate defines its own tick format.

4. **Binary crates (`main.rs`) are thin orchestrators.** Business logic lives in modules (`auth.rs`, `websocket.rs`, `parquet_sink.rs`). `main.rs` wires components together and handles graceful shutdown.

---

## Ownership & Borrowing Patterns

### Prefer `&T` Over Cloning

```rust
// GOOD — borrows the tick
pub fn write_tick(&mut self, tick: &Tick) -> Result<(), Error> { ... }

// BAD — takes ownership unnecessarily
pub fn write_tick(&mut self, tick: Tick) -> Result<(), Error> { ... }
```

Exception: When data must cross a `tokio::spawn` boundary, `Clone` is necessary because the spawned task has `'static` lifetime.

### Use `Arc` for Shared Immutable Config

```rust
// Config that's shared across tasks — clone the Arc, not the data
let config = Arc::new(load_config());
let config_clone = config.clone();
tokio::spawn(async move { use_config(&config_clone).await });
```

### Interior Mutability: `watch` Over `Mutex`

For single-writer, multi-reader mutable state (like the current PnL):

```rust
// GOOD — lock-free, always returns the latest value
let (pnl_tx, pnl_rx) = tokio::sync::watch::channel(0.0_f64);

// BAD — blocking, contention under load
let pnl = Arc::new(Mutex::new(0.0_f64));
```

---

## Async Patterns

### Use `tokio::select!` for Multiple Event Sources

```rust
loop {
    tokio::select! {
        msg = socket.recv() => { /* handle ZMQ message */ }
        _ = interval.tick() => { /* periodic task */ }
        _ = shutdown.recv() => { break; }
    }
}
```

### Never Block the Async Runtime

```rust
// BAD — blocks the Tokio thread pool
let data = std::fs::read_to_string("file.txt")?;

// GOOD — uses Tokio's async file I/O
let data = tokio::fs::read_to_string("file.txt").await?;

// ACCEPTABLE — for small, fast operations (config loading at startup)
// std::fs is fine during initialization, before the event loop starts
```

### Spawn Dedicated Tasks for Independent Concerns

```rust
// Each concern gets its own task
let ws_handle = tokio::spawn(websocket_stream(tokens, config, tx));
let hb_handle = tokio::spawn(heartbeat_timer(zmq_socket, pnl_rx));
let consumer_handle = tokio::spawn(storage_consumer(rx));
```

---

## Error Handling

### Use `Result<T, E>` Everywhere — No Panics in Libraries

```rust
// GOOD — returns an error the caller can handle
pub fn parse_binary_packet(data: &[u8]) -> Result<ParsedPacket, ParseError> { ... }

// BAD — crashes the process
pub fn parse_binary_packet(data: &[u8]) -> ParsedPacket {
    data[0]; // panics on empty slice
}
```

### Use `thiserror` for Library Error Types

```rust
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Packet too short: expected {expected}, got {actual}")]
    TooShort { expected: usize, actual: usize },
    
    #[error("Unknown subscription mode: {0}")]
    UnknownMode(u8),
}
```

### Use `Box<dyn Error>` in Binary Crate Boundaries

```rust
// In main.rs — acceptable because it's the top-level error boundary
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> { ... }
```

### Log and Continue (for Non-Fatal Errors in Hot Paths)

```rust
// A single bad packet shouldn't crash the ingestion pipeline
match parse_binary_packet(&data) {
    Ok(parsed) => { /* process */ }
    Err(e) => {
        warn!("Binary parse error: {e}");
        // Continue processing next packet
    }
}
```

---

## Logging Standards

### Use `tracing`, Not `println!`

All logging goes through the `tracing` crate with structured fields:

```rust
use tracing::{info, warn, error};

info!("Ingested {tick_count} ticks total");
warn!("QuestDB unavailable, running in Parquet-only mode: {e}");
error!("HEARTBEAT TIMEOUT! No message for {:?}", elapsed);
```

### Log Levels

| Level | Use For |
|---|---|
| `error!` | System failures, circuit breaker triggers, data loss |
| `warn!` | Degraded operation, non-fatal errors, parse failures |
| `info!` | Startup/shutdown, periodic throughput, state changes |
| `debug!` | Per-tick data, frame-level details (disabled in production) |
| `trace!` | Raw byte dumps, internal state snapshots |

### Structured Fields Over String Interpolation

```rust
// GOOD — structured, queryable
info!(inst_id = tick.inst_id, price = tick.price, seq = tick.seq_no, "Tick received");

// ACCEPTABLE — format string (currently used)
info!("Tick #{count}: inst_id={} price={:.2}", tick.inst_id, tick.price);
```

---

## Testing Strategy

### Unit Tests: Synthetic Data

```rust
#[test]
fn test_parse_ltp_packet() {
    let data = make_ltp_packet(); // deterministic, known values
    let pkt = parse_binary_packet(&data).unwrap();
    assert_eq!(pkt.token, "26009");
    assert_eq!(pkt.last_traded_price, 24550);
}
```

### Integration Tests: Replay Recorded Sessions

*(Phase 1 — not yet implemented)*

Record a real market session to Parquet files, then replay through the system and verify the output matches expected behavior.

### Property Tests: Invariants

*(Phase 1 — not yet implemented)*

Use `proptest` or `quickcheck` to verify invariants like:
- Every parsed tick has a non-zero `seq_no`
- Price is always non-negative after normalization
- Parquet files written can be read back with identical data

---

## Dependencies: Selection Criteria

Only add a dependency if:

1. **It solves a non-trivial problem** (e.g., `tokio` for async, `arrow` for columnar data)
2. **It's well-maintained** (recent commits, responsive maintainers)
3. **It doesn't bring a huge transitive dependency tree**
4. **There's no simpler alternative**

Current approved dependencies:

| Crate | Purpose | Why |
|---|---|---|
| `tokio` | Async runtime | Industry standard, required for WebSocket + channels |
| `serde` / `serde_json` | Serialization | Universal Rust serialization |
| `reqwest` | HTTP client | For REST auth and circuit breaker API calls |
| `tokio-tungstenite` | WebSocket client | Async-compatible, well-maintained |
| `zeromq` | Cross-process PUB/SUB | Circuit breaker isolation |
| `arrow` / `parquet` | Columnar storage | Industry standard for tick data |
| `questdb-rs` | QuestDB ILP sender | Official client, minimal |
| `tracing` | Structured logging | De facto Rust logging standard |
| `totp-rs` | TOTP generation | Angel One auth requirement |
| `chrono` | Timestamps | Required for date-based Parquet paths |
| `dotenvy` | Env file loading | Simple, no magic |
| `thiserror` | Error types | Clean error enum derivation |
