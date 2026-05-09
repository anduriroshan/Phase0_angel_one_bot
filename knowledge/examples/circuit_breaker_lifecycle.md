# Example Flow: Circuit Breaker Lifecycle

> Complete trace of the circuit breaker from startup through trigger.

---

## Scenario A: Normal Operation (No Trigger)

```
t=0.000s  circuit_breaker.exe starts
          ├── Load .env (CIRCUIT_BREAKER_MAX_LOSS=10000, GRACE_SECS=10, DRY_RUN=true)
          ├── Load JWT from .jwt_token file
          ├── Connect ZMQ SUB to tcp://127.0.0.1:5555
          └── Start select! loop. Grace period active.

t=0.010s  Watchdog fires (every 10ms)
          └── startup_time.elapsed() = 10ms < grace_period (10s) → SUPPRESSED

t=0.050s  First ZMQ message received: {"heartbeat": true, "pnl": 0.0, ...}
          └── last_heartbeat = Instant::now()

t=0.070s  Another heartbeat received
          └── last_heartbeat updated

          ... (heartbeats arrive every 20ms) ...

t=10.000s Grace period expires.
          Watchdog now actively checks last_heartbeat.elapsed()

t=10.010s Watchdog fires. last_heartbeat.elapsed() = 5ms < 50ms → OK
t=10.020s Watchdog fires. last_heartbeat.elapsed() = 2ms < 50ms → OK

          ... (system runs normally for hours) ...

t=21600s  Market closes (15:30 IST). User stops ingestion node.
          Heartbeats stop. Circuit breaker triggers after 50ms.
          └── DRY_RUN=true → logs warning, exits cleanly.
```

## Scenario B: Heartbeat Timeout (Ingestion Crash)

```
t=600.000s  System running normally. Heartbeats arriving every 20ms.
t=600.020s  Last heartbeat received.
t=600.025s  Ingestion node panics (e.g., OOM, WebSocket fatal error).
            ZMQ PUB socket dies. No more heartbeats.
t=600.030s  Watchdog fires. last_heartbeat.elapsed() = 10ms < 50ms → OK
t=600.040s  Watchdog fires. elapsed = 20ms → OK
t=600.050s  Watchdog fires. elapsed = 30ms → OK
t=600.060s  Watchdog fires. elapsed = 40ms → OK
t=600.070s  Watchdog fires. elapsed = 50ms → OK (exactly at threshold, not over)
t=600.080s  Watchdog fires. elapsed = 60ms > 50ms → TRIGGER!

            ╔══════════════════════════════════════╗
            ║   CIRCUIT BREAKER TRIGGERED          ║
            ║   Executing emergency shutdown...    ║
            ╚══════════════════════════════════════╝

            DRY_RUN=true:
            └── "DRY-RUN mode — skipping REST order cancellation."
            └── exit(1)

            DRY_RUN=false (Phase 1):
            ├── Fetch public IP from api.ipify.org
            ├── POST /cancelAllOrders → log response
            ├── POST /exitAllPositions → log response
            └── exit(1)
```

## Scenario C: PnL Breach

```
t=3600.000s  System running. Strategy has accumulated losses.
t=3600.020s  Heartbeat received: {"heartbeat": true, "pnl": -9500.0, ...}
             └── pnl.abs() = 9500 < 10000 → OK

t=3600.040s  Heartbeat received: {"heartbeat": true, "pnl": -10200.0, ...}
             └── pnl.abs() = 10200 >= 10000 → PnL BREACH!

             "PnL BREACH DETECTED! PnL=-10200.00 exceeds max_loss=10000.00"
             → execute_panic_sequence()
             → exit(1)
```

---

## Key Timing Parameters

| Parameter | Value | Source |
|---|---|---|
| Heartbeat publish interval | 20ms | Ingestion node timer task |
| Watchdog check interval | 10ms | `WATCHDOG_INTERVAL` constant |
| Heartbeat timeout threshold | 50ms | `HEARTBEAT_TIMEOUT` constant |
| Startup grace period | 10s | `CIRCUIT_BREAKER_GRACE_SECS` env var |
| Max detection latency | ~60ms | Timeout (50ms) + watchdog interval (10ms) |

The circuit breaker can detect and react to a system failure within **~60ms** of the last heartbeat.
