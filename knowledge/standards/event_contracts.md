# Event Contracts

> Defines schema versioning, evolution rules, and the discipline that
> keeps `TickEvent`, `SignalEvent`, `OrderEvent`, and `FillEvent` stable
> across phases without breaking replay of historical event logs.

**Status:** Phase 0 has only `Tick` and `PnlMessage`. Phase 1 introduces signal/order/fill events. This document is the contract for all of them.

---

## Why This Matters

The system stores events to Parquet. Strategies are replayed against historical events months later. If we change an event's schema and break compatibility, **all historical event logs become unreadable** — and our entire backtest history disappears.

This is not a hypothetical. It happens whenever someone adds a field "just to test something" and the new code can no longer parse old files. The discipline below prevents that.

---

## The Three Rules

1. **Every event has a `version` field, set at emission time.** Never inferred, never optional.
2. **Adding a field requires bumping the version.** Removing or repurposing a field requires a new event type.
3. **Readers must handle every version that has ever been written.** "Old data" is not a justification for dropping versions; the event log is the audit trail.

---

## Event Type Inventory

Phase 1 events:

| Event | Version | Source | Consumed by |
|---|---|---|---|
| `TickEvent` | 1 | Ingestion | Storage, OrderBook cache, Strategy |
| `BookUpdated` | 1 | OrderBook cache | Strategy, State |
| `SignalEvent` | 1 | Strategy | Risk |
| `RiskApprovedOrder` | 1 | Risk | Execution |
| `RiskRejectedSignal` | 1 | Risk | Strategy (logged), Audit |
| `OrderSubmitted` | 1 | Execution | State, Audit |
| `OrderAcknowledged` | 1 | Broker → Execution | State |
| `OrderRejected` | 1 | Broker → Execution / Risk | State, Strategy |
| `OrderFilled` | 1 | Broker → Execution | State, Strategy |
| `OrderCancelled` | 1 | Broker → Execution | State |
| `OrderStale` | 1 | Execution timeout | State, Audit |
| `PositionUpdated` | 1 | State | Strategy, Risk, UI |
| `PnlUpdated` | 1 | State | Circuit breaker (via heartbeat), UI |
| `Heartbeat` | 1 | Ingestion | Circuit breaker |
| `CircuitBreak` | 1 | Circuit breaker | Audit |
| `SessionStart` / `SessionEnd` | 1 | Scheduler | Strategy, State |
| `StrategyParamUpdated` | 1 | Strategy cold path | Strategy hot path |

Each event lives as a Rust struct in the `common` crate and serializes to a Parquet column or a JSON-on-ZMQ frame, depending on transport.

---

## Schema Skeleton (every event)

```rust
pub struct EventEnvelope<T> {
    pub event_type: &'static str,  // e.g. "SignalEvent"
    pub version: u32,
    pub ts_ns: i64,                // exchange ts when applicable; else system clock
    pub source: ComponentId,       // emitting component
    pub correlation_id: Option<EventId>, // for causal chains
    pub payload: T,
}
```

`event_type` is a stable string. `version` starts at 1 and only increases. `correlation_id` chains causally-related events: every `SignalEvent` carries an ID; the resulting `OrderSubmitted` references that ID; the resulting `OrderFilled` references the `OrderSubmitted` ID. This is what enables end-to-end traceability.

---

## Evolution Patterns

### Adding an Optional Field — Same Version

You can **only** do this if **all writers** populate the new field with a documented default that **all readers** must accept. Even then, prefer a version bump because future-you will misremember the default.

```rust
// V1
struct SignalEvent {
    pub signal_id: SignalId,
    // ...
}

// Adding `rationale` — bump to V2 (preferred), even though you could "default" it to None
```

### Adding a Required Field — Bump Version

```rust
// V1
struct SignalEvent { /* ... */ }

// V2
struct SignalEventV2 {
    /* all V1 fields */
    pub rationale: SignalRationale,  // NEW, required
}

// In code:
enum SignalEventVersioned {
    V1(SignalEvent),
    V2(SignalEventV2),
}

impl SignalEventVersioned {
    pub fn upgrade_to_latest(self) -> SignalEventV2 {
        match self {
            Self::V2(s) => s,
            Self::V1(s) => SignalEventV2 {
                /* copy fields */,
                rationale: SignalRationale::Unknown, // documented "I don't know" sentinel
            },
        }
    }
}
```

Old replay logs return `V1`; the strategy engine always operates on the latest in-memory shape via `upgrade_to_latest()`. The `Unknown` sentinel is **not** the same as a default value — it explicitly says "this data didn't exist when this event was written," and downstream analytics can filter accordingly.

