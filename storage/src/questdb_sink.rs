//! # QuestDB Hot Sink
//!
//! Streams tick data into QuestDB via the InfluxDB Line Protocol (ILP) over HTTP.
//! This provides sub-millisecond write latency for real-time querying through
//! QuestDB's web console at `http://localhost:9000`.

use common::Tick;
use questdb::ingress::{Buffer, Sender, TimestampNanos};
use tracing::{debug, error, info};

/// QuestDB ILP sender wrapper with auto-flushing buffer.
pub struct QuestDbSink {
    sender: Sender,
    buffer: Buffer,
    /// Number of rows buffered before an automatic flush.
    flush_threshold: usize,
    /// Current number of buffered rows.
    buffered: usize,
}

impl QuestDbSink {
    /// Create a new QuestDB sink.
    ///
    /// Connects to the QuestDB instance using the address from the
    /// `QUESTDB_ILP_ADDR` env var (default: `localhost:9000`).
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let addr = std::env::var("QUESTDB_ILP_ADDR")
            .unwrap_or_else(|_| "localhost:9000".to_string());

        let conf_str = format!("http::addr={addr};");
        let sender = Sender::from_conf(&conf_str)?;

        info!("QuestDB sink connected to {addr}");

        Ok(Self {
            sender,
            buffer: Buffer::new(),
            flush_threshold: 1000,
            buffered: 0,
        })
    }

    /// Buffer a single tick for writing to QuestDB.
    ///
    /// The buffer is automatically flushed when `flush_threshold` rows are reached.
    pub fn write_tick(&mut self, tick: &Tick) -> Result<(), Box<dyn std::error::Error>> {
        self.buffer
            .table("ticks")?
            .column_i64("inst_id", tick.inst_id as i64)?
            .column_i64("side", tick.side as i64)?
            .column_f64("price", tick.price)?
            .column_i64("qty", tick.qty)?
            .column_i64("seq_no", tick.seq_no)?
            .column_f64("best_bid_price", tick.best_bid_price)?
            .column_i64("best_bid_qty", tick.best_bid_qty)?
            .column_f64("best_ask_price", tick.best_ask_price)?
            .column_i64("best_ask_qty", tick.best_ask_qty)?
            .at(TimestampNanos::new(tick.ts_ns))?;

        self.buffered += 1;

        if self.buffered >= self.flush_threshold {
            self.flush()?;
        }

        Ok(())
    }

    /// Flush all buffered rows to QuestDB immediately.
    pub fn flush(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.buffered == 0 {
            return Ok(());
        }

        self.sender.flush(&mut self.buffer)?;
        debug!("Flushed {} ticks to QuestDB", self.buffered);
        self.buffered = 0;

        Ok(())
    }
}

impl Drop for QuestDbSink {
    fn drop(&mut self) {
        if self.buffered > 0 {
            if let Err(e) = self.flush() {
                error!("Failed to flush remaining ticks on drop: {e}");
            }
        }
    }
}
