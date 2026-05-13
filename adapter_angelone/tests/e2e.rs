//! End-to-end pipeline test: data → book → strategy → order → fill
//!
//! Uses NautilusTrader's `BacktestEngine` + `SimulatedExchange` so no live
//! connections are needed.  The `SimulatedExchange` fills market orders
//! immediately against the best quote, producing `OrderFilled` events that
//! update `Portfolio`.
//!
//! This test is the acceptance gate for Step 4 of PHASE_1_CHECKLIST.md:
//! prove the full pipeline before wiring a live `LiveTradingNode`.

use std::fmt::Debug;

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_common::actor::DataActor;
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Data, QuoteTick},
    enums::{AccountType, AssetClass, BookType, CurrencyType, OmsType, OrderSide},
    identifiers::{InstrumentId, Symbol, Venue},
    instruments::{FuturesContract, Instrument, InstrumentAny},
    types::{Currency, Money, Price, Quantity},
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};
use ustr::Ustr;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Indian Rupee — not built in, so we register it once.
fn inr() -> Currency {
    Currency::new("INR", 2, 356, "Indian Rupee", CurrencyType::Fiat)
}

/// NIFTY JUN 2026 futures on NSE.
///
/// Lot size: 75, tick size: ₹0.05, denomination: INR.
fn nifty_jun26_futures() -> FuturesContract {
    // activation 2026-05-01, expiry 2026-05-26 (last Thursday of May)
    let activation = UnixNanos::from(1_746_057_600_000_000_000_u64); // 2026-05-01 00:00 UTC
    let expiration = UnixNanos::from(1_748_304_000_000_000_000_u64); // 2026-05-26 10:00 UTC

    let venue = Venue::new("NSE");
    FuturesContract::new(
        InstrumentId::new(Symbol::new("NIFTY26MAYFUT"), venue),
        Symbol::new("NIFTY26MAYFUT"),
        AssetClass::Index,
        Some(Ustr::from("XNSE")),
        Ustr::from("NIFTY"),
        activation,
        expiration,
        inr(),
        2,                          // price precision (₹1.50 → "1.50")
        Price::from("0.05"),        // tick size
        Quantity::from("1"),        // multiplier
        Quantity::from("75"),       // lot size (NSE NIFTY standard)
        None,                       // max_quantity
        Some(Quantity::from("75")), // min_quantity = 1 lot
        None,                       // max_price
        None,                       // min_price
        None,                       // margin_init
        None,                       // margin_maint
        None,                       // maker_fee
        None,                       // taker_fee
        None,                       // info
        UnixNanos::default(),
        UnixNanos::default(),
    )
}

/// Build a standard NSE backtest engine with 50 crore rupees starting balance.
fn create_nse_engine() -> BacktestEngine {
    let config = BacktestEngineConfig::default();
    let mut engine = BacktestEngine::new(config).unwrap();
    engine
        .add_venue(
            SimulatedVenueConfig::builder()
                .venue(Venue::new("NSE"))
                .oms_type(OmsType::Netting)
                .account_type(AccountType::Cash)
                .book_type(BookType::L1_MBP)
                .starting_balances(vec![Money::new(500_000_000.0, inr())])
                .build(),
        )
        .unwrap();
    engine
}

/// Construct a quote tick for the NIFTY futures in integer-paise prices.
///
/// `bid_rs` / `ask_rs` are ₹ strings (e.g. `"24500.00"`), `ts_ns` is a
/// monotonically-increasing UNIX nanosecond timestamp.
fn nifty_quote(instrument_id: InstrumentId, bid_rs: &str, ask_rs: &str, ts_ns: u64) -> Data {
    Data::Quote(QuoteTick::new(
        instrument_id,
        Price::from(bid_rs),
        Price::from(ask_rs),
        Quantity::from("75"),  // 1 lot
        Quantity::from("75"),  // 1 lot
        ts_ns.into(),
        ts_ns.into(),
    ))
}

// ---------------------------------------------------------------------------
// Stub strategy: buy 1 lot on the very first quote tick, then do nothing.
// ---------------------------------------------------------------------------

struct BuyOnFirstTickStrategy {
    core: StrategyCore,
    instrument_id: InstrumentId,
    has_bought: bool,
}

impl BuyOnFirstTickStrategy {
    fn new(instrument_id: InstrumentId) -> Self {
        let config = StrategyConfig {
            strategy_id: Some("BUY-FIRST-001".into()),
            order_id_tag: Some("001".to_string()),
            ..Default::default()
        };
        Self {
            core: StrategyCore::new(config),
            instrument_id,
            has_bought: false,
        }
    }
}

nautilus_strategy!(BuyOnFirstTickStrategy);

impl Debug for BuyOnFirstTickStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuyOnFirstTickStrategy")
            .field("instrument_id", &self.instrument_id)
            .field("has_bought", &self.has_bought)
            .finish()
    }
}

