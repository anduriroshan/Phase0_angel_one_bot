# Next Steps — After Successful Backtest

## Current State (as of May 13, 2026)

- **All code compiles** — `cargo build --workspace` succeeds (warnings only)
- **All tests pass** — 65/65 green
- **Backtest runs end-to-end** — produces orders, positions, PnL
- **Data pipeline fixed** — `cargo run -p ingestion` now writes ticks to both QuestDB and Parquet
- **Bid/ask swap fixed** — `best_5_sell[0]` = bid, `best_5_buy[0]` = ask (Angel One's naming is inverted)
- **Exchange type routing fixed** — NFO tokens → exchange_type=2, NSE/equity → exchange_type=1
- **Subscription reads from `config/trading.toml`** — no more hardcoded tokens

## Prerequisite: Confirm Clean Data

Before proceeding, verify the May 14 backtest has **zero** `Quote has crossed prices` warnings:

```powershell
cargo run -p backtest -- --date 2026-05-14 2>&1 | Select-String "crossed"
```

If warnings still appear, the bid/ask data is still inverted — do NOT proceed until this is clean.

---

## Step 1: Build `AngelOneExecutionClient` (Order Placement)

**Goal:** Enable real order placement on Angel One via REST API.

**File:** `adapter_angelone/src/execution.rs`

**What exists:** A skeleton that implements NautilusTrader's `ExecutionClient` trait with dry-run mode.

**What to implement:**

1. `submit_order(cmd: SubmitOrder)` → build REST payload → POST to Angel One:
   ```
   POST https://apiconnect.angelone.in/rest/secure/angelbroking/order/v1/placeOrder
   Headers: Authorization: Bearer {jwt}, X-PrivateKey: {api_key}
   Body: {
     "variety": "NORMAL",
     "tradingsymbol": "<from InstrumentMapping>",
     "symboltoken": "<token>",
     "transactiontype": "BUY" | "SELL",
     "exchange": "NSE" | "NFO",
     "ordertype": "MARKET" | "LIMIT",
     "producttype": "MIS" | "CARRYFORWARD",
     "duration": "DAY",
     "quantity": "<qty>",
     "price": "<price if LIMIT>"
   }
   ```

2. `cancel_order(cmd: CancelOrder)` → POST to Angel One cancel endpoint

3. Parse response → emit `OrderAccepted` or `OrderRejected` back to NautilusTrader

**Safety:**
- `ANGEL_ONE_DRY_RUN=true` (default in `.env`) → logs the payload but does NOT send
- Set `ANGEL_ONE_DRY_RUN=false` ONLY when ready to go live
- Test with 1 share of a ₹50-150 stock (IOC, SAIL, etc.)

**Config needed in `.env`:**
```
ANGEL_ONE_DRY_RUN=true
```

---

## Step 2: Add Low-Price Stocks to `config/trading.toml`

Add 2-3 stocks suitable for live testing (₹50-150 range, high liquidity):

```toml
[[instruments]]
symbol              = "IOC"
token               = 1624          # VERIFY from Angel One ScripMaster
trading_symbol      = "IOC-EQ"
exchange            = "NSE"
product_type        = "MIS"

[[instruments]]
symbol              = "SAIL"
token               = 2963          # VERIFY from Angel One ScripMaster
trading_symbol      = "SAIL-EQ"
exchange            = "NSE"
product_type        = "MIS"
```

**IMPORTANT:** Verify token numbers from Angel One ScripMaster before adding:
https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json

---

## Step 3: Live Dry-Run Test

Run with dry-run enabled to verify the full pipeline works without placing real orders:

```powershell
# Terminal 1: QuestDB
docker compose up -d

# Terminal 2: Live trading node (dry-run)
$env:ANGEL_ONE_DRY_RUN="true"
cargo run -p trading
```

**Verify in logs:**
- Ticks flowing (bid/ask both non-zero)
- Strategy generating signals
- Order commands logged (but not sent)
- Parquet files appearing in `./data/raw/YYYY/MM/DD/`

---

## Step 4: Live Order Test (Real Money — Small)

Once dry-run looks perfect:

```powershell
$env:ANGEL_ONE_DRY_RUN="false"
cargo run -p trading
```

**Risk controls:**
- `config/strategy_intraday_vwap.toml` → set `capital_per_stock` to ₹500-1000
- `config/nse_risk.toml` → verify `max_qty_per_stock` is set to 1-5 shares
- MIS product type = auto-squared by broker at 3:15 PM (no overnight risk)
- Worst case loss: a few hundred rupees

---

## Step 5: Collect Data for ML (Parallel — 2-3 Weeks)

While live-testing, the system is collecting tick data to `./data/raw/`.

After 2 weeks you'll have:
- ~500,000 rows per instrument per day × 10 days = 5 million rows per stock
- Bid, ask, spread, imbalance, volume — all the tick features needed for ML

---

## Step 6: Historical OHLCV Download (Can Start Immediately)

Write a script to pull 1 year of 1-minute OHLCV from Angel One:

```
GET https://apiconnect.angelone.in/rest/secure/angelbroking/historical/v1/getCandleData
Headers: Authorization: Bearer {jwt}, X-PrivateKey: {api_key}
Body: {
  "exchange": "NSE",
  "symboltoken": "1624",
  "interval": "ONE_MINUTE",
  "fromdate": "2025-05-14 09:15",
  "todate": "2025-06-14 15:30"
}
```

**Note:** API limits to ~30 days per request. Loop month-by-month to get 1 year.

Save as parquet in `./data/ohlcv/YYYY/MM/{token}.parquet`.

---

## Step 7: ML Model Training

**Recommended first model:**

1. **Input features** (from OHLCV + derived):
   - Returns (1m, 5m, 15m lookback)
   - RSI(14), MACD, Bollinger band width
   - Volume ratio (current / 20-period avg)
   - Time-of-day encoding
   - Intraday VWAP deviation

2. **Label:** sign(close[t+5] - close[t]) → binary classification (up/down)

3. **Model:** LightGBM or XGBoost (fast, interpretable, works well with tabular data)

4. **Framework:** Python (scikit-learn / lightgbm), export to ONNX

5. **Integration:** new `strategy_ml/` Rust crate that loads ONNX model via `ort` crate, calls `model.predict(features)` on each tick

---

## Architecture Reference

```
config/trading.toml          ← instruments, tokens, exchanges
config/strategy_*.toml       ← strategy parameters
.env                         ← credentials (never commit)

ingestion/src/main.rs        ← data-only collection node
trading/src/main.rs          ← full LiveTradingNode (data + strategies + orders)
adapter_angelone/src/
  data.rs                    ← WebSocket → QuoteTick (DataClient)
  execution.rs               ← Order placement (ExecutionClient)
  decode.rs                  ← Binary frame parser
storage/src/
  parquet_sink.rs            ← Cold storage (./data/raw/)
  questdb_sink.rs            ← Hot storage (real-time queries)
backtest/src/main.rs         ← Replay parquet data through strategies
```

---

## Key Gotchas for the Next Agent

1. **Angel One's depth field names are inverted** — `best_5_buy` = ASK side, `best_5_sell` = BID side. This is fixed in `common/src/schema.rs` and `adapter_angelone/src/decode.rs`. Do NOT "fix" it back.

2. **NautilusTrader owns logging** — never call `tracing_subscriber::fmt::init()` before creating a `LiveNode` or `BacktestEngine`. The `ingestion` binary is fine because it doesn't use NautilusTrader.

3. **Exchange type matters** — NSE equities/index = 1 (NSE_CM), NFO derivatives = 2 (NSE_FO). Wrong value = zero data from WebSocket (no error, just silence).

4. **Parquet filenames** — format is `{token}_{flush_timestamp_ms}.parquet`. The backtest reads all files matching the token prefix in the date folder.

5. **MIS orders auto-square at 3:15 PM IST** — the broker does this. Strategy should close positions by 2:45 PM to avoid unfavorable auto-square pricing.

6. **ScripMaster tokens change on expiry** — futures tokens change every month. Always verify against the live ScripMaster JSON before updating `config/trading.toml`.
