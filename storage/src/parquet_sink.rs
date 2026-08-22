//! # Parquet Cold Sink
//!
//! Accumulates tick data in memory as Arrow record batches and periodically
//! flushes them to compressed Parquet files on disk.
//!
//! Output path: `./data/raw/YYYY/MM/DD/{inst_id}.parquet`
//! Compression: Zstd (level 3)
//! Flush triggers: every 60 minutes OR when buffer reaches 500,000 rows.

use arrow::array::{Float64Array, Int16Array, Int32Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::{Datelike, Utc};
use common::Tick;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, info};

/// Maximum rows before triggering a flush.
const MAX_BUFFER_ROWS: usize = 500_000;

/// Flush interval in minutes.
const FLUSH_INTERVAL_MINS: u64 = 60;

/// In-memory buffer that accumulates ticks and writes Parquet files.
pub struct ParquetSink {
    /// Ticks grouped by instrument ID for per-instrument Parquet files.
    buffers: HashMap<i32, Vec<Tick>>,
    /// Total rows across all instrument buffers.
    total_rows: usize,
    /// Timestamp of the last flush.
    last_flush: tokio::time::Instant,
    /// Base output directory.
    base_dir: PathBuf,
    /// Arrow schema for the tick table.
    schema: Arc<Schema>,
}

