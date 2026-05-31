# research/ — offline analysis pipeline

Python tooling for strategy research over the recorded tick data. Kept entirely
separate from the Rust trading engine: this is the *cold path* where slow analysis
runs and (later) ML models train. Nothing here touches the live order path.

## Why this exists

The 13-day net-of-cost backtest showed the current rule-based strategies have no
edge after STT (see `knowledge`/memory). Rather than curve-fit dead signals, we
hunt for a *real* edge. First idea, motivated by sector co-movement (e.g. INFY vs
HCLTECH): **pairs / statistical arbitrage** — trade the mean-reverting *spread*
between two cointegrated names, which is market-neutral.

## Layout

| File | Purpose |
|---|---|
| `data_loader.py` | Parquet ticks → clean, resampled per-symbol mid-price series |
| `scan_pairs.py` | Correlation + Engle-Granger cointegration scan → ranked candidate pairs |
| `sector_map.json` | Manual symbol → sector map (ScripMaster has no sector field) |
| `requirements.txt` | pandas, pyarrow, numpy, scipy, statsmodels |

## Setup

```powershell
cd research
python -m venv .venv
.\.venv\Scripts\Activate.ps1
pip install -r requirements.txt
```

## Run

```powershell
python scan_pairs.py                    # auto-discovers data/raw/2026/05
python scan_pairs.py --resample 5min    # coarser bars
python scan_pairs.py --dates 2026-05-29
```

Output: a ranked table (also saved to `pairs_scan.csv`) with, per pair:
`mean_corr`, `frac_coint_p05` (fraction of days cointegrated at p<0.05),
`median_p`, `median_beta` (hedge ratio), and whether the two names share a sector.

## How to read it

- **High correlation + high `frac_coint_p05`** = a candidate pairs trade.
- **High correlation but NOT cointegrated** = they move together but the spread
  drifts — *not* tradeable as a pair (the classic trap).
- A candidate is only worth pursuing if the spread's typical swing clears
  **~2× round-trip STT** (a pair is two legs = double cost). Cost-gate before
  getting excited.

## Honest caveats

- Cointegration is properly a multi-month, daily-frequency concept. The per-day
  intraday test here is a **screen**, not proof.
- Today only INFY / HCLTECH / SUNPHARMA have recorded data, over ~9–13 days.
  That is a **tiny** sample — results are illustrative, for validating the
  pipeline, not for trading decisions.
- No transaction costs are applied in the scan itself.

## Extending

1. Widen recording: `python ../scripts/extract_nifty50_tokens.py` then run
   `cargo run -p ingestion` each session for months.
2. Add the new symbols/tokens to `EQUITY_TOKENS` in `data_loader.py` (or wire it
   to read `config/recording.toml`).
3. Re-run the scan over the larger universe and longer history.

## Roadmap (after pairs research)

- Feature engineering from L2 depth (order-book imbalance, microprice, spread).
- A cold-path ML model emitting an edge score consumed by a Rust strategy via a
  `StrategyParamUpdated` event — never in the hot path.
- Walk-forward / out-of-sample validation, always net of cost.
