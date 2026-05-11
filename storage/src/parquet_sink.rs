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

            let filename = format!("{inst_id}.parquet");
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
