#!/usr/bin/env python3
"""
Export a trading day's tick data from QuestDB to Parquet files.

Run this ON THE SERVER where QuestDB is running:
    python scripts/export_questdb_to_parquet.py --date 2026-05-13

If the server was running old code (before the bid/ask swap fix), add --swap-bid-ask:
    python scripts/export_questdb_to_parquet.py --date 2026-05-13 --swap-bid-ask

Produces files matching the Rust parquet_sink schema:
    data/raw/YYYY/MM/DD/{inst_id}.parquet

Dependencies:  pip install pandas pyarrow
"""

import argparse
import os
import sys
import urllib.parse
import urllib.request
import json
import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq
from pathlib import Path

QUESTDB_HOST = os.environ.get("QUESTDB_HOST", "localhost")
QUESTDB_PORT = os.environ.get("QUESTDB_PORT", "9000")

# Must match storage/src/parquet_sink.rs schema exactly.
SCHEMA = pa.schema([
    pa.field("ts_ns",          pa.int64(),   nullable=False),
    pa.field("inst_id",        pa.int32(),   nullable=False),
    pa.field("side",           pa.int16(),   nullable=False),
    pa.field("price",          pa.float64(), nullable=False),
    pa.field("qty",            pa.int64(),   nullable=False),
    pa.field("seq_no",         pa.int64(),   nullable=False),
    pa.field("best_bid_price", pa.float64(), nullable=False),
    pa.field("best_bid_qty",   pa.int64(),   nullable=False),
    pa.field("best_ask_price", pa.float64(), nullable=False),
    pa.field("best_ask_qty",   pa.int64(),   nullable=False),
])


def questdb_query(sql: str) -> tuple:
    """Execute a SQL query against QuestDB and return (columns, dataset)."""
    url = f"http://{QUESTDB_HOST}:{QUESTDB_PORT}/exec?query={urllib.parse.quote(sql)}&limit=10000000"
    try:
        with urllib.request.urlopen(url, timeout=120) as r:
            result = json.loads(r.read())
            if "error" in result:
                raise RuntimeError(f"QuestDB error: {result['error']}")
            columns = [col["name"] for col in result["columns"]]
            return columns, result["dataset"]
    except urllib.error.HTTPError as e:
        error_msg = e.read().decode("utf-8")
        try:
            error_json = json.loads(error_msg)
            msg = error_json.get("error", error_msg)
        except Exception:
            msg = error_msg
        print(f"QuestDB HTTP Error: {msg}")
        print(f"SQL was: {sql}")
        raise RuntimeError(f"QuestDB query failed: {msg}")


def export_date(date_str: str, data_dir: str, swap_bid_ask: bool):
    yyyy, mm, dd = date_str.split("-")

    # 'timestamp' is the designated column name. We cast it to long (micros) 
    # and multiply by 1000 to get nanoseconds directly.
    sql = (
        f"SELECT "
        f"cast(\"timestamp\" as long) * 1000 as ts_ns, "
        f"inst_id, side, price, qty, seq_no, "
        f"best_bid_price, best_bid_qty, best_ask_price, best_ask_qty "
        f"FROM ticks "
        f"WHERE \"timestamp\" IN '{date_str}' "
        f"ORDER BY \"timestamp\" ASC"
    )

    print(f"Querying QuestDB for {date_str} ...")
    columns, dataset = questdb_query(sql)

    if not dataset:
        print(f"No data returned from QuestDB for date {date_str}.")
        sys.exit(1)

    df = pd.DataFrame(dataset, columns=columns)
    
    # Ensure ts_ns is int64
    df["ts_ns"] = df["ts_ns"].astype("int64")
    
    print(f"Total rows: {len(df):,}")
    print(f"Instruments (inst_id): {sorted(df['inst_id'].unique())}")

    if swap_bid_ask:
        print("--swap-bid-ask: swapping best_bid_price<->best_ask_price columns")
        df["best_bid_price"], df["best_ask_price"] = df["best_ask_price"].copy(), df["best_bid_price"].copy()
        df["best_bid_qty"],   df["best_ask_qty"]   = df["best_ask_qty"].copy(),   df["best_bid_qty"].copy()

    out_root = Path(data_dir) / yyyy / mm / dd
    out_root.mkdir(parents=True, exist_ok=True)

    for inst_id, group in df.groupby("inst_id"):
        group = group.sort_values("ts_ns").reset_index(drop=True)

        table = pa.table({
            "ts_ns":          pa.array(group["ts_ns"].astype("int64"),           type=pa.int64()),
            "inst_id":        pa.array(group["inst_id"].astype("int32"),          type=pa.int32()),
            "side":           pa.array(group["side"].astype("int16"),             type=pa.int16()),
            "price":          pa.array(group["price"].astype("float64"),          type=pa.float64()),
            "qty":            pa.array(group["qty"].astype("int64"),              type=pa.int64()),
            "seq_no":         pa.array(group["seq_no"].astype("int64"),           type=pa.int64()),
            "best_bid_price": pa.array(group["best_bid_price"].astype("float64"), type=pa.float64()),
            "best_bid_qty":   pa.array(group["best_bid_qty"].astype("int64"),     type=pa.int64()),
            "best_ask_price": pa.array(group["best_ask_price"].astype("float64"), type=pa.float64()),
            "best_ask_qty":   pa.array(group["best_ask_qty"].astype("int64"),     type=pa.int64()),
        }, schema=SCHEMA)

        out_path = out_root / f"{int(inst_id)}.parquet"
        pq.write_table(table, out_path, compression="zstd")

        prices  = group["price"].unique()
        bid_ok  = (group["best_bid_price"] > 0).sum()
        ask_ok  = (group["best_ask_price"] > 0).sum()
        both_ok = ((group["best_bid_price"] > 0) & (group["best_ask_price"] > 0)).sum()
        t0 = pd.to_datetime(group["ts_ns"].min(), unit="ns")
        t1 = pd.to_datetime(group["ts_ns"].max(), unit="ns")
        warn = ("  <-- WARNING: bid always 0" if bid_ok == 0 else
                "  <-- WARNING: ask always 0" if ask_ok == 0 else "")
        print(f"  {out_path.name}: {len(group):>6} rows | {t0} -> {t1} | "
              f"{len(prices)} unique prices | bid>0:{bid_ok} ask>0:{ask_ok} both>0:{both_ok}{warn}")

    print(f"\nDone. Parquet files written to {out_root}")
    print(f"Next step: cargo run -p backtest -- --date {date_str}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--date",         required=True, help="Date in YYYY-MM-DD format")
    parser.add_argument("--data-dir",     default="./data/raw", help="Base data directory (default: ./data/raw)")
    parser.add_argument("--swap-bid-ask", action="store_true",
                        help="Swap bid/ask columns — use when server ran old code before the inversion fix")
    args = parser.parse_args()
    
    try:
        export_date(args.date, args.data_dir, args.swap_bid_ask)
    except Exception as e:
        print(f"FAILED: {e}")
        sys.exit(1)