### Removing a Field — New Event Type

You don't remove fields from an existing version. The old version is immortal as long as historical Parquet files exist. To remove a field, define a new event type (`SignalEventV3` if drastic — but more commonly, a new event name like `RefinedSignal`) and stop emitting the old one going forward. The reader must still handle `SignalEventV1` and `SignalEventV2` for replay.

### Repurposing a Field — Forbidden

Never give an existing field a new meaning. If `qty` used to mean "lots" and now means "shares," you have silently broken every historical record. Add a new field with a new name (`qty_shares: i64`), keep the old field for legacy versions, and document the cutover.

---

## Wire Formats

| Transport | Format | Versioning |
|---|---|---|
| In-process channels (`mpsc`, `watch`) | Direct Rust types | Compile-time |
| ZMQ PUB/SUB (cross-process) | JSON | Field `version` in payload |
| Parquet (event log on disk) | Arrow schema | One file per `(event_type, version)` |

JSON for ZMQ is fine in Phase 1 because the volume is low (heartbeats, occasional control). Phase 2+ may switch to a binary format (FlatBuffers, SBE) if measurement shows JSON parsing is on the hot path. Don't preemptively optimize — see [vision/system_philosophy.md](../vision/system_philosophy.md#5-correctness-over-speed).

The Parquet "one file per event_type/version" partitioning means readers can quickly identify which versions are present in a given date range without parsing payloads. Output path:

```
./data/events/{YYYY/MM/DD}/{event_type}_v{version}.parquet
```

---

## ID Generation

| ID | Generated by | Format | Stable across restart? |
|---|---|---|---|
| `EventId` | Emitter | UUIDv7 (time-sortable) | Yes (random + time) |
| `SignalId` | Strategy | `{strategy_id}-{counter}` | Yes (counter persisted per strategy) |
| `client_order_id` | Execution | `{strategy_id}-{signal_id}-{attempt}` | Yes (deterministic from signal) |
| `broker_order_id` | Broker | Opaque string | Yes (broker-provided) |

Deterministic IDs (signal, client_order_id) are required for idempotency. UUIDv7 for `EventId` gives natural time ordering and global uniqueness without coordination.

---

## Causal Chains

Every `SignalEvent` produces zero or more downstream events. The `correlation_id` field chains them:

```
SignalEvent(id=S1)
    └─▶ RiskApprovedOrder(corr=S1)
            └─▶ OrderSubmitted(corr=S1, order_id=O1)
                    ├─▶ OrderAcknowledged(corr=O1)
                    ├─▶ OrderPartiallyFilled(corr=O1)
                    └─▶ OrderFilled(corr=O1)
                            └─▶ PositionUpdated(corr=O1)
                                    └─▶ PnlUpdated(corr=O1)
```

Audit queries traverse this graph to answer: "Why did the system enter position X at time T?" — start from the position update, follow `correlation_id` backward to the originating signal, read its `rationale`. This is the key observability win of structured event contracts.

---

## Validation

Every event passes through a validator at emit time and at parse time:

- **Type-level invariants** (Rust type system): impossible states are unrepresentable. E.g., `Side` is an enum, not an `i8`.
- **Range invariants** (validation function): `qty > 0`, `price_paise > 0`, `ts_ns within reasonable window`.
- **Cross-field invariants**: e.g., a `LIMIT` order must have `limit_price_paise.is_some()`.

Validation failure on emit = bug in emitter (panic in debug, log + drop in release). Validation failure on parse = data corruption (log error, skip event, continue replay).

---

## Migration Discipline

When introducing a schema change:

1. Open an ADR (`adr/ADR-XXX-{event_name}-v2.md`) describing the change and why.
2. Implement the new versioned struct.
3. Implement `upgrade_to_latest()`.
4. Add tests: write V1, read V1, upgrade, assert latest shape.
5. Bump the writer version. Old logs remain readable.
6. **Never touch existing on-disk Parquet files.** They are append-only artifacts.

---

## See Also

- [vision/system_philosophy.md](../vision/system_philosophy.md#2-event-sourcing) — events are immutable; logs are append-only
- [vision/design_principles.md](../vision/design_principles.md#data-integrity-principles) — schema is the contract (principle 14)
- [runtime/event_bus.md](../runtime/event_bus.md) — event types in flight
- [runtime/replay_engine.md](../runtime/replay_engine.md) — replay must read every historical version
- [glossary.md](../glossary.md) — `Event`, `Event Log`, `Schema` definitions

**Last verified against commit:** _pending Phase 1 implementation_
