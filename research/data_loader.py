"""Load recorded Parquet tick data into aligned, resampled mid-price series.

The Phase 0 recorder writes one Parquet file per instrument per flush to
    data/raw/YYYY/MM/DD/{token}_{flush_ms}.parquet
with columns: ts_ns, inst_id, side, price, qty, seq_no,
              best_bid_price, best_bid_qty, best_ask_price, best_ask_qty
              (+ L2 levels 2..5 for data recorded after the L2 upgrade).

This module turns those raw ticks into clean per-symbol mid-price Series on a
fixed time grid, suitable for correlation / cointegration research.
"""
from __future__ import annotations

import glob
import os
from datetime import time as dtime

import numpy as np
import pandas as pd

IST = "Asia/Kolkata"
SESSION_START = dtime(9, 15)
SESSION_END = dtime(15, 30)

# Angel One token -> symbol. Multiple tokens may map to one symbol where a
# contract rolled or a token was corrected (see NEXT_STEPS). Extend as the
# recorded universe grows (or generate from config/recording.toml).
DEFAULT_TOKEN_MAP = {
    1594: "INFY",
    7229: "HCLTECH",
    3351: "SUNPHARMA",
    26000: "NIFTY",
    26009: "NIFTY",      # spot token corrected mid-May
    66071: "NIFTYFUT",
    57515: "NIFTYFUT",   # future token rolled
}

# Equity universe to scan for pairs (symbol -> representative token).
# Today only these three have recorded data; add more once the wide recorder
# (config/recording.toml) has run for a while.
EQUITY_TOKENS = {
    "INFY": 1594,
    "HCLTECH": 7229,
    "SUNPHARMA": 3351,
}

_COLS = ["ts_ns", "price", "best_bid_price", "best_ask_price"]


def _read_parquet_files(paths):
    frames = []
    for p in paths:
        try:
            frames.append(pd.read_parquet(p, columns=_COLS))
        except Exception as e:  # noqa: BLE001
            print(f"  warn: could not read {p}: {e}")
    if not frames:
        return pd.DataFrame(columns=_COLS)
    return pd.concat(frames, ignore_index=True)


def _mid(df):
    """Mid price with graceful fallback: (bid+ask)/2, else one side, else LTP."""
    bid = df["best_bid_price"].to_numpy(dtype="float64")
    ask = df["best_ask_price"].to_numpy(dtype="float64")
    ltp = df["price"].to_numpy(dtype="float64")
    mid = np.where(
        (bid > 0) & (ask > 0),
        (bid + ask) / 2.0,
        np.where(bid > 0, bid, np.where(ask > 0, ask, np.where(ltp > 0, ltp, np.nan))),
    )
    return pd.Series(mid, index=df.index)


def load_day_mid(token, date, data_dir="data/raw", resample="1min"):
    """Resampled mid-price Series (IST tz) for one token on one day.

    Returns an empty Series if no data exists for that token/day.
    """
    y, m, d = date.split("-")
    folder = os.path.join(data_dir, y, m, d)
    paths = sorted(
        glob.glob(os.path.join(folder, f"{token}_*.parquet"))
        + glob.glob(os.path.join(folder, f"{token}.parquet"))
    )
    df = _read_parquet_files(paths)
    if df.empty:
        return pd.Series(dtype="float64")
    df = df.sort_values("ts_ns")
    idx = pd.to_datetime(df["ts_ns"], unit="ns", utc=True).dt.tz_convert(IST)
    s = pd.Series(_mid(df).to_numpy(), index=idx).dropna()
    if s.empty:
        return s
    mask = (s.index.time >= SESSION_START) & (s.index.time <= SESSION_END)
    s = s[mask]
    if s.empty:
        return s
    return s.resample(resample).last().ffill().dropna()


def discover_dates(data_dir, year, month):
    """All YYYY-MM-DD date folders present under data_dir/year/month."""
    base = os.path.join(data_dir, str(year), f"{int(month):02d}")
    if not os.path.isdir(base):
        return []
    days = [d for d in os.listdir(base) if os.path.isdir(os.path.join(base, d))]
    return sorted(f"{year}-{int(month):02d}-{int(d):02d}" for d in days)
