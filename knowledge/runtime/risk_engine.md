# Risk Engine & Circuit Breaker

> Defines the risk management architecture, kill conditions,
> and emergency shutdown procedures.

---

## Philosophy

Risk management is **infrastructure**, not application code. It exists at the system level, below strategies, and cannot be bypassed by any component.

The circuit breaker is the **last line of defense**. It assumes everything else has failed.

---

## Circuit Breaker Architecture

### Process Isolation

The circuit breaker runs as a **separate binary** (`circuit_breaker` crate), in a separate OS process. This is non-negotiable.

```
┌─────────────────┐      ZMQ PUB/SUB       ┌──────────────────┐
│  Ingestion Node │ ──────────────────────▶ │  Circuit Breaker │
│  (ingestion.exe)│  tcp://127.0.0.1:5555   │  (circuit_breaker│
│                 │                         │       .exe)      │
└─────────────────┘                         └───────┬──────────┘
                                                    │
                                              (on trigger)
                                                    │
                                                    ▼
                                            ┌──────────────┐
                                            │ Angel One API │
                                            │ Cancel/Exit   │
                                            └──────────────┘
```

**Rationale:** If the ingestion process panics, leaks memory, or deadlocks, the circuit breaker must still be able to execute the emergency shutdown.

### Kill Conditions

| Condition | Threshold | Behavior |
|---|---|---|
| **Heartbeat timeout** | No message for >50ms (after grace period) | Trigger panic sequence |
| **PnL breach** | Cumulative loss ≥ `CIRCUIT_BREAKER_MAX_LOSS` (default: ₹10,000) | Trigger panic sequence |

### Startup Grace Period

The circuit breaker suppresses the heartbeat watchdog for a configurable grace period (default: 10 seconds) after startup. This prevents false triggers during:

- Ingestion node authentication (~100ms)
- WebSocket connection setup (~500ms)
- First heartbeat transmission

**Configuration:** `CIRCUIT_BREAKER_GRACE_SECS` environment variable.

### Panic Sequence

When triggered, the circuit breaker executes this sequence **exactly once**, then hard-exits:

```
1. Log CIRCUIT BREAKER TRIGGERED
2. If dry-run mode → log warning, skip REST calls, exit(1)
3. Fetch real public IP (api.ipify.org)
4. POST /order/v1/cancelAllOrders
5. POST /order/v1/exitAllPositions
6. Log completion
7. std::process::exit(1)
```

The `exit(1)` is deliberate. The circuit breaker does not attempt to recover. It kills everything and requires human intervention to restart.

### Dry-Run Mode

In Phase 0, `CIRCUIT_BREAKER_DRY_RUN` defaults to `true`:

- The heartbeat watchdog and PnL monitoring run normally.
- If triggered, the panic sequence **logs the event but skips REST API calls**.
- This is correct because Phase 0 places no orders — there's nothing to cancel.

Set `CIRCUIT_BREAKER_DRY_RUN=false` in Phase 1 when the execution engine is live.

---

## Heartbeat Protocol

### Publisher (Ingestion Node)

A dedicated `tokio::spawn` timer publishes heartbeats every 20ms via ZeroMQ PUB:

```json
{"heartbeat": true, "pnl": 0.0, "timestamp": 1700000000}
```

**Critical design decision:** The heartbeat timer runs **independently of tick flow**. It fires every 20ms regardless of whether any market ticks have arrived. This prevents false triggers during:

- Slow market periods (e.g., lunch hours, low-volume days)
- Periods between ticks (indices update ~4 ticks/sec, leaving 250ms gaps)
- Startup before the first tick arrives

### Subscriber (Circuit Breaker)

The circuit breaker runs a `tokio::select!` loop:

1. **ZMQ recv arm:** Receives heartbeat messages, updates `last_heartbeat` timestamp, checks PnL.
2. **Watchdog arm:** Every 10ms, checks if `last_heartbeat.elapsed() > 50ms`. If true (and past grace period), triggers panic.

---

## Future: Pre-Trade Risk Engine (Phase 1+)

When the execution engine is introduced, a pre-trade risk engine will sit between strategy signals and order submission:

```
Strategy ──[Signal]──▶ Risk Engine ──[Approved Order]──▶ Execution Engine
                            │
                            └──[Rejected]──▶ Log + Alert
```

### Pre-Trade Checks (Planned)

| Check | Rule |
|---|---|
| **Position limit** | Max N lots per instrument |
| **Order size** | Max quantity per single order |
| **Daily loss limit** | Cumulative loss < threshold (softer than circuit breaker) |
| **Duplicate order** | No identical orders within T milliseconds |
| **Stale market** | Reject signals if last tick is older than S seconds |
| **Max open orders** | No more than M orders resting simultaneously |

### Distinction from Circuit Breaker

| | Pre-Trade Risk Engine | Circuit Breaker |
|---|---|---|
| **When** | Before order placement | After fault detection |
| **Granularity** | Per-order, per-strategy | System-wide |
| **Response** | Reject individual order | Cancel ALL, exit ALL, hard exit |
| **Recovery** | Strategy can retry | Human intervention required |
| **Process** | Same process as execution | Separate OS process |

---

## Configuration Reference

| Variable | Default | Description |
|---|---|---|
| `CIRCUIT_BREAKER_MAX_LOSS` | `10000` | Max cumulative loss in INR |
| `CIRCUIT_BREAKER_GRACE_SECS` | `10` | Startup grace period (seconds) |
| `CIRCUIT_BREAKER_DRY_RUN` | `true` | Skip REST calls when triggered |
| `ANGEL_JWT_TOKEN` | — | JWT for REST API (auto-loaded from `.jwt_token` file) |
| `ANGEL_API_KEY` | — | Angel One API key |
