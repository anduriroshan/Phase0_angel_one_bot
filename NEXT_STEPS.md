# Next Steps (Updated May 29, 2026)

## Current State

- **Backtest engine works** — replays Parquet tick data through NautilusTrader
- **Two strategies running:** `strategy_basis_arb` (NIFTY futures-vs-spot) + `strategy_intraday_vwap` (INFY, HCLTECH, SUNPHARMA)
- **Bugs fixed:** correct NIFTY 50 spot token (26000), correct lot size (65), stop-loss cooldown, crossed-quote filter, basis-arb ratio sanity check
- **May 29 backtest:** PnL +₹302.83, Win Rate 60%, Expectancy +₹4.92, 216 orders, 4 positions

---

## Strategy Hardening (Quick Wins)

| # | Task | Why |
|---|------|-----|
| 1 | **Add `max_z_score` cap to basis-arb** | The z=-24.2 signal on May 29 is a warm-up artifact (30 samples, near-zero variance, then sudden price jump). Cap at e.g. \|z\| > 10 → skip. Add a `max_z_score` field to `BasisArbParams` in config. |
| 2 | **Require minimum std_dev in basis-arb** | When `std_dev < 1.0` (₹0.01 in paise), the z-score blows up on noise. Add `min_std_dev` guard. |
| 3 | **Multi-day backtest validation** | Run on all available data days (not just May 22 & 29) to confirm positive expectancy holds. |
| 4 | **VWAP parameter sweep** | Expectancy is marginal (+₹4.92). Try z_score_threshold 2.5, tighter stop_loss_pct, or wider re-entry cooldown to improve risk-adjusted returns. |

---

## Phase 1 Checklist Progression

| Step | Status | What |
|------|--------|------|
| 1 | **Done** | NautilusTrader crates wired (strategies + backtest running) |
| 6 | **Done** | `strategy_basis_arb` implemented |
| **2** | **Next** | `adapter_angelone` DataClient — live WebSocket → `QuoteTick` / `OrderBookDeltas` |
| **3** | Blocked by 2 | `adapter_angelone` ExecutionClient — orders via REST |
| **4** | Blocked by 3 | E2E integration test (mock WS + mock broker) |
| **5** | Blocked by 4 | `risk_nse` crate (lot size, freeze qty, STT trap) |

---

## Recommended Execution Order

1. **Z-score cap + min std_dev** — prevents bogus basis-arb signals at market open
2. **Multi-day backtest** — validates both strategies before investing in adapter work
3. **Step 2: `adapter_angelone` DataClient** — the core custom work that enables live trading
4. **Step 3: ExecutionClient** — order placement via Angel One REST API
5. **Step 4: E2E test** — mock WS + mock broker, full signal-to-fill pipeline
6. **Step 5: `risk_nse`** — NSE F&O specific pre-trade checks
7. **Live dry-run** — real data, logged orders, no execution
8. **Live trading** — small size (1 lot NIFTY, 1-5 shares equity)

---

## Key Gotchas

1. **Angel One's depth field names are inverted** — `best_5_buy` = ASK side, `best_5_sell` = BID side. Fixed in `common/src/schema.rs` and `adapter_angelone/src/decode.rs`. Do NOT "fix" it back.
2. **NautilusTrader owns logging** — never call `tracing_subscriber::fmt::init()` before creating a `LiveNode` or `BacktestEngine`.
3. **Exchange type matters** — NSE equities/index = 1 (NSE_CM), NFO derivatives = 2 (NSE_FO). Wrong value = zero data from WebSocket (no error, just silence).
4. **Parquet filenames** — format is `{token}_{flush_timestamp_ms}.parquet`. The backtest reads all files matching the token prefix in the date folder.
5. **MIS orders auto-square at 3:15 PM IST** — strategy should close positions by 2:45 PM to avoid unfavorable auto-square pricing.
6. **ScripMaster tokens change on expiry** — futures tokens change every month. Always verify against the live ScripMaster JSON before updating `config/trading.toml`.
