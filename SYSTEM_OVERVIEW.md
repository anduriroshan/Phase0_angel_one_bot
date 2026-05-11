# What We Built — A Plain-English Guide

> **Who this is for:** Anyone curious about what this trading bot does, even if
> you have never written a line of code or placed a stock market order in your
> life.

---

## The Big Picture

Imagine you want to trade stocks on the Indian stock exchange (NSE) — but
instead of sitting at a screen all day clicking "Buy" and "Sell", you write a
set of rules in advance, and a computer follows those rules for you
automatically, at lightning speed, 24/5.

That is exactly what this project does. It is an **algorithmic trading bot**
connected to **Angel One** (a popular Indian stockbroker) that:

1. Watches live price changes in real time.
2. Follows pre-defined trading rules (called *strategies*).
3. Automatically places, tracks, and closes orders.
4. Has multiple safety layers to prevent catastrophic mistakes.

The whole system is written in **Rust** — a programming language chosen
because it is extremely fast and crashes far less than alternatives. For a
trading bot where milliseconds and correctness matter, this is the right
choice.

> 📚 Want to understand algorithmic trading better?
> - [What is Algo Trading? — Zerodha Varsity](https://zerodha.com/varsity/module/trading-systems/)
> - [Algorithmic Trading (Wikipedia)](https://en.wikipedia.org/wiki/Algorithmic_trading)

---

## How the Stock Exchange Works (Quick Primer)

Before diving into what the bot does, it helps to understand three basic
concepts:

| Term | What it means |
|---|---|
| **Bid price** | The highest price a buyer is currently willing to pay |
| **Ask price** | The lowest price a seller is currently willing to accept |
| **Mid price** | The average of bid and ask — a fair estimate of current value |
| **VWAP** | Volume-Weighted Average Price — the "typical" price of a stock for a session, weighted by how much was traded at each price |
| **Z-score** | A number that tells you how far away the current price is from the average, measured in units of "normal variation" |
| **MIS** | Margin Intraday Square-off — Angel One's intraday product type; all positions are automatically closed at 15:15 if you haven't closed them yourself |

> 📚 Learn more:
> - [What is VWAP? — Investopedia](https://www.investopedia.com/terms/v/vwap.asp)
> - [Intraday Trading Basics — Zerodha Varsity](https://zerodha.com/varsity/chapter/intraday-trading-basics/)
> - [What is a Z-score? (plain English)](https://www.simplypsychology.org/z-score.html)

---

## The Two Strategies

The bot currently runs **two strategies simultaneously**, each targeting
different opportunities.

---

### Strategy 1 — Basis Arbitrage on NIFTY (`strategy_basis_arb`)

**What it trades:** NIFTY50 futures (the June 2026 contract) vs. the NIFTY50
spot index.

**The idea in one sentence:**
> NIFTY futures should trade at a small, predictable premium over the spot
> index. When the gap (called the "basis") becomes unusually large or small,
> bet that it will snap back to normal.

**How it works, step by step:**

1. The bot receives real-time price quotes for both:
   - The *NIFTY50 index* (the spot — what the index is worth right now).
   - The *NIFTY June futures contract* (a promise to buy/sell NIFTY at a fixed
     price on expiry day).

2. It calculates the **basis** = `futures price − spot price`.

3. Over a rolling window of recent ticks it computes:
   - The **average basis** (what's "normal").
   - The **standard deviation** (how much it normally varies).
   - The **z-score** = `(current basis − average) / standard deviation`.

4. Decision rules:
   - z-score > **+2.0** → futures are unusually expensive → **SELL futures**
     (expect the basis to shrink back).
   - z-score < **−2.0** → futures are unusually cheap → **BUY futures**
     (expect the basis to grow back to normal).

5. This is a **carry/spread position** — it is hedged. The profit comes not
   from guessing the direction of the market, but from the price gap
   normalising.

**Product type used:** `CARRYFORWARD` — this position is held overnight if
needed (not auto-squared intraday).

> 📚 Learn more:
> - [What is Futures Basis? — Investopedia](https://www.investopedia.com/terms/b/basis.asp)
> - [NIFTY Futures — NSE India](https://www.nseindia.com/products-services/equity-derivatives-futures-on-index)
> - [Statistical Arbitrage (Wikipedia)](https://en.wikipedia.org/wiki/Statistical_arbitrage)

---

### Strategy 2 — VWAP Mean-Reversion on Equity Stocks (`strategy_intraday_vwap`)

**What it trades:** INFY, HCLTECH, and SUNPHARMA shares on NSE.

**The idea in one sentence:**
> Liquid large-cap stocks tend to drift away from their session average price
> and then drift back. Buy when a stock is unusually cheap relative to the
> day's average; sell short when it is unusually expensive.

**How it works, step by step:**

1. At 9:15 AM IST (NSE market open), the bot starts collecting price quotes
   for all three stocks.

2. For each stock it tracks:
   - **Session mean** — the running average of mid-prices since 9:15 AM
     (a proxy for VWAP).
   - **Rolling std** — the standard deviation of the last 40 price ticks
     (a measure of recent volatility — how much prices are bouncing around).

3. Every time a new quote arrives, it computes:
   ```
   z = (current price − session mean) / rolling std
   ```

4. Decision rules:
   - z < **−2.0** → the stock is trading 2 standard deviations *below* its
     session average → likely oversold → **BUY** (expect a bounce back up).
   - z > **+2.0** → the stock is trading 2 standard deviations *above* its
     session average → likely overbought → **SELL SHORT** (expect a pullback).
   - |z| drops below **0.5** → price has reverted toward the mean → **close
     the position** and take profit.

5. **Signal-strength position sizing:**
   The stronger the signal, the larger the position:
   ```
   base quantity = floor(₹50,000 / current price)
   actual quantity = base × min(|z| / 2.0,  2.0)   ← capped at 2×
   ```
   Example: INFY at ₹1,500 → base = 33 shares.
   If z = −3.0, multiplier = min(3/2, 2) = 1.5 → buy 49 shares.

6. **Hard exit at 14:45 IST:** All open positions are forcibly closed 30
   minutes before Angel One's automatic 15:15 square-off. This avoids:
   - Being closed at a bad market price by the broker.
   - Paying penalty charges for MIS positions held too long.

**Product type used:** `MIS` (Margin Intraday Square-off) — these are
intraday-only trades. No overnight risk.

> 📚 Learn more:
> - [Mean Reversion Trading — Investopedia](https://www.investopedia.com/terms/m/meanreversion.asp)
> - [What is Short Selling? — Zerodha Varsity](https://zerodha.com/varsity/chapter/shorting/)
> - [Standard Deviation in Finance — Investopedia](https://www.investopedia.com/terms/s/standarddeviation.asp)
> - [Position Sizing — Investopedia](https://www.investopedia.com/terms/p/positionsizing.asp)

---

## The Safety Layers

A trading bot handling real money needs multiple independent safeguards. We
built three:

### 1. NSE Risk Engine (`risk_nse`)

Before **every single order** is sent to Angel One, it is checked against
these rules:

| Check | What it prevents |
|---|---|
| **Lot size** | F&O orders must be in multiples of the contract lot size (e.g. NIFTY = 75 units) |
| **Freeze quantity** | NSE bans single orders above a certain size (e.g. 1,800 lots for NIFTY); we reject anything above this |
| **STT trap** | Deep in-the-money options have a punishing tax if exercised; we block those orders |
| **Physical settlement** | Some contracts settle by delivering actual shares on expiry; we block accidental exercise of those |

> 📚 Learn more:
> - [NSE F&O Contract Specifications](https://www.nseindia.com/products-services/equity-derivatives-contract-specification)
> - [STT on Options — Zerodha Varsity](https://zerodha.com/varsity/chapter/intro-to-options/)

### 2. Circuit Breaker (`circuit_breaker`)

This is a **completely separate process** (a different program running in
parallel). Every second, the main trading bot sends a "I am alive" heartbeat.
If the trading bot crashes, hangs, or loses network, the circuit breaker
detects the missing heartbeat and can send a kill signal.

Think of it like a dead-man's switch on a train — if the driver lets go, the
brakes apply automatically.

### 3. Dry-Run Mode

By default, the bot runs in **dry-run mode** — it goes through all the
motions of strategy calculation and order construction, but does NOT actually
send any orders to Angel One. You can watch the logs and see "what would have
happened" without any real money at risk.

To switch to live trading, you must explicitly set:
```
ANGEL_ONE_DRY_RUN=false
```

---

## The Data Flow (What Happens Every Second)

```
Angel One WebSocket
      │
      │  (live price quotes, ~20/sec per stock)
      ▼
AngelOneDataClient   ← decodes the raw binary market data
      │
      ▼
NautilusTrader Engine  ← routes quotes to strategies
      │
      ├──▶  BasisArbStrategy        ← watches NIFTY futures vs spot
      │           │
      │           └──▶ z-score > 2? ──▶ submit order
      │
      └──▶  IntradayVwapStrategy    ← watches INFY, HCLTECH, SUNPHARMA
                  │
                  └──▶ z-score > 2? ──▶ submit order
                                              │
                                              ▼
                                    NSE Risk Engine  ← pre-trade checks
                                              │
                                              ▼
                                    AngelOneExecutionClient
                                              │
                                              ▼
                                    Angel One REST API  ──▶ Exchange
```

---

## The Stocks and Instruments

| Symbol | What it is | Strategy | Product |
|---|---|---|---|
| NIFTY26JUNFUT | NIFTY50 June 2026 futures contract | Basis Arb | CARRYFORWARD |
| NIFTY | NIFTY50 spot index | Basis Arb | MIS |
| INFY | Infosys Ltd shares | VWAP Mean-Reversion | MIS |
| HCLTECH | HCL Technologies Ltd shares | VWAP Mean-Reversion | MIS |
| SUNPHARMA | Sun Pharmaceutical Industries Ltd shares | VWAP Mean-Reversion | MIS |

> ✅ **Tokens verified** from Angel One's ScripMaster (May 2026): INFY=1594, HCLTECH=7229, SUNPHARMA=3351. Re-verify before each expiry cycle as tokens can change.

---

## Capital Allocation

| Strategy | Capital per instrument | Max leverage (MIS) | Notes |
|---|---|---|---|
| Basis Arb | 1 lot per signal | ~1× (F&O) | Size defined in `config/strategy_basis_arb.toml` |
| VWAP Intraday | ₹50,000 per stock | up to 5× (MIS) | 3 stocks → ₹1.5L base deployed |

Total capital at risk (intraday): roughly **₹1.5L base**, up to **₹7.5L gross
exposure** at maximum MIS leverage across all three stocks.

---

## Key Configuration Files

| File | What it controls |
|---|---|
| `config/trading.toml` | Instruments, account ID, venue |
| `config/strategy_basis_arb.toml` | z-score threshold, window size, lot size for basis arb |
| `config/strategy_intraday_vwap.toml` | z-score threshold, capital per stock, exit time, symbols |
| `config/nse_risk.toml` | Per-instrument lot sizes, freeze limits, settlement flags |

---

## How to Run (Phase 0 / Data-Only Mode)

```bash
# 1. Start the database (QuestDB — stores tick data)
docker compose up -d

# 2. Start the market data ingestion (streams live prices to database)
cargo run -p ingestion

# 3. Start the circuit breaker watchdog (separate terminal)
cargo run -p circuit_breaker
```

The full trading node (`cargo run -p trading`) requires Angel One API
credentials loaded as environment variables and `ANGEL_ONE_DRY_RUN=false`.

---

## What "Deterministic Replay" Means (and Why It Matters)

One of the strongest guarantees in this system is **replay determinism**: if
you record all the market data from a live session and play it back, the bot
will produce **exactly the same orders**, down to the nanosecond timestamp.

Why does this matter?

- **Debugging:** If a bad trade happens, you can replay the session and
  inspect exactly what the strategy was "thinking" at every tick.
- **Backtesting:** You can test a strategy against months of historical data
  before risking a single rupee.
- **Auditing:** Every order has a rationale tag embedded (e.g.
  `intraday_vwap|z=-2.41|price=1487.50|mean=1502.30|std=6.12`) so you can
  always see why the bot acted.

This is achieved by:
- Never using the system clock (`std::time::SystemTime::now()`) in any strategy.
- All timestamps come from the tick data itself (injected by NautilusTrader's
  internal clock).
- State is computed by replaying events — not stored as mutable global variables.

> 📚 Learn more:
> - [Event Sourcing (plain English) — Martin Fowler](https://martinfowler.com/eaaDev/EventSourcing.html)
> - [Backtesting — Investopedia](https://www.investopedia.com/terms/b/backtesting.asp)

---

## The Technology Stack

| Component | Technology | Why |
|---|---|---|
| Programming language | **Rust** | Speed + memory safety; no garbage collector pauses |
| Trading engine core | **NautilusTrader** | Production-grade order book, execution FSM, backtest engine — we don't reinvent these |
| Broker connection | **Angel One SmartAPI** | WebSocket for live data, REST API for orders |
| Tick storage (hot) | **QuestDB** | Time-series database optimised for financial tick data |
| Tick storage (cold) | **Apache Parquet** | Compressed columnar files for long-term storage / backtesting |
| IST time arithmetic | **chrono + chrono-tz** | Correct handling of India Standard Time (UTC+5:30) |
| Process isolation | **ZeroMQ** | Circuit-breaker heartbeat across separate processes |

> 📚 Learn more about NautilusTrader:
> - [NautilusTrader official site](https://nautilusrader.io)
> - [NautilusTrader GitHub](https://github.com/nautechsystems/nautilus_trader)

---

## Glossary of Terms Used in This Codebase

| Term | Plain English |
|---|---|
| **Tick** | A single price update from the exchange — like one frame in a movie |
| **Quote tick** | A tick that contains both the best buy (bid) and best sell (ask) price |
| **InstrumentId** | The unique name NautilusTrader uses for a tradeable asset, e.g. `INFY.NSE` |
| **DataClient** | The part that receives live market data from Angel One |
| **ExecutionClient** | The part that sends orders to Angel One |
| **Strategy** | A set of rules that decides when to buy or sell |
| **Actor** | NautilusTrader's name for a strategy that reacts to market events |
| **Order** | An instruction to buy or sell a specific quantity at market price |
| **MIS** | Margin Intraday Square-off — intraday only; broker closes any open positions at 15:15 |
| **CARRYFORWARD** | F&O product type — position can be held overnight |
| **Lot size** | The minimum number of shares/units you must trade in F&O (e.g. 1 lot of NIFTY = 75 units) |
| **Basis** | The difference between futures price and spot price |
| **Z-score** | How many standard deviations away from the average something is |
| **Rolling window** | Only the last N data points are considered; older ones are dropped |
| **Dry run** | The bot calculates and logs what it *would* do, but sends no real orders |

---

## Phase 1 Status: Backtesting Engine (`backtest`)

As part of Milestone 5 (Step 10), we have implemented a **fully-fledged Backtest Engine** to replay recorded market data over our strategies. 

**What has been implemented:**
- A `backtest` crate that sets up NautilusTrader's `BacktestEngine`.
- A custom data ingestion pipeline that reads historical `.parquet` files from the Phase 0 `storage` pipeline.
- Full offline simulation using the exact same strategy binaries (`BasisArbStrategy` and `IntradayVwapStrategy`) and Risk Checks (`NseRiskCheck`) that the live trading bot uses. 
- Fast, bit-deterministic replay of millions of ticks allowing for precise parameter tuning.

**What is expected out of the Backtest Engine:**
- **Tearsheets and JSON Results:** At the end of a backtest run, the engine outputs statistics (total orders, fills, rejects, max drawdown, and overall realized PnL).
- **Strategy Validation:** It proves whether a strategy is profitable on a given historical day. By swapping configurations (e.g., changing the entry z-score from 2.0 to 2.5), we can directly measure the impact on PnL.
- **Latency Profiling:** It validates that the strategy logic executes well within the required nanosecond budget.

