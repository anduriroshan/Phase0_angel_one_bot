//! Integration tests for `BasisArbStrategy`.
//!
//! Tests run through NautilusTrader's `BacktestEngine` with a
//! `SimulatedExchange` for NSE.  No live network connections.
//!
//! ## Tests
//! - `test_signal_emitted_on_threshold_breach` — synthetic tick stream that
//!   crosses the z-score threshold produces exactly 1 order (Step 6).
//! - `test_replay_determinism` — running the same tick stream twice yields
//!   bit-identical order counts (Step 7).
//! - `test_no_signal_before_min_samples` — signal not emitted until window warm-up.
//! - `test_no_signal_within_threshold` — noise ticks below threshold produce no orders.

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Data, QuoteTick},
    enums::{AccountType, AssetClass, BookType, CurrencyType, OmsType},
    identifiers::{InstrumentId, Symbol, Venue},
    instruments::{FuturesContract, IndexInstrument, InstrumentAny},
    types::{Currency, Money, Price, Quantity},
};
use strategy_basis_arb::{BasisArbConfig, BasisArbParams, BasisArbStrategy};
use ustr::Ustr;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn inr() -> Currency {
    Currency::new("INR", 2, 356, "Indian Rupee", CurrencyType::Fiat)
}

fn nse_venue() -> Venue {
    Venue::new("NSE")
}

fn futures_id() -> InstrumentId {
    InstrumentId::new(Symbol::new("NIFTY26JUNFUT"), nse_venue())
}

fn spot_id() -> InstrumentId {
    InstrumentId::new(Symbol::new("NIFTY"), nse_venue())
}

/// NIFTY JUN 2026 futures instrument.
fn nifty_futures() -> FuturesContract {
    let activation = UnixNanos::from(1_746_057_600_000_000_000_u64);
    let expiration = UnixNanos::from(1_751_001_600_000_000_000_u64);
    // FuturesContract::new returns Self (panics on invalid) — no .expect() needed.
    FuturesContract::new(
        futures_id(),
        Symbol::new("NIFTY26JUNFUT"),
        AssetClass::Index,
        Some(Ustr::from("XNSE")),
        Ustr::from("NIFTY"),
        activation,
        expiration,
        inr(),
        2,
        Price::from("0.05"),
        Quantity::from("1"),
        Quantity::from("75"),
        None,
        Some(Quantity::from("75")),
        None, None, None, None, None, None, None,
        UnixNanos::default(),
        UnixNanos::default(),
    )
}

/// NIFTY spot index instrument.
/// IndexInstrument::new(id, raw_symbol, currency, price_precision, size_precision,
///   price_increment, size_increment, info, ts_event, ts_init) — 10 args, returns Self.
fn nifty_spot() -> IndexInstrument {
    IndexInstrument::new(
        spot_id(),
        Symbol::new("NIFTY"),
        inr(),
        2_u8,                   // price_precision
        0_u8,                   // size_precision
        Price::from("0.05"),    // price_increment
        Quantity::from("1"),    // size_increment
        None,                   // info
        UnixNanos::default(),
        UnixNanos::default(),
    )
}

/// Standard NSE backtest engine with ₹5-crore starting balance.
fn create_engine() -> BacktestEngine {
    let mut engine = BacktestEngine::new(BacktestEngineConfig::default()).unwrap();
    engine
        .add_venue(
            SimulatedVenueConfig::builder()
                .venue(nse_venue())
                .oms_type(OmsType::Netting)
                .account_type(AccountType::Cash)
                .book_type(BookType::L1_MBP)
                .starting_balances(vec![Money::new(500_000_000.0, inr())])
                .build(),
        )
        .unwrap();
    engine
}

/// Default strategy config: small window, low min_samples for fast testing.
fn test_config() -> BasisArbConfig {
    let params = BasisArbParams {
        schema_version: 1,
        window_secs: 1,     // tiny window for test speed
        z_score_threshold: 2.0,
        min_samples: 5,     // warm up quickly
        futures_instrument_id: "NIFTY26JUNFUT.NSE".to_string(),
        spot_instrument_id: "NIFTY.NSE".to_string(),
        trade_qty_units: 75,
    };
    BasisArbConfig::new(params, futures_id(), spot_id())
}

