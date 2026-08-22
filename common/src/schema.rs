//! Unified Tick Schema and supporting types.
//!
//! Every tick that flows through the pipeline—regardless of broker or
//! subscription mode—is normalized to the [`Tick`] struct before it touches
//! storage or the circuit-breaker PnL stream.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Exchange Types (Angel One SmartAPI)
// ---------------------------------------------------------------------------

/// Broker exchange identifiers used in WebSocket subscription requests
/// and present in every binary response packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ExchangeType {
    NseCm = 1,
    NseFo = 2,
    BseCm = 3,
    BseFo = 4,
    McxFo = 5,
    NcxFo = 7,
    CdeFo = 13,
}

impl ExchangeType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::NseCm),
            2 => Some(Self::NseFo),
            3 => Some(Self::BseCm),
            4 => Some(Self::BseFo),
            5 => Some(Self::McxFo),
            7 => Some(Self::NcxFo),
            13 => Some(Self::CdeFo),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Subscription Modes
// ---------------------------------------------------------------------------

/// Data granularity modes supported by the Angel One WebSocket v2 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum SubscriptionMode {
    Ltp = 1,
    Quote = 2,
    SnapQuote = 3,
    Depth = 4,
}

impl SubscriptionMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Ltp),
            2 => Some(Self::Quote),
            3 => Some(Self::SnapQuote),
            4 => Some(Self::Depth),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Unified Tick Schema
// ---------------------------------------------------------------------------