impl ParquetSink {
    /// Create a new Parquet sink with the given base directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        let schema = Arc::new(Schema::new(vec![
            Field::new("ts_ns", DataType::Int64, false),
            Field::new("inst_id", DataType::Int32, false),
            Field::new("side", DataType::Int16, false),
            Field::new("price", DataType::Float64, false),
            Field::new("qty", DataType::Int64, false),
            Field::new("seq_no", DataType::Int64, false),
            Field::new("best_bid_price", DataType::Float64, false),
            Field::new("best_bid_qty", DataType::Int64, false),
            Field::new("best_ask_price", DataType::Float64, false),
            Field::new("best_ask_qty", DataType::Int64, false),
            // L2 depth levels 2..5 — appended AFTER the L1 columns so the
            // backtest's index-based reader and pre-existing L1 parquet files
            // keep working unchanged.
            Field::new("bid_price_2", DataType::Float64, false),
            Field::new("bid_qty_2", DataType::Int64, false),
            Field::new("bid_price_3", DataType::Float64, false),
            Field::new("bid_qty_3", DataType::Int64, false),
            Field::new("bid_price_4", DataType::Float64, false),
            Field::new("bid_qty_4", DataType::Int64, false),
            Field::new("bid_price_5", DataType::Float64, false),
            Field::new("bid_qty_5", DataType::Int64, false),
            Field::new("ask_price_2", DataType::Float64, false),
            Field::new("ask_qty_2", DataType::Int64, false),
            Field::new("ask_price_3", DataType::Float64, false),
            Field::new("ask_qty_3", DataType::Int64, false),
            Field::new("ask_price_4", DataType::Float64, false),
            Field::new("ask_qty_4", DataType::Int64, false),
            Field::new("ask_price_5", DataType::Float64, false),
            Field::new("ask_qty_5", DataType::Int64, false),
            // --- Everything below is appended AFTER the original 26 columns
            // (indices 0..25) so existing readers (e.g. backtest's index-based
            // loader) and pre-existing Parquet files keep working unchanged. ---
            Field::new("ts_recv_ns", DataType::Int64, false),
            Field::new("exchange_type", DataType::Int16, false),
            Field::new("volume", DataType::Int64, false),
            Field::new("avg_traded_price", DataType::Float64, false),
            Field::new("total_buy_qty", DataType::Float64, false),
            Field::new("total_sell_qty", DataType::Float64, false),
            Field::new("open", DataType::Float64, false),
            Field::new("high", DataType::Float64, false),
            Field::new("low", DataType::Float64, false),
            Field::new("close", DataType::Float64, false),
            Field::new("last_trade_ts_ns", DataType::Int64, false),
            Field::new("open_interest", DataType::Int64, false),
            Field::new("oi_change_pct_raw", DataType::Int64, false),
            Field::new("upper_circuit", DataType::Float64, false),
            Field::new("lower_circuit", DataType::Float64, false),
            Field::new("week_52_high", DataType::Float64, false),
            Field::new("week_52_low", DataType::Float64, false),
            Field::new("best_bid_num_orders", DataType::Int32, false),
            Field::new("best_ask_num_orders", DataType::Int32, false),
            Field::new("bid_num_orders_2", DataType::Int32, false),
            Field::new("bid_num_orders_3", DataType::Int32, false),
            Field::new("bid_num_orders_4", DataType::Int32, false),
            Field::new("bid_num_orders_5", DataType::Int32, false),
            Field::new("ask_num_orders_2", DataType::Int32, false),
            Field::new("ask_num_orders_3", DataType::Int32, false),
            Field::new("ask_num_orders_4", DataType::Int32, false),
            Field::new("ask_num_orders_5", DataType::Int32, false),
        ]));

        Self {
            buffers: HashMap::new(),
            total_rows: 0,
            last_flush: tokio::time::Instant::now(),
            base_dir: base_dir.into(),
            schema,
        }
    }

    /// Push a tick into the in-memory buffer.
    ///
    /// Returns `true` if the buffer was flushed (hit threshold).
    pub fn push(&mut self, tick: &Tick) -> Result<bool, Box<dyn std::error::Error>> {
        self.buffers
            .entry(tick.inst_id)
            .or_default()
            .push(tick.clone());
        self.total_rows += 1;

        if self.total_rows >= MAX_BUFFER_ROWS {
            self.flush_all()?;
            return Ok(true);
        }

        Ok(false)
    }

    /// Check if a time-based flush is due.
    pub fn should_time_flush(&self) -> bool {
        self.last_flush.elapsed()
            >= tokio::time::Duration::from_secs(FLUSH_INTERVAL_MINS * 60)
    }

    /// Flush all buffered data to Parquet files.
    pub fn flush_all(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.total_rows == 0 {
            return Ok(());
        }

        let now = Utc::now();
        let date_dir = self.base_dir.join(format!(
            "{}/{:02}/{:02}",
            now.format("%Y"),
            now.month(),
            now.day()
        ));

        // Collect first to release the mutable borrow on self.buffers
        let entries: Vec<(i32, Vec<Tick>)> = self.buffers.drain().collect();

        // Write each instrument buffer to its own Parquet file
        for (inst_id, ticks) in entries {
            if ticks.is_empty() {
                continue;
            }

            let dir = date_dir.clone();
            fs::create_dir_all(&dir)?;

            // Include the flush timestamp in the filename so successive
            // flushes produce separate files instead of overwriting each other.
            // e.g. 1594_1778668109000.parquet
            let filename = format!("{}_{}.parquet", inst_id, now.timestamp_millis());
            let path = dir.join(&filename);

            self.write_parquet(&path, &ticks)?;
            info!(
                "Wrote {} ticks for inst_id={inst_id} to {}",
                ticks.len(),
                path.display()
            );
        }

        debug!("Flushed {} total rows to Parquet", self.total_rows);
        self.total_rows = 0;
        self.last_flush = tokio::time::Instant::now();

        Ok(())
    }

    /// Write a batch of ticks to a single Parquet file.
    fn write_parquet(
        &self,
        path: &PathBuf,
        ticks: &[Tick],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ts_ns: Vec<i64> = ticks.iter().map(|t| t.ts_ns).collect();
        let inst_id: Vec<i32> = ticks.iter().map(|t| t.inst_id).collect();
        let side: Vec<i16> = ticks.iter().map(|t| t.side as i16).collect();
        let price: Vec<f64> = ticks.iter().map(|t| t.price).collect();
        let qty: Vec<i64> = ticks.iter().map(|t| t.qty).collect();
        let seq_no: Vec<i64> = ticks.iter().map(|t| t.seq_no).collect();
        let best_bid_price: Vec<f64> = ticks.iter().map(|t| t.best_bid_price).collect();
        let best_bid_qty: Vec<i64> = ticks.iter().map(|t| t.best_bid_qty).collect();
        let best_ask_price: Vec<f64> = ticks.iter().map(|t| t.best_ask_price).collect();
        let best_ask_qty: Vec<i64> = ticks.iter().map(|t| t.best_ask_qty).collect();

        // L2 depth levels 2..5.
        let bid_price_2: Vec<f64> = ticks.iter().map(|t| t.bid_price_2).collect();
        let bid_qty_2: Vec<i64> = ticks.iter().map(|t| t.bid_qty_2).collect();
        let bid_price_3: Vec<f64> = ticks.iter().map(|t| t.bid_price_3).collect();
        let bid_qty_3: Vec<i64> = ticks.iter().map(|t| t.bid_qty_3).collect();
        let bid_price_4: Vec<f64> = ticks.iter().map(|t| t.bid_price_4).collect();
        let bid_qty_4: Vec<i64> = ticks.iter().map(|t| t.bid_qty_4).collect();
        let bid_price_5: Vec<f64> = ticks.iter().map(|t| t.bid_price_5).collect();
        let bid_qty_5: Vec<i64> = ticks.iter().map(|t| t.bid_qty_5).collect();
        let ask_price_2: Vec<f64> = ticks.iter().map(|t| t.ask_price_2).collect();
        let ask_qty_2: Vec<i64> = ticks.iter().map(|t| t.ask_qty_2).collect();
        let ask_price_3: Vec<f64> = ticks.iter().map(|t| t.ask_price_3).collect();
        let ask_qty_3: Vec<i64> = ticks.iter().map(|t| t.ask_qty_3).collect();
        let ask_price_4: Vec<f64> = ticks.iter().map(|t| t.ask_price_4).collect();
        let ask_qty_4: Vec<i64> = ticks.iter().map(|t| t.ask_qty_4).collect();
        let ask_price_5: Vec<f64> = ticks.iter().map(|t| t.ask_price_5).collect();
        let ask_qty_5: Vec<i64> = ticks.iter().map(|t| t.ask_qty_5).collect();

        let ts_recv_ns: Vec<i64> = ticks.iter().map(|t| t.ts_recv_ns).collect();
        let exchange_type: Vec<i16> = ticks.iter().map(|t| t.exchange_type).collect();
        let volume: Vec<i64> = ticks.iter().map(|t| t.volume).collect();
        let avg_traded_price: Vec<f64> = ticks.iter().map(|t| t.avg_traded_price).collect();
        let total_buy_qty: Vec<f64> = ticks.iter().map(|t| t.total_buy_qty).collect();
        let total_sell_qty: Vec<f64> = ticks.iter().map(|t| t.total_sell_qty).collect();
        let open: Vec<f64> = ticks.iter().map(|t| t.open).collect();
        let high: Vec<f64> = ticks.iter().map(|t| t.high).collect();
        let low: Vec<f64> = ticks.iter().map(|t| t.low).collect();
        let close: Vec<f64> = ticks.iter().map(|t| t.close).collect();
        let last_trade_ts_ns: Vec<i64> = ticks.iter().map(|t| t.last_trade_ts_ns).collect();
        let open_interest: Vec<i64> = ticks.iter().map(|t| t.open_interest).collect();
        let oi_change_pct_raw: Vec<i64> = ticks.iter().map(|t| t.oi_change_pct_raw).collect();
        let upper_circuit: Vec<f64> = ticks.iter().map(|t| t.upper_circuit).collect();
        let lower_circuit: Vec<f64> = ticks.iter().map(|t| t.lower_circuit).collect();
        let week_52_high: Vec<f64> = ticks.iter().map(|t| t.week_52_high).collect();
        let week_52_low: Vec<f64> = ticks.iter().map(|t| t.week_52_low).collect();
        let best_bid_num_orders: Vec<i32> = ticks.iter().map(|t| t.best_bid_num_orders).collect();
        let best_ask_num_orders: Vec<i32> = ticks.iter().map(|t| t.best_ask_num_orders).collect();
        let bid_num_orders_2: Vec<i32> = ticks.iter().map(|t| t.bid_num_orders_2).collect();
        let bid_num_orders_3: Vec<i32> = ticks.iter().map(|t| t.bid_num_orders_3).collect();
        let bid_num_orders_4: Vec<i32> = ticks.iter().map(|t| t.bid_num_orders_4).collect();
        let bid_num_orders_5: Vec<i32> = ticks.iter().map(|t| t.bid_num_orders_5).collect();
        let ask_num_orders_2: Vec<i32> = ticks.iter().map(|t| t.ask_num_orders_2).collect();
        let ask_num_orders_3: Vec<i32> = ticks.iter().map(|t| t.ask_num_orders_3).collect();
        let ask_num_orders_4: Vec<i32> = ticks.iter().map(|t| t.ask_num_orders_4).collect();
        let ask_num_orders_5: Vec<i32> = ticks.iter().map(|t| t.ask_num_orders_5).collect();

        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(Int64Array::from(ts_ns)),
                Arc::new(Int32Array::from(inst_id)),
                Arc::new(Int16Array::from(side)),
                Arc::new(Float64Array::from(price)),
                Arc::new(Int64Array::from(qty)),
                Arc::new(Int64Array::from(seq_no)),
                Arc::new(Float64Array::from(best_bid_price)),
                Arc::new(Int64Array::from(best_bid_qty)),
                Arc::new(Float64Array::from(best_ask_price)),
                Arc::new(Int64Array::from(best_ask_qty)),
                Arc::new(Float64Array::from(bid_price_2)),
                Arc::new(Int64Array::from(bid_qty_2)),
                Arc::new(Float64Array::from(bid_price_3)),
                Arc::new(Int64Array::from(bid_qty_3)),
                Arc::new(Float64Array::from(bid_price_4)),
                Arc::new(Int64Array::from(bid_qty_4)),
                Arc::new(Float64Array::from(bid_price_5)),
                Arc::new(Int64Array::from(bid_qty_5)),
                Arc::new(Float64Array::from(ask_price_2)),
                Arc::new(Int64Array::from(ask_qty_2)),
                Arc::new(Float64Array::from(ask_price_3)),
                Arc::new(Int64Array::from(ask_qty_3)),
                Arc::new(Float64Array::from(ask_price_4)),
                Arc::new(Int64Array::from(ask_qty_4)),
                Arc::new(Float64Array::from(ask_price_5)),
                Arc::new(Int64Array::from(ask_qty_5)),
                Arc::new(Int64Array::from(ts_recv_ns)),
                Arc::new(Int16Array::from(exchange_type)),
                Arc::new(Int64Array::from(volume)),
                Arc::new(Float64Array::from(avg_traded_price)),
                Arc::new(Float64Array::from(total_buy_qty)),
                Arc::new(Float64Array::from(total_sell_qty)),
                Arc::new(Float64Array::from(open)),
                Arc::new(Float64Array::from(high)),
                Arc::new(Float64Array::from(low)),
                Arc::new(Float64Array::from(close)),
                Arc::new(Int64Array::from(last_trade_ts_ns)),
                Arc::new(Int64Array::from(open_interest)),
                Arc::new(Int64Array::from(oi_change_pct_raw)),
                Arc::new(Float64Array::from(upper_circuit)),
                Arc::new(Float64Array::from(lower_circuit)),
                Arc::new(Float64Array::from(week_52_high)),
                Arc::new(Float64Array::from(week_52_low)),
                Arc::new(Int32Array::from(best_bid_num_orders)),
                Arc::new(Int32Array::from(best_ask_num_orders)),
                Arc::new(Int32Array::from(bid_num_orders_2)),
                Arc::new(Int32Array::from(bid_num_orders_3)),
                Arc::new(Int32Array::from(bid_num_orders_4)),
                Arc::new(Int32Array::from(bid_num_orders_5)),
                Arc::new(Int32Array::from(ask_num_orders_2)),
                Arc::new(Int32Array::from(ask_num_orders_3)),
                Arc::new(Int32Array::from(ask_num_orders_4)),
                Arc::new(Int32Array::from(ask_num_orders_5)),
            ],
        )?;

        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .build();

        let file = fs::File::create(path)?;
        let mut writer = ArrowWriter::try_new(file, self.schema.clone(), Some(props))?;
        writer.write(&batch)?;
        writer.close()?;

        Ok(())
    }
}

