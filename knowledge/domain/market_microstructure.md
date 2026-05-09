# Market Microstructure

> This document teaches the foundational concepts of market microstructure
> that any agent working on this system must understand.

---

## Core Concepts

### Order Book

The order book is the central data structure of any exchange. It is a prioritized queue of resting orders at various price levels.

**Bid side (buyers):** Orders to buy, sorted highest price first (best bid on top).
**Ask side (sellers):** Orders to sell, sorted lowest price first (best ask on top).

```
        ASK (sellers)
        ₹245.60 × 500
        ₹245.55 × 200  ← best ask (lowest offer)
        ─────────────
        ₹245.50 × 300  ← best bid (highest demand)
        ₹245.45 × 800
        BID (buyers)
```

### Spread

The **bid-ask spread** is the gap between the best bid and best ask:

```
spread = best_ask - best_bid = ₹245.55 - ₹245.50 = ₹0.05
```

The spread represents the cost of immediacy. Tighter spreads = more liquid markets. NIFTY 50 futures typically have a spread of ₹0.05 (1 tick). Illiquid stock options may have spreads of ₹5–₹50.

### Liquidity

Liquidity is the ability to trade a desired quantity without significantly moving the price.

- **Depth:** Total quantity available at each price level.
- **Resilience:** How quickly the book refills after a large trade.
- **Tightness:** How narrow the spread is.

NIFTY 50 and BANKNIFTY are among the most liquid instruments on NSE. Small F&O contracts on individual stocks are far less liquid.

### Matching Engine

The exchange's matching engine processes incoming orders against the book:

1. **Price-time priority (FIFO):** At each price level, the order that arrived first gets filled first.
2. **Aggressive vs. passive:** An incoming buy at ₹245.55 "crosses" the best ask and gets immediately filled (aggressive). A buy at ₹245.45 rests in the book (passive).

### Slippage

The difference between the **expected** fill price and the **actual** fill price.

Sources of slippage:
- **Market impact:** Your order consumes liquidity and moves the price.
- **Latency:** Price moved between signal generation and order arrival.
- **Queue position:** In limit orders, you may not be first in the queue.

**Critical for backtesting:** Backtests that assume fills at the mid-price (or LTP) are lying. Realistic fill simulation must model the spread, queue position, and impact.

---

## Order Types (NSE Context)

| Type | Behavior | Use Case |
|---|---|---|
| **Market** | Fill immediately at best available price | Emergency exits, circuit breaker panic |
| **Limit** | Rest in book at specified price, fill only at that price or better | Passive entry, capturing spread |
| **SL (Stop Loss)** | Becomes market order when trigger price is hit | Risk management, trailing stops |
| **SL-M** | Stop-loss market order | Guaranteed exit (with slippage) |

### Order Lifecycle

```
Created → Submitted → Acknowledged → [Partial Fill]* → Filled | Cancelled | Rejected
```

- **Created:** Strategy generates the order intent.
- **Submitted:** Execution engine sends it to the exchange.
- **Acknowledged:** Exchange confirms receipt (ACK). The order is now in the book.
- **Partial Fill:** Some quantity is matched. Remaining quantity stays in the book.
- **Filled:** Full quantity matched. Order is complete.
- **Cancelled:** Withdrawn before full fill (by us or by exchange).
- **Rejected:** Exchange refused the order (insufficient margin, invalid parameters, etc.).

---

## Maker vs. Taker

- **Maker (passive):** Your order adds liquidity to the book. You wait to be filled. Some exchanges give maker rebates.
- **Taker (aggressive):** Your order removes liquidity from the book. You cross the spread. You pay the full spread cost.

**NSE specifics:** NSE does not have explicit maker/taker fee tiers like US exchanges. But the economic principle still applies — crossing the spread costs money.

---

## Relevant to This System

### What Our Data Contains

In SnapQuote mode (mode 3), each tick from Angel One gives us:

- `last_traded_price` — the most recent match price
- `best_5_buy` / `best_5_sell` — top 5 levels of the order book
- `volume` — cumulative traded volume for the day
- `open_interest` — for F&O instruments

### What Our Data Does NOT Contain

- Individual order IDs (we don't see who is in the queue)
- Trade-by-trade data (we see snapshots, not every trade)
- Order book deltas (we get full snapshots each tick, not incremental updates)

This matters for strategy design. We cannot build strategies that depend on order-flow analysis or queue position. We **can** build strategies based on price levels, spreads, volume, and OI changes.

---

## Key Metrics for NSE (India)

| Parameter | Typical Value |
|---|---|
| NIFTY 50 tick size | ₹0.05 |
| NIFTY spread (liquid hours) | ₹0.05–₹0.10 |
| BANKNIFTY spread (liquid hours) | ₹0.05–₹0.10 |
| Market hours | 09:15–15:30 IST |
| Pre-open session | 09:00–09:08 IST |
| Settlement | T+1 (equities), same day (index F&O) |
| STT (Securities Transaction Tax) | 0.0125% on sell side (options), 0.01% (futures) |
