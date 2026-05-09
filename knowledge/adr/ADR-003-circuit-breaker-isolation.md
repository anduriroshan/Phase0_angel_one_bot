# ADR-003: Circuit Breaker Process Isolation

**Status:** Accepted  
**Date:** 2026-05-07  
**Decision makers:** rosha  

---

## Context

The circuit breaker is the system's last line of defense. When triggered, it must cancel all orders and exit all positions — even if the rest of the system has crashed, hung, or gone into an undefined state.

The key question: should the circuit breaker run in-process (as a Tokio task) or as a separate OS process?

## Decision

**Separate OS process.** The circuit breaker compiles to its own binary (`circuit_breaker.exe`) and communicates with the ingestion node via ZeroMQ PUB/SUB over TCP.

## Alternatives Considered

| Alternative | Why Rejected |
|---|---|
| **In-process Tokio task** | If the ingestion process panics, OOMs, or deadlocks, the circuit breaker dies with it. Unacceptable for a kill switch. |
| **Systemd watchdog (Linux)** | Not portable to Windows. Also can't check PnL — only process liveness. |
| **Separate thread (same process)** | Better than a Tokio task, but still dies if the process is killed by the OS (e.g., OOM killer). |
| **External monitoring service** | Adds infrastructure dependency. The circuit breaker should work on a single laptop with no external services. |

## Tradeoffs

**Advantages:**
- The circuit breaker survives ingestion process crashes.
- It can be restarted independently without restarting ingestion.
- The ZMQ PUB/SUB boundary enforces a clean API (JSON messages only — no shared state).
- It can be tested in isolation by publishing mock heartbeats.

**Disadvantages:**
- Two binaries to start, manage, and keep running.
- ZMQ adds a dependency and a TCP port (`5555`).
- If the ingestion node crashes, the circuit breaker loses its heartbeat source and will trigger after 50ms (by design, but potentially disruptive during development).
- The startup grace period (10s) is a workaround for the fact that the two processes start independently and the heartbeat source isn't immediately available.

## Consequences

- Users must start both binaries: `cargo run -p ingestion` and `cargo run -p circuit_breaker`.
- The startup order matters: ingestion should ideally start first (it binds the ZMQ PUB socket), but the grace period makes the order flexible.
- In production, both should be managed by a process supervisor (e.g., `supervisord`, Windows Task Scheduler, or a simple wrapper script).
- The `--dry-run` mode prevents accidental REST API calls during development (defaulting to `true` in Phase 0).
