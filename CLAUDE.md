# Phase 0 Angel One Trading Bot

## What to build next

The active task list is **[knowledge/PHASE_1_CHECKLIST.md](knowledge/PHASE_1_CHECKLIST.md)**.
Work through it step by step — do not skip ahead.

Architecture decision: we use NautilusTrader's Rust crates as the trading
engine foundation. See [adr/ADR-007-nautilus-trader-foundation.md](knowledge/adr/ADR-007-nautilus-trader-foundation.md).

## Project overview

Rust-based algorithmic trading system for Angel One (Indian stock broker).
Phase 0 (complete) handles real-time market data ingestion and storage.
Phase 1 (active) adds the full trading pipeline by building an Angel One
adapter on top of NautilusTrader's production-grade Rust crates.

## What NautilusTrader provides (DO NOT reimplement)

- Order book maintenance (`nautilus-model::orderbook`)
- Clock abstraction (`nautilus-common::clock` — LiveClock + TestClock)
- Execution FSM (`nautilus-execution`)
- Portfolio / position / PnL (`nautilus-portfolio`)
- Pre-trade risk engine (`nautilus-risk`)
- Strategy framework (`nautilus-trading` — Actor + Strategy traits)
- Backtest / replay engine (`nautilus-backtest`)
- Parquet persistence (`nautilus-persistence`)

## What we write

- `adapter_angelone/` — DataClient + ExecutionClient for Angel One
- `risk_nse/` — NSE F&O specific checks (freeze qty, lot size, STT trap)
- `strategy_basis_arb/` — first strategy (Actor trait impl)
- `trading/` — LiveTradingNode binary wiring everything
- `circuit_breaker/` — unchanged from Phase 0

## Crate layout

| Crate | Purpose |
|---|---|
| `common` | Shared types, binary parser (Phase 0) |
| `ingestion` | WebSocket streaming from Angel One (Phase 0) |
| `storage` | Parquet (cold) + QuestDB (hot) sinks (Phase 0) |
| `circuit_breaker` | Out-of-process kill switch (ZMQ heartbeat) |
| `adapter_angelone` | Angel One DataClient + ExecutionClient (Phase 1) |
| `risk_nse` | NSE F&O risk checks (Phase 1) |
| `strategy_basis_arb` | Basis-arb reference strategy (Phase 1) |
| `trading` | LiveTradingNode binary (Phase 1) |

## How to run (Phase 0)

```bash
docker compose up -d           # QuestDB
cargo run -p ingestion         # Terminal 1
cargo run -p circuit_breaker   # Terminal 2
```

## Key design rules

- All prices in integer paise (no f64 in hot path)
- All timestamps via NautilusTrader's Clock trait (no SystemTime::now())
- Event sourcing: state = fold over immutable event log
- Replay determinism: same inputs → bit-identical outputs
- Risk is infrastructure: cannot be bypassed by strategies
