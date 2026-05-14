//! # Storage Node
//!
//! Consumes ticks from the ingestion channel and fans out to:
//! - **Hot sink:** QuestDB (ILP/HTTP) for real-time queries
//! - **Cold sink:** Parquet files for offline analysis
//!
//! Both sinks operate independently; a failure in one does not block the other.

pub mod parquet_sink;
pub mod questdb_sink;

use common::Tick;
use parquet_sink::ParquetSink;
use questdb_sink::QuestDbSink;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Run the storage consumer loop.
///
/// Reads ticks from the provided channel and writes them to both the QuestDB
/// hot sink and the Parquet cold sink. If QuestDB is unavailable, the system
/// degrades gracefully and continues writing Parquet files.
pub async fn storage_consumer(mut rx: mpsc::Receiver<Tick>) {
    // Try to connect to QuestDB (optional — pipeline works without it)
    let mut questdb = match QuestDbSink::new() {
        Ok(sink) => {
            info!("QuestDB hot sink ready");
            Some(sink)
        }
        Err(e) => {
            warn!("QuestDB unavailable, running in Parquet-only mode: {e}");
            None
        }
    };

    let mut parquet = ParquetSink::new("./data/raw");

    let mut count: u64 = 0;
    let mut questdb_errors: u64 = 0;

    // Periodic flush timer for Parquet
    let mut flush_interval = tokio::time::interval(tokio::time::Duration::from_secs(60));

    loop {
        tokio::select! {
            tick = rx.recv() => {
                match tick {
                    Some(tick) => {
                        count += 1;

                        // Drop pre-market circuit-limit packets.
                        // At market open Angel One sends the upper/lower circuit
                        // limits in the depth fields rather than real order book
                        // prices, producing a wildly crossed quote (e.g. bid=1235,
                        // ask=1010 for a ₹1100 stock). Discard any tick where the
                        // bid-ask gap exceeds 5% of the ask price.
                        if tick.best_ask_price > 0.0 {
                            let spread_pct = (tick.best_bid_price - tick.best_ask_price).abs()
                                / tick.best_ask_price;
                            if spread_pct > 0.05 {
                                continue;
                            }
                        }

                        // Hot sink: QuestDB
                        if let Some(ref mut qdb) = questdb {
                            if let Err(e) = qdb.write_tick(&tick) {
                                questdb_errors += 1;
                                if questdb_errors <= 5 {
                                    error!("QuestDB write error (#{questdb_errors}): {e}");
                                }
                                // Don't fail the whole pipeline
                            }
                        }

                        // Cold sink: Parquet
                        if let Err(e) = parquet.push(&tick) {
                            error!("Parquet buffer error: {e}");
                        }
                    }
                    None => {
                        info!("Tick channel closed, flushing and shutting down storage");
                        break;
                    }
                }
            }
            _ = flush_interval.tick() => {
                // Time-based Parquet flush
                if parquet.should_time_flush() {
                    if let Err(e) = parquet.flush_all() {
                        error!("Parquet time-based flush error: {e}");
                    }
                }
                // QuestDB periodic flush
                if let Some(ref mut qdb) = questdb {
                    if let Err(e) = qdb.flush() {
                        error!("QuestDB periodic flush error: {e}");
                    }
                }
            }
        }
    }

    // Final flush
    if let Some(ref mut qdb) = questdb {
        if let Err(e) = qdb.flush() {
            error!("QuestDB final flush error: {e}");
        }
    }
    if let Err(e) = parquet.flush_all() {
        error!("Parquet final flush error: {e}");
    }

    info!("Storage consumer shut down. Processed {count} ticks.");
}