/// Constructs a QuoteTick for an instrument.
fn quote(id: InstrumentId, bid: &str, ask: &str, ts_ns: u64) -> Data {
    Data::Quote(QuoteTick::new(
        id,
        Price::from(bid),
        Price::from(ask),
        Quantity::from("75"),
        Quantity::from("75"),
        ts_ns.into(),
        ts_ns.into(),
    ))
}

/// Builds a synthetic tick stream interleaving futures and spot ticks.
///
/// `futures_prices` and `spot_prices` are paired `(bid, ask)` strings.
/// Timestamps are monotonically increasing, 1 second apart.
fn build_ticks(
    futures_prices: &[(&str, &str)],
    spot_prices: &[(&str, &str)],
    base_ns: u64,
) -> Vec<Data> {
    assert_eq!(futures_prices.len(), spot_prices.len());
    let step_ns: u64 = 1_000_000_000;
    let mut ticks = Vec::with_capacity(futures_prices.len() * 2);
    for (i, ((fb, fa), (sb, sa))) in futures_prices.iter().zip(spot_prices.iter()).enumerate() {
        let ts = base_ns + i as u64 * step_ns;
        ticks.push(quote(futures_id(), fb, fa, ts));
        ticks.push(quote(spot_id(), sb, sa, ts + 1)); // spot 1 ns after futures
    }
    ticks
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// **Step 6 acceptance test** — signal emitted exactly once when z-score breaches threshold.
///
/// Design:
/// - 5 warm-up ticks: basis ≈ 100 (futures at 24600, spot at 24500)
/// - 1 shock tick: futures spike to 24900, basis jumps to 400 → z ≫ 2 → SELL signal
#[test]
fn test_signal_emitted_on_threshold_breach() {
    let mut engine = create_engine();
    engine.add_instrument(&InstrumentAny::FuturesContract(nifty_futures())).unwrap();
    engine.add_instrument(&InstrumentAny::IndexInstrument(nifty_spot())).unwrap();

    let strategy = BasisArbStrategy::new(test_config());
    engine.add_strategy(strategy).unwrap();

    let base_ns: u64 = 1_746_089_700_000_000_000;

    // 5 warm-up ticks: normal basis of ~100
    let warm_up: Vec<(&str, &str)> = vec![
        ("24599.50", "24600.50"), // futures
        ("24599.50", "24600.50"),
        ("24599.50", "24600.50"),
        ("24599.50", "24600.50"),
        ("24599.50", "24600.50"),
    ];
    let warm_spot: Vec<(&str, &str)> = vec![
        ("24499.50", "24500.50"), // spot
        ("24499.50", "24500.50"),
        ("24499.50", "24500.50"),
        ("24499.50", "24500.50"),
        ("24499.50", "24500.50"),
    ];

    // 1 shock tick: futures spike 300 points above normal → z ≫ 2
    let shock_fut = vec![("24899.50", "24900.50")];
    let shock_spot = vec![("24499.50", "24500.50")];

    let mut ticks = build_ticks(&warm_up, &warm_spot, base_ns);
    let shock_base = base_ns + 5 * 1_000_000_000;
    ticks.extend(build_ticks(&shock_fut, &shock_spot, shock_base));

    engine.add_data(ticks, None, true, true).unwrap();
    engine.run(None, None, None, false).unwrap();

    let result = engine.get_result();
    assert_eq!(
        result.total_orders, 1,
        "Expected exactly 1 order on threshold breach, got {}",
        result.total_orders
    );
}

/// **Step 7 — replay determinism** — same tick stream → identical order count both runs.
#[test]
fn test_replay_determinism() {
    let base_ns: u64 = 1_746_089_700_000_000_000;

    let warm_up: Vec<(&str, &str)> = vec![
        ("24599.50", "24600.50"),
        ("24599.50", "24600.50"),
        ("24599.50", "24600.50"),
        ("24599.50", "24600.50"),
        ("24599.50", "24600.50"),
    ];
    let warm_spot: Vec<(&str, &str)> = vec![
        ("24499.50", "24500.50"),
        ("24499.50", "24500.50"),
        ("24499.50", "24500.50"),
        ("24499.50", "24500.50"),
        ("24499.50", "24500.50"),
    ];
    let shock_fut = vec![("24899.50", "24900.50")];
    let shock_spot = vec![("24499.50", "24500.50")];

    let run = |base: u64| -> usize {
        let mut engine = create_engine();
        engine.add_instrument(&InstrumentAny::FuturesContract(nifty_futures())).unwrap();
        engine.add_instrument(&InstrumentAny::IndexInstrument(nifty_spot())).unwrap();
        engine.add_strategy(BasisArbStrategy::new(test_config())).unwrap();

        let mut ticks = build_ticks(&warm_up, &warm_spot, base);
        let shock_base = base + 5 * 1_000_000_000;
        ticks.extend(build_ticks(&shock_fut, &shock_spot, shock_base));

        engine.add_data(ticks, None, true, true).unwrap();
        engine.run(None, None, None, false).unwrap();
        engine.get_result().total_orders
    };

    let run1 = run(base_ns);
    let run2 = run(base_ns);
    assert_eq!(run1, run2, "Replay non-determinism: run1={run1} run2={run2}");
    // Also assert both produce exactly 1 order.
    assert_eq!(run1, 1, "Expected 1 order per run, got {run1}");
}

/// **No signal before warm-up** — z-score is undefined until `min_samples` are collected.
#[test]
fn test_no_signal_before_min_samples() {
    let mut engine = create_engine();
    engine.add_instrument(&InstrumentAny::FuturesContract(nifty_futures())).unwrap();
    engine.add_instrument(&InstrumentAny::IndexInstrument(nifty_spot())).unwrap();

    let mut config = test_config();
    config.params.min_samples = 100; // never reached with 3 ticks
    engine.add_strategy(BasisArbStrategy::new(config)).unwrap();

    let base_ns: u64 = 1_746_089_700_000_000_000;
    let fut = vec![("24599.50", "24600.50"); 3];
    let spot = vec![("24499.50", "24500.50"); 3];
    let ticks = build_ticks(&fut, &spot, base_ns);

    engine.add_data(ticks, None, true, true).unwrap();
    engine.run(None, None, None, false).unwrap();

    assert_eq!(engine.get_result().total_orders, 0, "No orders before min_samples");
}

/// **No signal within threshold** — small random noise stays within ±2σ.
#[test]
fn test_no_signal_within_threshold() {
    let mut engine = create_engine();
    engine.add_instrument(&InstrumentAny::FuturesContract(nifty_futures())).unwrap();
    engine.add_instrument(&InstrumentAny::IndexInstrument(nifty_spot())).unwrap();

    engine.add_strategy(BasisArbStrategy::new(test_config())).unwrap();

    // 30 ticks with basis staying in [99, 101] — z never exceeds 2σ
    let base_ns: u64 = 1_746_089_700_000_000_000;
    let prices: Vec<(&str, &str)> = [
        "24599.50", "24600.50", "24598.50", "24601.50", "24599.50",
        "24600.50", "24598.50", "24601.50", "24599.50", "24600.50",
        "24599.50", "24600.50", "24598.50", "24601.50", "24599.50",
        "24600.50", "24598.50", "24601.50", "24599.50", "24600.50",
        "24599.50", "24600.50", "24598.50", "24601.50", "24599.50",
        "24600.50", "24598.50", "24601.50", "24599.50", "24600.50",
    ].chunks(2).map(|c| (c[0], c[1])).collect();

    let spot: Vec<(&str, &str)> = vec![("24499.50", "24500.50"); prices.len()];
    let ticks = build_ticks(&prices, &spot, base_ns);

    engine.add_data(ticks, None, true, true).unwrap();
    engine.run(None, None, None, false).unwrap();

    assert_eq!(engine.get_result().total_orders, 0, "No orders for ticks within threshold");
}