impl Drop for ParquetSink {
    fn drop(&mut self) {
        if self.total_rows > 0 {
            if let Err(e) = self.flush_all() {
                error!("Failed to flush Parquet on drop: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make_test_tick(inst_id: i32, price: f64, seq: i64) -> Tick {
        Tick {
            ts_ns: 1_700_000_000_000_000_000,
            inst_id,
            side: 0,
            price,
            qty: 100,
            seq_no: seq,
            best_bid_price: price - 0.05,
            best_bid_qty: 100,
            best_ask_price: price + 0.05,
            best_ask_qty: 100,
            bid_price_2: 0.0, bid_qty_2: 0,
            bid_price_3: 0.0, bid_qty_3: 0,
            bid_price_4: 0.0, bid_qty_4: 0,
            bid_price_5: 0.0, bid_qty_5: 0,
            ask_price_2: 0.0, ask_qty_2: 0,
            ask_price_3: 0.0, ask_qty_3: 0,
            ask_price_4: 0.0, ask_qty_4: 0,
            ask_price_5: 0.0, ask_qty_5: 0,
            ts_recv_ns: 1_700_000_000_100_000_000,
            exchange_type: 1,
            volume: 0,
            avg_traded_price: 0.0,
            total_buy_qty: 0.0,
            total_sell_qty: 0.0,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            close: 0.0,
            last_trade_ts_ns: 0,
            open_interest: 0,
            oi_change_pct_raw: 0,
            upper_circuit: 0.0,
            lower_circuit: 0.0,
            week_52_high: 0.0,
            week_52_low: 0.0,
            best_bid_num_orders: 0,
            best_ask_num_orders: 0,
            bid_num_orders_2: 0,
            bid_num_orders_3: 0,
            bid_num_orders_4: 0,
            bid_num_orders_5: 0,
            ask_num_orders_2: 0,
            ask_num_orders_3: 0,
            ask_num_orders_4: 0,
            ask_num_orders_5: 0,
        }
    }

    #[test]
    fn test_parquet_write_and_read() {
        let tmp_dir = std::env::temp_dir().join("phase0_test_parquet");
        let _ = fs::remove_dir_all(&tmp_dir);

        let mut sink = ParquetSink::new(&tmp_dir);

        // Push some test ticks
        for i in 0..10 {
            sink.push(&make_test_tick(26009, 245.50 + i as f64, i))
                .unwrap();
        }

        sink.flush_all().unwrap();

        // Verify files were created
        let has_parquet = walkdir(&tmp_dir)
            .iter()
            .any(|p| p.extension().map(|e| e == "parquet").unwrap_or(false));
        assert!(has_parquet, "Expected .parquet files in {}", tmp_dir.display());

        // Cleanup
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    /// Recursively list all files in a directory.
    fn walkdir(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    files.extend(walkdir(&path));
                } else {
                    files.push(path);
                }
            }
        }
        files
    }
}