/// The canonical tick representation used throughout the pipeline.
///
/// All fields are populated from the binary WebSocket stream and mapped
/// to this struct before being pushed into the in-memory channel.
///
/// **Price convention:** Angel One transmits prices as integers in *paise*
/// (i.e. ₹245.50 → 24550). The `price` field stores the converted `f64`
/// value (divided by 100).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tick {
    /// Exchange timestamp in nanoseconds since epoch.
    /// Derived from the exchange_timestamp field (milliseconds) × 1_000_000.
    pub ts_ns: i64,

    /// Internal instrument identifier.  
    /// Parsed from the 25-byte null-terminated ASCII token in the binary packet.
    pub inst_id: i32,

    /// Trade side: 1 = Buy, 2 = Sell, 0 = Unknown / Trade.
    /// Derived from the best-bid/ask context or set to 0 for LTP-only feeds.
    pub side: i8,

    /// Execution or quote price in ₹ (paise value ÷ 100).
    pub price: f64,

    /// Order or trade quantity.
    pub qty: i64,

    /// Exchange sequence number for gap detection.
    pub seq_no: i64,

    /// Best Bid Price (Top of Book L1)
    pub best_bid_price: f64,

    /// Best Bid Quantity
    pub best_bid_qty: i64,

    /// Best Ask Price (Top of Book L1)
    pub best_ask_price: f64,

    /// Best Ask Quantity
    pub best_ask_qty: i64,

    // --- Order-book depth levels 2..5 (L2) ---
    // Populated from SnapQuote (mode 3); 0 when depth is unavailable. Bid levels
    // come from the packet's best_5_sell, ask levels from best_5_buy (the
    // documented Angel One flag inversion — see `to_tick`). Appended after the
    // L1 fields so existing readers / parquet files remain valid.
    pub bid_price_2: f64,
    pub bid_qty_2: i64,
    pub bid_price_3: f64,
    pub bid_qty_3: i64,
    pub bid_price_4: f64,
    pub bid_qty_4: i64,
    pub bid_price_5: f64,
    pub bid_qty_5: i64,
    pub ask_price_2: f64,
    pub ask_qty_2: i64,
    pub ask_price_3: f64,
    pub ask_qty_3: i64,
    pub ask_price_4: f64,
    pub ask_qty_4: i64,
    pub ask_price_5: f64,
    pub ask_qty_5: i64,

    // --- Capture-boundary metadata ---
    /// Wall-clock time this packet was received by our process, in nanoseconds
    /// since epoch. NOT used in any trading decision (would violate replay
    /// determinism — see ADR-005); this is diagnostic-only, for measuring feed
    /// latency and detecting stale/frozen connections after the fact.
    pub ts_recv_ns: i64,

    /// Angel One exchange_type this tick arrived on (1=NSE_CM, 2=NSE_FO,
    /// 3=BSE_CM, 4=BSE_FO, 5=MCX_FO, 7=NCX_FO, 13=CDE_FO). Provenance field —
    /// lets a single store safely mix instruments from different exchanges.
    pub exchange_type: i16,

    // --- Quote extension (mode 2+): previously parsed and discarded ---
    /// Cumulative traded volume for the session so far. 0 for LTP-only feeds.
    /// Required for a real (volume-weighted) VWAP; the mid-price running mean
    /// used today is only an approximation without this field.
    pub volume: i64,
    /// Exchange-computed average traded price for the session so far (₹).
    pub avg_traded_price: f64,
    /// Cumulative total buy-side order quantity across the book (as sent by
    /// the exchange, not just top-5 depth). A raw order-flow imbalance input.
    pub total_buy_qty: f64,
    /// Cumulative total sell-side order quantity across the book.
    pub total_sell_qty: f64,
    /// Session open price (₹).
    pub open: f64,
    /// Session high price so far (₹).
    pub high: f64,
    /// Session low price so far (₹).
    pub low: f64,
    /// Previous session close price (₹).
    pub close: f64,

    // --- SnapQuote extension (mode 3): previously parsed and discarded ---
    /// Timestamp of the last actual trade (not just this quote update), in
    /// nanoseconds since epoch. 0 when unavailable (e.g. LTP/Quote-only modes).
    pub last_trade_ts_ns: i64,
    /// Open interest (F&O only; 0 for cash-market instruments).
    pub open_interest: i64,
    /// Raw exchange value for OI change. Scale/sign are NOT verified against
    /// a live sample — do not assume this is already a percentage until
    /// confirmed against real data. Stored raw rather than guessed-converted.
    pub oi_change_pct_raw: i64,
    /// Upper circuit price band for the session (₹). Needed to correctly
    /// distinguish real crossed-book artifacts from legitimate circuit-limit
    /// quotes, instead of the fixed 1% spread heuristic used today.
    pub upper_circuit: f64,
    /// Lower circuit price band for the session (₹).
    pub lower_circuit: f64,
    /// 52-week high (₹).
    pub week_52_high: f64,
    /// 52-week low (₹).
    pub week_52_low: f64,

    // --- Depth order counts (mode 3) ---
    // Distinguishes "one large order" from "many small orders" at a price
    // level — invisible in qty alone. Same bid/ask inversion as the price/qty
    // ladders above (see `to_tick`): bid counts come from best_5_sell,
    // ask counts come from best_5_buy.
    pub best_bid_num_orders: i32,
    pub best_ask_num_orders: i32,
    pub bid_num_orders_2: i32,
    pub bid_num_orders_3: i32,
    pub bid_num_orders_4: i32,
    pub bid_num_orders_5: i32,
    pub ask_num_orders_2: i32,
    pub ask_num_orders_3: i32,
    pub ask_num_orders_4: i32,
    pub ask_num_orders_5: i32,
}

// ---------------------------------------------------------------------------
// Extended Quote Data (available in Quote and SnapQuote modes)
// ---------------------------------------------------------------------------

/// Additional OHLCV and market depth fields available in Quote (mode 2)
/// and SnapQuote (mode 3) subscriptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteData {
    pub last_traded_qty: i64,
    pub avg_traded_price: i64,
    pub volume: i64,
    pub total_buy_qty: f64,
    pub total_sell_qty: f64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
}

