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
    pub fn to_tick(&self) -> Tick {
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
