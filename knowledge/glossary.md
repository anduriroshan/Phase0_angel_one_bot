# Glossary

> Canonical definitions for every term used in this system.
> If a term is not defined here, it has no agreed-upon meaning.
> Agents must use these definitions — not invent new semantics.

---

## Market Data Terms

**Tick**
: A single market data update. In this system, a `Tick` is the normalized output of the binary packet parser. See `common::schema::Tick`. One tick = one observation of an instrument's state at a point in time.

**LTP (Last Traded Price)**
: The price at which the most recent trade was matched on the exchange. In our schema, this is the `price` field (after conversion from paise to rupees).

**Paise**
: Indian currency sub-unit. ₹1 = 100 paise. Angel One transmits all prices as integers in paise. The division by 100 happens exactly once, in `ParsedPacket::to_tick()`.

**Instrument / Instrument ID**
: A unique identifier for a tradable entity on the exchange. In NSE, tokens like `26009` (NIFTY 50 index) and `26000` (NIFTY BANK index) are instrument IDs. Stored as `inst_id: i32` in the Tick schema.

**Exchange Timestamp**
: The timestamp assigned by the exchange's matching engine when the market event occurred. This is the authoritative time source for ordering events. Stored as `ts_ns: i64` (nanoseconds since Unix epoch, derived from millisecond source × 1,000,000).

**Sequence Number**
: A monotonically increasing integer assigned by the exchange to each market data event. Used for **gap detection** — if `seq_no` jumps by more than 1, data was lost. Stored as `seq_no: i64`.

**SnapQuote**
: Angel One's most detailed subscription mode (mode 3). Includes LTP, OHLCV, top-5 order book depth, open interest, and circuit limits. Produces 379-byte binary packets.

**Order Book / Book**
: The collection of all resting buy and sell orders at various price levels. See [market_microstructure.md](domain/market_microstructure.md) for full definition.

**Depth**
: The number of price levels visible in the order book. Our SnapQuote feed provides L2 data (best 5 buy + 5 sell levels).

**Spread**
: The difference between the best ask (lowest sell price) and best bid (highest buy price). Represents the cost of immediacy.

**Open Interest (OI)**
: For derivatives (F&O), the total number of outstanding contracts that have not been settled. Only present in SnapQuote mode for F&O instruments.

---

## System Architecture Terms

**Event**
: An immutable record of something that happened at a specific time. Events are the atoms of the system. Examples: a tick arrived, a signal was generated, an order was filled, a heartbeat was sent.

**Event Bus**
: The messaging layer that routes events between system components. Currently implemented as `mpsc` channels (in-process) and ZMQ PUB/SUB (cross-process). See [event_bus.md](runtime/event_bus.md).

**Event Log**
: The persistent, ordered record of all events. The single source of truth. In Phase 0, the Parquet files serve as the event log for market data events.

**Event Sourcing**
: The architectural pattern where state is never mutated directly — instead, state is derived by replaying the event log. See [system_philosophy.md](vision/system_philosophy.md).

**Actor**
: An independent component that processes messages from its inbox, maintains private state, and sends messages to other actors. In this system, actors are Rust crates running as Tokio tasks or separate processes.

**Signal / SignalEvent**
: *(Phase 1+)* A strategy's intent to enter or exit a position. A signal is not an order — it must pass through the risk engine before becoming an order.

**Fill / FillEvent**
: *(Phase 1+)* Confirmation from the exchange that an order was matched (fully or partially). Contains fill price, quantity, and timestamp.

**Execution Report**
: *(Phase 1+)* Any state change in an order's lifecycle: acknowledged, partially filled, fully filled, cancelled, rejected.

---

## Infrastructure Terms

**Hot Sink**
: QuestDB. Real-time queryable storage for live monitoring. Data is written via InfluxDB Line Protocol (ILP) over HTTP. Queryable via SQL at `http://localhost:9000`.

**Cold Sink**
: Parquet files on disk. Columnar, compressed (Zstd), optimized for batch analytics and backtesting. Output path: `./data/raw/YYYY/MM/DD/{inst_id}.parquet`.

**Circuit Breaker**
: A separate binary process that monitors the system's health and PnL via ZMQ, and triggers an emergency shutdown if thresholds are breached. See [risk_engine.md](runtime/risk_engine.md).

**Heartbeat**
: A periodic message (every 20ms) from the ingestion node to the circuit breaker, proving the pipeline is alive. If the heartbeat stops, the circuit breaker triggers.

**Panic Sequence**
: The circuit breaker's emergency shutdown procedure: cancel all orders → exit all positions → hard exit. Irreversible. Requires human restart.

**Grace Period**
: The startup window (default: 10 seconds) during which the circuit breaker suppresses the heartbeat watchdog. Prevents false triggers during system initialization.

**Dry-Run Mode**
: Circuit breaker mode where triggers are logged but no REST API calls are made. Default in Phase 0 (no live orders to cancel).

---

## Data Pipeline Terms

**Normalization**
: The process of converting broker-specific data formats into the Unified Tick Schema. Happens once, at the ingestion boundary. All downstream code works with normalized `Tick` structs.

**Backpressure**
: The mechanism by which a slow consumer signals a fast producer to slow down. Our `mpsc` channel has a capacity of 8192; if the consumer falls behind, the producer blocks.

**Fan-out**
: Distributing a single data stream to multiple consumers. Currently: ticks fan out to storage (Parquet + QuestDB) and PnL monitoring. Future: also to strategy engines.

**Gap Detection**
: Using sequence numbers to detect missing market data. If `seq_no` is not monotonically increasing, some ticks were lost (network issue, parser error, etc.).

---

## Exchange-Specific Terms (NSE / Angel One)

**NSE_CM (Cash Market)**
: National Stock Exchange cash/spot market. Exchange type = 1. Index tokens (NIFTY 50, NIFTY BANK) live here.

**NSE_FO (Futures & Options)**
: NSE derivatives segment. Exchange type = 2. Futures and options contracts live here. Do NOT subscribe index tokens (26009, 26000) on this exchange.

**JWT Token**
: JSON Web Token received from the Angel One login endpoint. Used as `Authorization: Bearer <token>` for REST API calls. Valid until midnight IST.

**Feed Token**
: A session token used specifically for WebSocket authentication (`x-feed-token` header). Different from the JWT.

**TOTP**
: Time-based One-Time Password. Required for Angel One authentication. Generated using SHA1, 6 digits, 30-second window, from the TOTP secret in the developer portal.

**ILP (InfluxDB Line Protocol)**
: A text-based protocol for writing time-series data. Used by QuestDB. Format: `table_name,tag=value field=value timestamp_ns`.

---

## Phase Definitions

**Phase 0 — Data Substrate** *(current)*
: Read-only pipeline. Ingestion, storage, circuit breaker (dry-run). No orders placed. Goal: collect clean market data and prove the pipeline works.

**Phase 1 — Strategy & Execution** *(planned)*
: Strategy engine generates signals. Execution engine places orders. Risk engine enforces limits. Circuit breaker goes live.

**Phase 2 — Multi-Strategy** *(planned)*
: Multiple strategies share infrastructure. Portfolio-level risk. Strategy isolation and performance attribution.
