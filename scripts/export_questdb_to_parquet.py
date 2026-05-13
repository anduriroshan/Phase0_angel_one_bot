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
    url = f"http://{QUESTDB_HOST}:{QUESTDB_PORT}/exec?query={urllib.parse.quote(sql)}&limit=10000000"
    with urllib.request.urlopen(url, timeout=60) as r:
        result = json.loads(r.read())
    if "error" in result:
        raise RuntimeError(f"QuestDB error: {result['error']}")
    columns = [col["name"] for col in result["columns"]]
    return columns, result["dataset"]


def export_date(date_str: str, data_dir: str, swap_bid_ask: bool):
    yyyy, mm, dd = date_str.split("-")
    next_dd = f"{int(dd)+1:02d}"

    sql = (
        f"SELECT ts_ns, inst_id, side, price, qty, seq_no, "
        f"best_bid_price, best_bid_qty, best_ask_price, best_ask_qty "
        f"FROM ticks "
        f"WHERE ts_ns >= '{yyyy}-{mm}-{dd}T00:00:00.000000Z' "
        f"AND ts_ns < '{yyyy}-{mm}-{next_dd}T00:00:00.000000Z' "
        f"ORDER BY ts_ns ASC"
    )

    print(f"Querying QuestDB for {date_str} ...")
    columns, dataset = questdb_query(sql)

    if not dataset:
        print("No data returned from QuestDB for this date.")
        sys.exit(1)

    df = pd.DataFrame(dataset, columns=columns)
    print(f"Total rows: {len(df):,}")
    print(f"Instruments (inst_id): {sorted(df['inst_id'].unique())}")

    if swap_bid_ask:
        print("--swap-bid-ask: swapping best_bid_price<->best_ask_price columns")
        df["best_bid_price"], df["best_ask_price"] = df["best_ask_price"].copy(), df["best_bid_price"].copy()
        df["best_bid_qty"],   df["best_ask_qty"]   = df["best_ask_qty"].copy(),   df["best_bid_qty"].copy()

    out_dir = Path(data_dir) / yyyy / mm / dd
    out_dir.mkdir(parents=True, exist_ok=True)

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

        out_path = out_dir / f"{int(inst_id)}.parquet"
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

    print(f"\nDone. Parquet files written to {out_dir}")
    print("Next step: cargo run -p backtest -- --date", date_str)


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
    export_date(args.date, args.data_dir, args.swap_bid_ask)

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


def questdb_query(sql: str) -> list[dict]:
    url = f"http://{QUESTDB_HOST}:{QUESTDB_PORT}/exec?query={urllib.parse.quote(sql)}&limit=10000000"
    with urllib.request.urlopen(url, timeout=60) as r:
        result = json.loads(r.read())
    if "error" in result:
        raise RuntimeError(f"QuestDB error: {result['error']}")
    columns = [col["name"] for col in result["columns"]]
    return [dict(zip(columns, row)) for row in result["dataset"]]


def export_date(date_str: str, data_dir: str):
    yyyy, mm, dd = date_str.split("-")
    # QuestDB stores ts_ns as the designated timestamp column.
    # Filter by date using timestamp range (midnight UTC start/end).
    sql = (
        f"SELECT ts_ns, inst_id, side, price, qty, seq_no, "
        f"best_bid_price, best_bid_qty, best_ask_price, best_ask_qty "
        f"FROM ticks "
        f"WHERE ts_ns >= {yyyy}{mm}{dd}T000000.000000Z "
        f"AND ts_ns < {yyyy}{mm}{int(dd)+1:02d}T000000.000000Z "
        f"ORDER BY ts_ns ASC"
    )

    print(f"Querying QuestDB for {date_str}...")
    try:
        rows = questdb_query(sql)
    except Exception as e:
        # Retry with simpler date cast if server uses different syntax
        sql2 = (
            f"SELECT ts_ns, inst_id, side, price, qty, seq_no, "
            f"best_bid_price, best_bid_qty, best_ask_price, best_ask_qty "
            f"FROM ticks "
            f"WHERE ts_ns >= cast('{date_str}T00:00:00.000000Z' as timestamp) "
            f"AND ts_ns < cast('{yyyy}-{mm}-{int(dd)+1:02d}T00:00:00.000000Z' as timestamp) "
            f"ORDER BY ts_ns ASC"
        )
        print(f"Retrying with alternate syntax...")
        rows = questdb_query(sql2)

    if not rows:
        print("No data returned from QuestDB for this date.")
        sys.exit(1)

    df = pd.DataFrame(rows)
    print(f"Total rows: {len(df)}")
    print(f"Instruments: {sorted(df['inst_id'].unique())}")

    out_dir = Path(data_dir) / yyyy / mm / dd
    out_dir.mkdir(parents=True, exist_ok=True)

    for inst_id, group in df.groupby("inst_id"):
        group = group.sort_values("ts_ns").reset_index(drop=True)

        # Cast to exact dtypes matching Rust schema.
        table = pa.table({
            "ts_ns":          pa.array(group["ts_ns"].astype("int64"),   type=pa.int64()),
            "inst_id":        pa.array(group["inst_id"].astype("int32"), type=pa.int32()),
            "side":           pa.array(group["side"].astype("int16"),    type=pa.int16()),
            "price":          pa.array(group["price"].astype("float64"), type=pa.float64()),
            "qty":            pa.array(group["qty"].astype("int64"),     type=pa.int64()),
            "seq_no":         pa.array(group["seq_no"].astype("int64"),  type=pa.int64()),
            "best_bid_price": pa.array(group["best_bid_price"].astype("float64"), type=pa.float64()),
            "best_bid_qty":   pa.array(group["best_bid_qty"].astype("int64"),     type=pa.int64()),
            "best_ask_price": pa.array(group["best_ask_price"].astype("float64"), type=pa.float64()),
            "best_ask_qty":   pa.array(group["best_ask_qty"].astype("int64"),     type=pa.int64()),
        }, schema=SCHEMA)

        out_path = out_dir / f"{int(inst_id)}.parquet"
        pq.write_table(table, out_path, compression="zstd")

        # Quick sanity check
        prices = group["price"].unique()
        bid_ok = (group["best_bid_price"] > 0).sum()
        ask_ok = (group["best_ask_price"] > 0).sum()
        t0 = pd.to_datetime(group["ts_ns"].min(), unit="ns")
        t1 = pd.to_datetime(group["ts_ns"].max(), unit="ns")
        print(f"  {out_path}: {len(group)} rows | {t0} -> {t1} | "
              f"{len(prices)} unique prices | bid>0:{bid_ok} ask>0:{ask_ok}")

    print(f"\nDone. Files written to {out_dir}")


def export_date(date_str: str, data_dir: str, swap_bid_ask: bool = False):  # noqa: redeclared below
    pass  # replaced by the real function above — see definition above main()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--date", required=True, help="Date in YYYY-MM-DD format")
    parser.add_argument("--data-dir", default="./data/raw", help="Base data directory")
    parser.add_argument(
        "--swap-bid-ask", action="store_true",
        help="Swap best_bid_price<->best_ask_price columns. Use when the server "
             "ran old schema.rs code (before the bid/ask inversion fix) so that "
             "the exported parquet has correct bid<ask ordering."
    )
    args = parser.parse_args()
    export_date(args.date, args.data_dir, swap_bid_ask=args.swap_bid_ask)
