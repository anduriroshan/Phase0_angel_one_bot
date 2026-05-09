# ADR-002: Storage Architecture — Dual-Sink (Hot + Cold)

**Status:** Accepted  
**Date:** 2026-05-07  
**Decision makers:** rosha  

---

## Context

Phase 0 collects market tick data that must be stored for two purposes:

1. **Real-time monitoring:** Query current prices, volume, and OI during live market hours.
2. **Offline analysis:** Batch analytics, backtesting, ML feature engineering over historical data.

These are fundamentally different access patterns that benefit from different storage systems.

## Decision

### Hot Sink: QuestDB (ILP over HTTP)

- Write ticks to QuestDB via InfluxDB Line Protocol at `localhost:9000`.
- Auto-flush every 1000 rows.
- QuestDB runs in Docker (`docker-compose.yml`).
- Queryable via SQL at `http://localhost:9000` (web console).

### Cold Sink: Parquet Files (Arrow + Zstd)

- Buffer ticks in memory, grouped by instrument ID.
- Flush to disk as `.parquet` files every 60 minutes or when buffer reaches 500K rows.
- Compression: Zstd (level 3).
- Output path: `./data/raw/YYYY/MM/DD/{inst_id}.parquet`

### Graceful Degradation

If QuestDB is unavailable (Docker not running), the system continues in **Parquet-only mode**. This is logged as a warning, not an error.

## Alternatives Considered

| Alternative | Why Rejected |
|---|---|
| **QuestDB only** | Parquet files are essential for backtesting in Python (pandas, polars). QuestDB is for monitoring, not research. |
| **Parquet only** | No real-time querying capability. Can't monitor the pipeline during live market hours. |
| **ClickHouse** | Heavier than QuestDB. More complex deployment. QuestDB's ILP is simpler for tick data. |
| **TimescaleDB (Postgres)** | Row-oriented. Poor compression for tick data. Parquet wins for analytics. |
| **CSV files** | No compression, no columnar access, no type safety. Unacceptable for financial data. |
| **SQLite** | Row-oriented, poor concurrent write performance, no columnar analytics. |
| **Apache Kafka + downstream** | Massive infrastructure for a single-machine pipeline. |

## Tradeoffs

**Advantages:**
- Parquet is the industry standard for offline quant analytics. Every Python/Rust data tool reads it natively.
- QuestDB provides instant SQL queries over live data with no ETL step.
- Dual-sink means a bug in one doesn't lose data if the other is healthy.
- Zstd compression gives ~5-10x size reduction on tick data.

**Disadvantages:**
- Two storage systems means two codepaths to maintain.
- QuestDB requires Docker, which adds deployment complexity.
- Parquet files are write-once. Appending to an existing file requires reading, merging, and rewriting. Our current approach creates a new file per flush window, which may produce many small files.

## Consequences

- Running `docker compose up -d` is a prerequisite for hot-sink functionality, but not for the pipeline itself.
- Parquet files accumulate in `./data/raw/`. Users must monitor disk usage.
- The 60-minute flush interval means up to 60 minutes of data could be lost on a crash. This is acceptable for Phase 0 (we can re-run during the next session). Phase 1 should reduce this interval or add a WAL.
- The per-instrument file partitioning (`{inst_id}.parquet`) means each file is small and fast to load for single-instrument analysis, but cross-instrument queries require reading multiple files.
