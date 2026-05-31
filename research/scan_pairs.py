"""Scan recorded data for correlated / cointegrated equity pairs.

Pairs trading needs a SPREAD that mean-reverts (cointegration), not merely two
stocks that move together (correlation). This screen reports both.

Per pair, per day (default 1-minute bars):
  - Pearson correlation of 1-min returns
  - Engle-Granger cointegration p-value on price levels
  - OLS hedge ratio (beta) for the spread  a = alpha + beta * b
Aggregated across days:
  - mean return correlation
  - fraction of days cointegrated at p < 0.05
  - median cointegration p-value
  - median hedge ratio
Ranked by fraction-cointegrated (desc), then median p (asc).

CAVEATS - read before trusting any number:
  * Cointegration is properly a multi-month, daily-frequency concept; per-day
    intraday testing is a heuristic SCREEN, not proof.
  * With only a few days / few stocks the sample is tiny. Results are
    ILLUSTRATIVE. Re-run once months of the wide universe are recorded.
  * No transaction costs here. A "cointegrated" pair is only tradeable if the
    spread's swings exceed ~2x round-trip STT (pairs = two legs = double cost).

Usage:
    python scan_pairs.py                 # auto-discovers data/raw/2026/05
    python scan_pairs.py --resample 5min --month 06
"""
from __future__ import annotations

import argparse
import itertools
import json
import os

import numpy as np
import pandas as pd
import statsmodels.api as sm
from statsmodels.tsa.stattools import coint

from data_loader import EQUITY_TOKENS, discover_dates, load_day_mid

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SECTOR_MAP_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sector_map.json")


def load_sectors():
    try:
        with open(SECTOR_MAP_PATH) as f:
            return json.load(f)
    except Exception:  # noqa: BLE001
        return {}


def pair_day_stats(a: pd.Series, b: pd.Series, min_bars: int):
    """(corr, coint_pvalue, hedge_beta) for one aligned day, or None."""
    df = pd.concat([a.rename("a"), b.rename("b")], axis=1).dropna()
    if len(df) < min_bars:
        return None
    ret = df.pct_change().dropna()
    if len(ret) < 2 or ret["a"].std() == 0 or ret["b"].std() == 0:
        return None
    corr = ret["a"].corr(ret["b"])
    try:
        _, pval, _ = coint(df["a"], df["b"])
    except Exception:  # noqa: BLE001
        return None
    try:
        beta = sm.OLS(df["a"], sm.add_constant(df["b"])).fit().params.iloc[1]
    except Exception:  # noqa: BLE001
        beta = np.nan
    return corr, pval, beta


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--data-dir", default=os.path.join(REPO_ROOT, "data", "raw"))
    ap.add_argument("--year", default="2026")
    ap.add_argument("--month", default="05")
    ap.add_argument("--dates", default="", help="comma-separated YYYY-MM-DD; default: discover the month")
    ap.add_argument("--resample", default="1min")
    ap.add_argument("--min-bars", type=int, default=30)
    ap.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "pairs_scan.csv"))
    args = ap.parse_args()

    dates = [d.strip() for d in args.dates.split(",") if d.strip()] or discover_dates(
        args.data_dir, args.year, args.month
    )
    if not dates:
        print(f"No date folders under {args.data_dir}/{args.year}/{args.month}")
        return

    symbols = list(EQUITY_TOKENS)
    print(f"Symbols : {symbols}")
    print(f"Dates   : {len(dates)} ({dates[0]} .. {dates[-1]})")
    print(f"Bars    : {args.resample}\n")

    # Preload each symbol's per-day resampled series.
    series = {sym: {} for sym in symbols}
    for sym in symbols:
        tok = EQUITY_TOKENS[sym]
        for date in dates:
            s = load_day_mid(tok, date, args.data_dir, args.resample)
            if not s.empty:
                series[sym][date] = s
        print(f"  loaded {sym}: {len(series[sym])} day(s) with data")

    sectors = load_sectors()
    rows = []
    for a_sym, b_sym in itertools.combinations(symbols, 2):
        corrs, pvals, betas = [], [], []
        for date in dates:
            sa, sb = series[a_sym].get(date), series[b_sym].get(date)
            if sa is None or sb is None:
                continue
            st = pair_day_stats(sa, sb, args.min_bars)
            if st is None:
                continue
            corrs.append(st[0])
            pvals.append(st[1])
            betas.append(st[2])
        if not pvals:
            continue
        pv = np.array(pvals)
        rows.append({
            "pair": f"{a_sym}-{b_sym}",
            "sector_a": sectors.get(a_sym, "?"),
            "sector_b": sectors.get(b_sym, "?"),
            "same_sector": bool(a_sym in sectors and sectors.get(a_sym) == sectors.get(b_sym)),
            "days": len(pvals),
            "mean_corr": round(float(np.nanmean(corrs)), 3),
            "frac_coint_p05": round(float((pv < 0.05).mean()), 2),
            "median_p": round(float(np.median(pv)), 3),
            "median_beta": round(float(np.nanmedian(betas)), 3),
        })

    if not rows:
        print("No pairs had enough aligned data to test.")
        return

    out = pd.DataFrame(rows).sort_values(
        ["frac_coint_p05", "median_p"], ascending=[False, True]
    )
    pd.set_option("display.width", 140)
    print("\n" + out.to_string(index=False))
    out.to_csv(args.out, index=False)
    print(f"\nSaved {args.out}")
    print(
        "\nReminder: illustrative screen on a tiny sample, no costs modeled. "
        "Re-run on months of the wide universe before drawing conclusions."
    )


if __name__ == "__main__":
    main()
