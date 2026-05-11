use anyhow::{Result, Context};
use clap::Parser;
use std::fs::File;
use std::path::PathBuf;
use tracing::{error, info};

use arrow::array::{Float64Array, Int64Array};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use nautilus_backtest::config::{BacktestEngineConfig, SimulatedVenueConfig};
use nautilus_backtest::engine::BacktestEngine;
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Data, QuoteTick},
    enums::{AccountType, BookType, OmsType},
    identifiers::{InstrumentId, Symbol, Venue},
    types::{Currency, Price, Quantity},
};
use strategy_basis_arb::BasisArbStrategy;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Date to replay (YYYY-MM-DD)
    #[arg(long)]
    date: String,
    
    /// Path to data directory
    #[arg(long, default_value = "./data/raw")]
    data_dir: String,
}

fn load_parquet(path: &PathBuf, venue: Venue) -> Result<Vec<Data>> {
    let file = File::open(path).context("Failed to open parquet file")?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let mut reader = builder.build()?;
    let mut data = Vec::new();

    let symbol_str = path.file_stem().unwrap().to_str().unwrap();
    let instrument_id = InstrumentId::new(Symbol::new(symbol_str), venue);

    while let Some(batch_res) = reader.next() {
        let batch = batch_res?;
        let ts_col = batch.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
        // Skip inst_id (1), side (2), price (3), qty (4), seq_no (5)
        let bid_price_col = batch.column(6).as_any().downcast_ref::<Float64Array>().unwrap();
        let bid_qty_col = batch.column(7).as_any().downcast_ref::<Int64Array>().unwrap();
        let ask_price_col = batch.column(8).as_any().downcast_ref::<Float64Array>().unwrap();
        let ask_qty_col = batch.column(9).as_any().downcast_ref::<Int64Array>().unwrap();

        for i in 0..batch.num_rows() {
            let ts = UnixNanos::from(ts_col.value(i) as u64);
            let bid_price = Price::new(bid_price_col.value(i), 2);
            let bid_size = Quantity::new(bid_qty_col.value(i) as f64, 0);
            let ask_price = Price::new(ask_price_col.value(i), 2);
            let ask_size = Quantity::new(ask_qty_col.value(i) as f64, 0);

            let tick = QuoteTick::new(
                instrument_id,
                bid_price,
                ask_price,
                bid_size,
                ask_size,
                ts,
                ts,
            );
            data.push(Data::Quote(tick));
        }
    }
    
    Ok(data)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    info!("Starting backtest for date: {}", args.date);

    let mut engine = BacktestEngine::new(BacktestEngineConfig::default())?;
    let venue = Venue::new("NSE");

    let venue_config = SimulatedVenueConfig::builder()
        .venue(venue)
        .oms_type(OmsType::Hedging)
        .account_type(AccountType::Margin)
        .book_type(BookType::L1_MBP)
        .starting_balances(vec![])
        .base_currency(std::str::FromStr::from_str("INR").unwrap())
        .build();
    engine.add_venue(venue_config)?;

    // Load Data
    let date_parts: Vec<&str> = args.date.split('-').collect();
    if date_parts.len() != 3 {
        anyhow::bail!("Date must be YYYY-MM-DD");
    }
    
    let path = PathBuf::from(&args.data_dir)
        .join(date_parts[0])
        .join(date_parts[1])
        .join(date_parts[2]);
        
    if !path.exists() {
        anyhow::bail!("Data directory does not exist: {:?}", path);
    }

    let mut all_data = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("parquet") {
            info!("Loading {:?}", p);
            let mut data = load_parquet(&p, venue)?;
            all_data.append(&mut data);
        }
    }

    info!("Sorting {} total ticks...", all_data.len());
    engine.add_data(all_data, None, false, true)?;

    let strategy_params = strategy_basis_arb::BasisArbParams::from_file("config/strategy_basis_arb.toml")
        .context("Failed to load strategy_basis_arb.toml")?;
    let futures_id = InstrumentId::new(
        Symbol::new(&strategy_params.futures_instrument_id.replace(".NSE", "")),
        venue,
    );
    let spot_id = InstrumentId::new(
        Symbol::new(&strategy_params.spot_instrument_id.replace(".NSE", "")),
        venue,
    );
    let strategy = BasisArbStrategy::new(strategy_basis_arb::BasisArbConfig::new(strategy_params, futures_id, spot_id));
    engine.add_strategy(strategy)?;

    let vwap_params = strategy_intraday_vwap::IntradayVwapParams::from_file("config/strategy_intraday_vwap.toml")
        .context("Failed to load strategy_intraday_vwap.toml")?;
    let vwap_strategy = strategy_intraday_vwap::IntradayVwapStrategy::new(strategy_intraday_vwap::IntradayVwapConfig::new(vwap_params));
    engine.add_strategy(vwap_strategy)?;

    info!("Running Backtest...");
    engine.run(None, None, None, false)?;
    info!("Backtest completed!");

    Ok(())
}
