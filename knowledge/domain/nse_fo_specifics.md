# NSE F&O Specifics

> Operational rules of the NSE Futures & Options segment that affect any
> strategy or risk decision. These are domain constants that the system
> must respect.

**Status:** Phase 1 reference. Numbers below are subject to NSE/SEBI revision — verify against current circulars before relying on them in code.

---

## Reading Discipline

> ⚠ **Verify before use.** STT rates, lot sizes, freeze quantities, and margin formulas are revised by SEBI / NSE periodically. The values in this document are stated for reference. Code must read these from a config file (or Angel One instrument master), not from this document. This document tells you **what kind of values exist and why they matter** — not the current month's exact numbers.

---

## Trading Sessions

NSE equity & F&O market timings (IST):

| Phase | Time | Notes |
|---|---|---|
| Pre-open call auction | 09:00 – 09:08 | Equity cash only; F&O does not pre-open. Random close 09:07–09:08. |
| Pre-open match + buffer | 09:08 – 09:15 | Orders queued from prior session matched. |
| **Continuous trading** | **09:15 – 15:30** | F&O active. |
| Closing session (cash) | 15:40 – 16:00 | Equity only. F&O does not have post-close. |
| Settlement | After hours | T+1 for equity (since 2023); T+0 same-day for index F&O P&L; T+1 for stock F&O. |

The strategy engine treats events outside continuous trading as **non-tradable**: it can compute features, but `risk_engine` rejects orders submitted outside 09:15–15:30 IST. The injected `Clock` provides the time; conversion to IST happens at the boundary (`Asia/Kolkata` timezone in `chrono-tz`).

**Special days:**
- **Muhurat trading** (Diwali): one-hour evening session; opt out by default — strategy must explicitly opt in.
- **Half-days** (Budget day, special trading sessions): NSE publishes circulars; the trading-calendar service must respect them.
- **Weekly expiries**: every Thursday for index options (NIFTY, BANKNIFTY, FINNIFTY have varying weekly cadences — verify current schedule).
- **Monthly expiries**: last Thursday of the contract month.

---

## Instruments and Tokens

Angel One distributes an instrument master file containing all tradable contracts with their tokens. Phase 0 hardcodes a few index tokens; Phase 1 must consume the master file.

```
Index (NSE_CM, exchange_type=1):
  26009  NIFTY 50 (index, not tradable directly)
  26000  NIFTY BANK
  26037  NIFTY FIN SERVICE (FINNIFTY underlying)

F&O (NSE_FO, exchange_type=2):
  Each option/future contract has its own token.
  Token format is opaque; strategies look up by (symbol, expiry, strike, type).
```

The instrument master must be downloaded daily (typically before market open) because contracts come and go (new strikes added, old contracts expire).

**Lookup pattern:**
```rust
fn find_option_token(
    master: &InstrumentMaster,
    symbol: &str,        // "NIFTY"
    expiry: NaiveDate,   // 2026-05-15
    strike_paise: i64,   // 2200000 = ₹22,000.00
    opt_type: OptionType,// CE | PE
) -> Option<i32>;
```

---

## Lot Sizes

F&O on NSE is traded in lots, not in shares. Lot sizes are revised periodically (typically every 6 months) by SEBI when the contract value drifts significantly.

```
Indicative — verify against current Angel One instrument master:

  NIFTY index options:    lot size = 25 (was 50 before Apr-2024)
  BANKNIFTY index options: lot size = 15
  FINNIFTY index options:  lot size = 25
  Stock F&O:              varies by stock (e.g., RELIANCE = 250 shares/lot)
```

**Implication for `Tick.qty` and `OrderEvent.qty`**: in the system, `qty` is **shares (or units)**, not **lots**. The conversion happens at the strategy/UI boundary. Internal math is always in shares to avoid the "lots vs shares" bug class. This is a glossary-locked decision; see [glossary.md](../glossary.md) `qty` semantics (add an entry if not present).

---

## Freeze Quantity

NSE rejects single orders larger than a per-instrument **freeze quantity** to prevent fat-finger errors.

```
Indicative — verify against current NSE circular:

  NIFTY:      ~1,800 lots per order (= 45,000 shares at lot 25)
  BANKNIFTY:  ~900 lots per order
  Stock F&O:  varies
```

The execution engine **must** split orders larger than the freeze quantity into multiple orders before submission. A single order over the freeze quantity is rejected with `EXCH_ORDER_QUANTITY_FREEZE`. Pre-trade risk check enforces a hard cap below the freeze quantity by default (e.g., 80% of freeze).

---

## Tick Size & Price Precision

| Instrument | Tick size |
|---|---|
| NIFTY / BANKNIFTY index options | ₹0.05 |
| Stock options | ₹0.05 (most names) |
| Stock futures | ₹0.05 |

**Internal representation**: paise (integer). ₹0.05 = 5 paise. All limit prices submitted to the broker must be aligned to the tick size; a non-aligned price is rejected. The execution engine rounds to the nearest tick before submission.

```rust
fn align_to_tick(price_paise: i64, tick_paise: i64) -> i64 {
    (price_paise / tick_paise) * tick_paise
}
```

---

## STT (Securities Transaction Tax)

STT is government tax, levied on a per-transaction basis, paid in addition to broker fees. It materially affects strategy economics — a high-frequency strategy can pay more in STT than it earns.

```
Indicative — verify against current SEBI / income tax circulars:

  Equity intraday (cash market):  0.025% on sell side
  Equity delivery:                0.1% on both sides
  Equity futures:                 0.0125% on sell side
  Equity options (premium):       0.0625% on sell side of premium
  Equity options (exercise):      0.125% on settlement value (the "STT trap")
```