impl DataActor for BuyOnFirstTickStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        // Subscribe to L1 quote ticks from the simulated NSE venue.
        self.subscribe_quotes(self.instrument_id, None, None);
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.unsubscribe_quotes(self.instrument_id, None, None);
        Ok(())
    }

    fn on_quote(&mut self, _quote: &QuoteTick) -> anyhow::Result<()> {
        if self.has_bought {
            return Ok(());
        }
        // Place a market buy for 1 NIFTY lot (75 units).
        let order = self.core.order_factory().market(
            self.instrument_id,
            OrderSide::Buy,
            Quantity::from("75"), // 1 lot
            None,                  // time_in_force → Day
            None,                  // reduce_only
            None,                  // quote_quantity
            None,                  // display_qty
            None,                  // expire_time
            None,                  // emulation_trigger
            None,                  // tags
        );
        self.submit_order(order, None, None, None)?;
        self.has_bought = true;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// **Step 4 acceptance test (happy path)**
///
/// Pipeline: QuoteTick → DataEngine → strategy `on_quote` → `submit_order`
/// → SimulatedExchange fills at bid → `OrderFilled` → Portfolio position.
#[test]
fn test_e2e_market_buy_filled_on_first_tick() {
    let instrument = nifty_jun26_futures();
    let instrument_id = instrument.id();
    let instrument_any = InstrumentAny::FuturesContract(instrument);

    let mut engine = create_nse_engine();
    engine.add_instrument(&instrument_any).unwrap();

    let strategy = BuyOnFirstTickStrategy::new(instrument_id);
    engine.add_strategy(strategy).unwrap();

    // Feed 5 ticks at one-second intervals starting at 09:15 IST (03:45 UTC).
    let base_ns: u64 = 1_746_089_700_000_000_000; // 2026-05-01 03:45:00 UTC
    let step_ns: u64 = 1_000_000_000; // 1 second

    let ticks: Vec<Data> = (0..5)
        .map(|i| {
            let mid = 24_500.00 + (i as f64) * 5.0;
            let bid = format!("{:.2}", mid - 0.50);
            let ask = format!("{:.2}", mid + 0.50);
            nifty_quote(instrument_id, &bid, &ask, base_ns + i * step_ns)
        })
        .collect();

    engine.add_data(ticks, None, true, true).unwrap();
    engine.run(None, None, None, false).unwrap();

    let result = engine.get_result();
    assert_eq!(result.iterations, 5, "All 5 ticks should be processed");
    assert!(
        result.total_orders >= 1,
        "Expected at least 1 order (market buy on first tick), got {}",
        result.total_orders
    );
    assert!(
        result.total_positions >= 1,
        "Expected at least 1 position after fill, got {}",
        result.total_positions
    );
}

/// **Idle pipeline test**
///
/// When no strategy subscribes to quotes, no orders are placed.
#[test]
fn test_e2e_no_orders_when_strategy_absent() {
    let instrument = nifty_jun26_futures();
    let instrument_id = instrument.id();
    let instrument_any = InstrumentAny::FuturesContract(instrument);

    let mut engine = create_nse_engine();
    engine.add_instrument(&instrument_any).unwrap();
    // Intentionally no strategy added.

    let ticks: Vec<Data> = (0..3)
        .map(|i| nifty_quote(instrument_id, "24499.50", "24500.50", 1_746_089_700_000_000_000 + i * 1_000_000_000))
        .collect();

    engine.add_data(ticks, None, true, true).unwrap();
    engine.run(None, None, None, false).unwrap();

    let result = engine.get_result();
    assert_eq!(result.total_orders, 0);
    assert_eq!(result.total_positions, 0);
}

/// **Idempotency test**
///
/// BuyOnFirstTickStrategy should only ever place ONE order even across many ticks.
#[test]
fn test_e2e_strategy_buys_exactly_once() {
    let instrument = nifty_jun26_futures();
    let instrument_id = instrument.id();
    let instrument_any = InstrumentAny::FuturesContract(instrument);

    let mut engine = create_nse_engine();
    engine.add_instrument(&instrument_any).unwrap();
    engine
        .add_strategy(BuyOnFirstTickStrategy::new(instrument_id))
        .unwrap();

    // 20 ticks — strategy must only place 1 order
    let ticks: Vec<Data> = (0..20_u64)
        .map(|i| nifty_quote(instrument_id, "24499.50", "24500.50", 1_746_089_700_000_000_000 + i * 1_000_000_000))
        .collect();

    engine.add_data(ticks, None, true, true).unwrap();
    engine.run(None, None, None, false).unwrap();

    let result = engine.get_result();
    assert_eq!(
        result.total_orders, 1,
        "Strategy should submit exactly 1 order, got {}",
        result.total_orders
    );
}