/// SnapQuote-only fields (mode 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapQuoteData {
    pub last_traded_timestamp: i64,
    pub open_interest: i64,
    pub oi_change_pct: i64,
    pub upper_circuit: i64,
    pub lower_circuit: i64,
    pub week_52_high: i64,
    pub week_52_low: i64,
    pub best_5_buy: Vec<DepthEntry>,
    pub best_5_sell: Vec<DepthEntry>,
}

/// A single price level in the order book depth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthEntry {
    pub flag: u16,
    pub qty: i64,
    pub price: i64,
    pub num_orders: u16,
}

// ---------------------------------------------------------------------------
// Full Parsed Packet (pre-normalization)
// ---------------------------------------------------------------------------

/// The complete parsed representation of one Angel One WebSocket binary packet.
/// The [`Tick`] is extracted from this during the normalization step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedPacket {
    pub mode: SubscriptionMode,
    pub exchange: ExchangeType,
    pub token: String,
    pub sequence_number: i64,
    pub exchange_timestamp: i64,
    pub last_traded_price: i64,

    /// Present when mode is Quote or SnapQuote.
    pub quote: Option<QuoteData>,
    /// Present only when mode is SnapQuote.
    pub snap: Option<SnapQuoteData>,
}

impl ParsedPacket {
    /// Convert the raw parsed packet into the normalized [`Tick`] schema.
    ///
    /// The token string is parsed as an `i32` instrument ID.
    /// Price is converted from paise (integer) to ₹ (f64).
    ///
    /// `ts_recv_ns` is the caller's wall-clock receive time (nanoseconds since
    /// epoch) for this packet. It is metadata only — captured at the ingestion
    /// boundary for latency diagnostics — and must never feed trading logic
    /// (see ADR-005; business logic uses the injected/exchange clock only).
    pub fn to_tick(&self, ts_recv_ns: i64) -> Tick {
        let inst_id = self.token.parse::<i32>().unwrap_or(0);
        Tick {
            ts_ns: self.exchange_timestamp * 1_000_000, // ms → ns
            inst_id,
            side: 0, // Side is not directly in the binary packet; set to 0 (trade)
            price: self.last_traded_price as f64 / 100.0,
            qty: self
                .quote
                .as_ref()
                .map(|q| q.last_traded_qty)
                .unwrap_or(0),
            seq_no: self.sequence_number,
            // Angel One SnapQuote depth: best_5_buy[0] is the *highest* bid
            // (buyers pay up to this price) and best_5_sell[0] is the *lowest*
            // ask (sellers accept down to this price).
            // However, empirical data shows best_5_buy[0].price > best_5_sell[0].price
            // which means the mapping is inverted relative to the flag names:
            // flag=0 entries are actually asks (sell side, ascending) and
            // flag=1 entries are bids (buy side, descending).
            // Fix: treat best_5_buy[0] as ask and best_5_sell[0] as bid.
            best_bid_price: self
                .snap
                .as_ref()
                .and_then(|s| s.best_5_sell.first())
                .map(|d| d.price as f64 / 100.0)
                .unwrap_or(0.0),
            best_bid_qty: self
                .snap
                .as_ref()
                .and_then(|s| s.best_5_sell.first())
                .map(|d| d.qty)
                .unwrap_or(0),
            best_ask_price: self
                .snap
                .as_ref()
                .and_then(|s| s.best_5_buy.first())
                .map(|d| d.price as f64 / 100.0)
                .unwrap_or(0.0),
            best_ask_qty: self
                .snap
                .as_ref()
                .and_then(|s| s.best_5_buy.first())
                .map(|d| d.qty)
                .unwrap_or(0),
            // --- L2 depth levels 2..5 ---
            // Same inversion: bid ladder = best_5_sell, ask ladder = best_5_buy.
            // Missing levels default to 0.0 / 0.
            bid_price_2: self.snap.as_ref().and_then(|s| s.best_5_sell.get(1)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            bid_qty_2: self.snap.as_ref().and_then(|s| s.best_5_sell.get(1)).map(|d| d.qty).unwrap_or(0),
            bid_price_3: self.snap.as_ref().and_then(|s| s.best_5_sell.get(2)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            bid_qty_3: self.snap.as_ref().and_then(|s| s.best_5_sell.get(2)).map(|d| d.qty).unwrap_or(0),
            bid_price_4: self.snap.as_ref().and_then(|s| s.best_5_sell.get(3)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            bid_qty_4: self.snap.as_ref().and_then(|s| s.best_5_sell.get(3)).map(|d| d.qty).unwrap_or(0),
            bid_price_5: self.snap.as_ref().and_then(|s| s.best_5_sell.get(4)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            bid_qty_5: self.snap.as_ref().and_then(|s| s.best_5_sell.get(4)).map(|d| d.qty).unwrap_or(0),
            ask_price_2: self.snap.as_ref().and_then(|s| s.best_5_buy.get(1)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            ask_qty_2: self.snap.as_ref().and_then(|s| s.best_5_buy.get(1)).map(|d| d.qty).unwrap_or(0),
            ask_price_3: self.snap.as_ref().and_then(|s| s.best_5_buy.get(2)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            ask_qty_3: self.snap.as_ref().and_then(|s| s.best_5_buy.get(2)).map(|d| d.qty).unwrap_or(0),
            ask_price_4: self.snap.as_ref().and_then(|s| s.best_5_buy.get(3)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            ask_qty_4: self.snap.as_ref().and_then(|s| s.best_5_buy.get(3)).map(|d| d.qty).unwrap_or(0),
            ask_price_5: self.snap.as_ref().and_then(|s| s.best_5_buy.get(4)).map(|d| d.price as f64 / 100.0).unwrap_or(0.0),
            ask_qty_5: self.snap.as_ref().and_then(|s| s.best_5_buy.get(4)).map(|d| d.qty).unwrap_or(0),

            ts_recv_ns,
            exchange_type: self.exchange as u8 as i16,

            volume: self.quote.as_ref().map(|q| q.volume).unwrap_or(0),
            avg_traded_price: self.quote.as_ref().map(|q| q.avg_traded_price as f64 / 100.0).unwrap_or(0.0),
            total_buy_qty: self.quote.as_ref().map(|q| q.total_buy_qty).unwrap_or(0.0),
            total_sell_qty: self.quote.as_ref().map(|q| q.total_sell_qty).unwrap_or(0.0),
            open: self.quote.as_ref().map(|q| q.open as f64 / 100.0).unwrap_or(0.0),
            high: self.quote.as_ref().map(|q| q.high as f64 / 100.0).unwrap_or(0.0),
            low: self.quote.as_ref().map(|q| q.low as f64 / 100.0).unwrap_or(0.0),
            close: self.quote.as_ref().map(|q| q.close as f64 / 100.0).unwrap_or(0.0),

            last_trade_ts_ns: self.snap.as_ref().map(|s| s.last_traded_timestamp * 1_000_000).unwrap_or(0),
            open_interest: self.snap.as_ref().map(|s| s.open_interest).unwrap_or(0),
            oi_change_pct_raw: self.snap.as_ref().map(|s| s.oi_change_pct).unwrap_or(0),
            upper_circuit: self.snap.as_ref().map(|s| s.upper_circuit as f64 / 100.0).unwrap_or(0.0),
            lower_circuit: self.snap.as_ref().map(|s| s.lower_circuit as f64 / 100.0).unwrap_or(0.0),
            week_52_high: self.snap.as_ref().map(|s| s.week_52_high as f64 / 100.0).unwrap_or(0.0),
            week_52_low: self.snap.as_ref().map(|s| s.week_52_low as f64 / 100.0).unwrap_or(0.0),

            // Same documented inversion as the price/qty ladders: bid counts
            // come from best_5_sell, ask counts come from best_5_buy.
            best_bid_num_orders: self.snap.as_ref().and_then(|s| s.best_5_sell.first()).map(|d| d.num_orders as i32).unwrap_or(0),
            best_ask_num_orders: self.snap.as_ref().and_then(|s| s.best_5_buy.first()).map(|d| d.num_orders as i32).unwrap_or(0),
            bid_num_orders_2: self.snap.as_ref().and_then(|s| s.best_5_sell.get(1)).map(|d| d.num_orders as i32).unwrap_or(0),
            bid_num_orders_3: self.snap.as_ref().and_then(|s| s.best_5_sell.get(2)).map(|d| d.num_orders as i32).unwrap_or(0),
            bid_num_orders_4: self.snap.as_ref().and_then(|s| s.best_5_sell.get(3)).map(|d| d.num_orders as i32).unwrap_or(0),
            bid_num_orders_5: self.snap.as_ref().and_then(|s| s.best_5_sell.get(4)).map(|d| d.num_orders as i32).unwrap_or(0),
            ask_num_orders_2: self.snap.as_ref().and_then(|s| s.best_5_buy.get(1)).map(|d| d.num_orders as i32).unwrap_or(0),
            ask_num_orders_3: self.snap.as_ref().and_then(|s| s.best_5_buy.get(2)).map(|d| d.num_orders as i32).unwrap_or(0),
            ask_num_orders_4: self.snap.as_ref().and_then(|s| s.best_5_buy.get(3)).map(|d| d.num_orders as i32).unwrap_or(0),
            ask_num_orders_5: self.snap.as_ref().and_then(|s| s.best_5_buy.get(4)).map(|d| d.num_orders as i32).unwrap_or(0),
        }
    }
}

// ---------------------------------------------------------------------------
// Circuit Breaker Messages
// ---------------------------------------------------------------------------

/// PnL/heartbeat message sent from the ingestion node to the circuit breaker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnlMessage {
    /// If true, this message serves as a heartbeat to the watchdog.
    #[serde(default)]
    pub heartbeat: bool,
    /// Cumulative PnL of the current session.
    #[serde(default)]
    pub pnl: f64,
    /// Unix timestamp of the message.
    #[serde(default)]
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn depth(price: i64, qty: i64) -> DepthEntry {
        DepthEntry { flag: 0, qty, price, num_orders: 1 }
    }

    fn snap(best_5_buy: Vec<DepthEntry>, best_5_sell: Vec<DepthEntry>) -> SnapQuoteData {
        SnapQuoteData {
            last_traded_timestamp: 0,
            open_interest: 0,
            oi_change_pct: 0,
            upper_circuit: 0,
            lower_circuit: 0,
            week_52_high: 0,
            week_52_low: 0,
            best_5_buy,
            best_5_sell,
        }
    }

    fn packet(snap: SnapQuoteData) -> ParsedPacket {
        ParsedPacket {
            mode: SubscriptionMode::SnapQuote,
            exchange: ExchangeType::NseCm,
            token: "1594".to_string(),
            sequence_number: 7,
            exchange_timestamp: 1_700_000_000_000, // ms
            last_traded_price: 9998,
            quote: None,
            snap: Some(snap),
        }
    }

    #[test]
    fn to_tick_populates_all_five_levels_with_inversion() {
        // ASK ladder lives in best_5_buy (ascending); BID ladder in best_5_sell
        // (descending) — the documented Angel One inversion.
        let t = packet(snap(
            vec![depth(10000, 1), depth(10005, 2), depth(10010, 3), depth(10015, 4), depth(10020, 5)],
            vec![depth(9995, 10), depth(9990, 20), depth(9985, 30), depth(9980, 40), depth(9975, 50)],
        ))
        .to_tick(0);

        // L1 (inverted).
        assert_eq!(t.best_bid_price, 99.95);
        assert_eq!(t.best_bid_qty, 10);
        assert_eq!(t.best_ask_price, 100.00);
        assert_eq!(t.best_ask_qty, 1);

        // L2..L5 laddered correctly.
        assert_eq!(t.bid_price_2, 99.90);
        assert_eq!(t.bid_price_5, 99.75);
        assert_eq!(t.bid_qty_5, 50);
        assert_eq!(t.ask_price_2, 100.05);
        assert_eq!(t.ask_price_5, 100.20);
        assert_eq!(t.ask_qty_5, 5);

        assert_eq!(t.inst_id, 1594);
        assert_eq!(t.seq_no, 7);
    }

    #[test]
    fn to_tick_zeroes_missing_levels() {
        // 2 ask levels, 1 bid level → deeper levels must be zero.
        let t = packet(snap(
            vec![depth(10000, 1), depth(10005, 2)],
            vec![depth(9995, 10)],
        ))
        .to_tick(0);

        assert_eq!(t.best_ask_price, 100.00);
        assert_eq!(t.ask_price_2, 100.05);
        assert_eq!(t.ask_price_3, 0.0); // missing
        assert_eq!(t.best_bid_price, 99.95);
        assert_eq!(t.bid_price_2, 0.0); // missing
    }

    #[test]
    fn to_tick_captures_recv_ts_and_extended_quote_snap_fields() {
        let pkt = ParsedPacket {
            mode: SubscriptionMode::SnapQuote,
            exchange: ExchangeType::NseFo,
            token: "62329".to_string(),
            sequence_number: 42,
            exchange_timestamp: 1_700_000_000_000, // ms
            last_traded_price: 2_350_000, // ₹23,500.00
            quote: Some(QuoteData {
                last_traded_qty: 75,
                avg_traded_price: 2_349_500,
                volume: 1_234_567,
                total_buy_qty: 100_000.0,
                total_sell_qty: 90_000.0,
                open: 2_340_000,
                high: 2_360_000,
                low: 2_330_000,
                close: 2_335_000,
            }),
            snap: Some(SnapQuoteData {
                last_traded_timestamp: 1_700_000_001_000,
                open_interest: 5_000_000,
                oi_change_pct: -250,
                upper_circuit: 2_500_000,
                lower_circuit: 2_100_000,
                week_52_high: 2_600_000,
                week_52_low: 1_900_000,
                best_5_buy: vec![depth(2_350_500, 1)], // ask ladder (inverted)
                best_5_sell: vec![DepthEntry { flag: 1, qty: 10, price: 2_349_500, num_orders: 7 }], // bid ladder
            }),
        };

        // Wall-clock receive time is caller-supplied metadata, not derived.
        let t = pkt.to_tick(9_999_999_999);
        assert_eq!(t.ts_recv_ns, 9_999_999_999);
        assert_eq!(t.exchange_type, ExchangeType::NseFo as u8 as i16);

        // Quote extension — previously parsed and silently discarded.
        assert_eq!(t.volume, 1_234_567);
        assert!((t.avg_traded_price - 23_495.00).abs() < 1e-9);
        assert_eq!(t.total_buy_qty, 100_000.0);
        assert_eq!(t.total_sell_qty, 90_000.0);
        assert!((t.open - 23_400.00).abs() < 1e-9);
        assert!((t.high - 23_600.00).abs() < 1e-9);
        assert!((t.low - 23_300.00).abs() < 1e-9);
        assert!((t.close - 23_350.00).abs() < 1e-9);

        // SnapQuote extension.
        assert_eq!(t.last_trade_ts_ns, 1_700_000_001_000 * 1_000_000);
        assert_eq!(t.open_interest, 5_000_000);
        assert_eq!(t.oi_change_pct_raw, -250); // stored raw, not rescaled
        assert!((t.upper_circuit - 25_000.00).abs() < 1e-9);
        assert!((t.lower_circuit - 21_000.00).abs() < 1e-9);
        assert!((t.week_52_high - 26_000.00).abs() < 1e-9);
        assert!((t.week_52_low - 19_000.00).abs() < 1e-9);

        // Depth order counts, same bid/ask inversion as price/qty.
        assert_eq!(t.best_bid_num_orders, 7);
        assert_eq!(t.best_ask_num_orders, 1);
    }
}