### The STT Trap on Option Exercise

If you hold a long option to expiry and it expires **in-the-money**, NSE auto-exercises it and charges STT on the **settlement value** (intrinsic × lot size), not on the premium. This is **20× higher** than the STT on closing the position via sale.

```
Example: Long NIFTY 22000 CE, premium ₹50, expiry settlement at ₹22150
  Intrinsic = ₹150 × 25 (lot) = ₹3750
  STT on exercise (0.125% of ₹3750) = ₹4.69
  STT on sale (0.0625% of ₹50 × 25 = ₹1250) = ₹0.78
  Trap = ~6× higher
  
For deep ITM options, the multiplier becomes much worse.
```

**Risk engine rule**: any open long option position with > X minutes to expiry, where intrinsic exceeds threshold, must trigger a "close before expiry" alert. The system never holds long options to settlement unless explicitly configured (with a comment justifying why).

---

## Margins

NSE F&O margin is computed by SPAN (CME risk model) + Exposure margin. The exact numbers vary by instrument, volatility, and SEBI circulars.

```
Indicative — verify against current Angel One margin calculator:

  NIFTY futures (1 lot ≈ ₹5.5 lakh notional): SPAN+Exp ≈ ₹70,000–80,000 (~13–15% of notional)
  Stock futures: typically 18–25% of notional
  Long options: full premium
  Short options: SPAN-based, similar to futures
```

**Pre-trade risk** must compute the projected margin requirement before submission. Angel One exposes a `getMargin` REST endpoint for this. A signal that would breach available margin is rejected at the risk gate, before reaching the execution engine.

**Intraday vs overnight**:
- Intraday positions get reduced margin (~50% of full SPAN+Exp).
- Carrying overnight requires full SPAN+Exp.
- Strategies should declare `intraday_only: bool` in their parameters; the risk engine uses this to choose the margin formula.

---

## Circuit Limits

Every contract has upper and lower price bands per session (typically 10% / 20% / 1.5x ATM bands depending on contract). Orders outside the band are rejected.

The SnapQuote packet exposes `upper_circuit` and `lower_circuit` (paise). Strategies should clamp limit prices to within the band and warn if they're emitting orders close to the band.

---

## Holidays

NSE publishes a yearly trading-holiday list. The trading-calendar service consumes this. The system **must** treat holidays as non-trading days regardless of clock state.

Indicative recurring holidays: Republic Day (Jan 26), Independence Day (Aug 15), Mahatma Gandhi Jayanti (Oct 2), Diwali (one trading day off), Christmas (Dec 25). Plus religious / regional holidays that vary year-to-year.

Implementation: a Parquet or JSON holiday calendar in `./data/calendar/holidays.json`, refreshed at least monthly. Risk engine rejects orders on holidays.

---

## Settlement

| Segment | Settlement | Timing |
|---|---|---|
| Equity cash | T+1 | Funds/securities credited on next working day |
| Index futures (NIFTY etc.) | T+1 cash settled | Daily MTM via cash; final on expiry |
| Index options (NIFTY etc.) | T+1 cash settled | At expiry: intrinsic value × lot × multiplier |
| Stock futures | T+1 physical or cash (post-Oct 2019 most are physical) | At expiry: physical delivery if held |
| Stock options ITM at expiry | Physical delivery | The single largest gotcha — see below |

### Physical Settlement of Stock Options

Since SEBI mandated physical settlement (Oct 2019), stock option positions held to expiry that are ITM result in **delivery of the underlying shares** (long call → buy shares; short call → sell shares; etc.). This requires the actual margin and shares to be available.

**Risk engine rule (Phase 1)**: stock options must be flat by expiry day or the position is force-closed by 13:30 IST on expiry day. Index options (cash-settled) don't have this issue.

---

## Symbol Naming Convention

NSE uses a canonical naming scheme. The Angel One instrument master uses a similar but not identical format. Verify on first use.

```
NIFTY26MAY24FUT     → NIFTY May-2024 Future
NIFTY26MAY2422000CE → NIFTY May-2024 22000 Call
RELIANCE26MAY24FUT  → Reliance May-2024 Future
```

Strategies build symbols deterministically from `(underlying, expiry, strike, opt_type)`. Hand-typed symbols are forbidden — they're a fat-finger source.

---

## Verification Sources (defer to live data)

For accurate current values, the system consults (in order of authority):

1. **NSE official website** — `nseindia.com/products-services/equity-derivatives-trading-info` for lot sizes, freeze qty, expiry calendar
2. **SEBI circulars** — for STT, margin policy changes
3. **Angel One instrument master** — daily download for tradable instruments
4. **Angel One margin calculator API** — for pre-trade margin estimation

Hardcoded constants in this document are **never** authoritative. The system must read live values.

---

## What Phase 0 Code Already Knows

Phase 0 ingests SnapQuote which contains `upper_circuit`, `lower_circuit`, `52_week_high`, `52_week_low`, and OI for F&O instruments. These are already correct in `common::ParsedPacket`. Phase 1 should expose them through the `OrderBook` API for risk and strategy use.

---

## See Also

- [domain/market_microstructure.md](market_microstructure.md) — order book theory, including the existing NSE table at the bottom
- [domain/exchange_protocols.md](exchange_protocols.md) — Angel One wire protocol (already covered)
- [runtime/risk_engine.md](../runtime/risk_engine.md) — pre-trade checks consuming these constants
- [runtime/execution_engine.md](../runtime/execution_engine.md) — order splitting, tick alignment
- [adr/ADR-006-execution-engine-isolation.md](../adr/ADR-006-execution-engine-isolation.md) — order lifecycle isolation

**Last verified against commit:** _NA — this document is reference material, not code-mirroring_
