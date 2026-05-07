//! Binary protocol parser for Angel One SmartAPI WebSocket v2.
//!
//! The WebSocket streams market data as **Little-Endian binary packets**.
//! This module is a direct port of the official Python SDK's
//! `_parse_binary_data()` method from `smartWebSocketV2.py`.
//!
//! # Packet Layout
//!
//! | Offset | Size | Type    | Field                |
//! |--------|------|---------|----------------------|
//! | 0      | 1    | u8      | subscription_mode    |
//! | 1      | 1    | u8      | exchange_type        |
//! | 2      | 25   | [u8;25] | token (null-term)    |
//! | 27     | 8    | i64     | sequence_number      |
//! | 35     | 8    | i64     | exchange_timestamp   |
//! | 43     | 8    | i64     | last_traded_price    |
//! | 51+    | var  | …       | Quote / SnapQuote    |

use crate::schema::{
    DepthEntry, ExchangeType, ParsedPacket, QuoteData, SnapQuoteData, SubscriptionMode,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Packet too short: expected at least {expected} bytes, got {actual}")]
    TooShort { expected: usize, actual: usize },

    #[error("Unknown subscription mode: {0}")]
    UnknownMode(u8),

    #[error("Unknown exchange type: {0}")]
    UnknownExchange(u8),
}

/// Minimum packet size: mode(1) + exchange(1) + token(25) + seq(8) + ts(8) + ltp(8) = 51
const MIN_PACKET_LEN: usize = 51;

/// Quote mode extends to byte 123.
const QUOTE_PACKET_LEN: usize = 123;

/// SnapQuote mode extends to byte 379.
const SNAP_QUOTE_PACKET_LEN: usize = 379;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a raw binary WebSocket frame into a [`ParsedPacket`].
///
/// Returns `Err` if the buffer is too short or contains unknown enum values.
pub fn parse_binary_packet(data: &[u8]) -> Result<ParsedPacket, ParseError> {
    if data.len() < MIN_PACKET_LEN {
        return Err(ParseError::TooShort {
            expected: MIN_PACKET_LEN,
            actual: data.len(),
        });
    }

    let mode_raw = data[0];
    let exchange_raw = data[1];

    let mode = SubscriptionMode::from_u8(mode_raw)
        .ok_or(ParseError::UnknownMode(mode_raw))?;
    let exchange = ExchangeType::from_u8(exchange_raw)
        .ok_or(ParseError::UnknownExchange(exchange_raw))?;

    let token = parse_token(&data[2..27]);
    let sequence_number = read_i64_le(data, 27);
    let exchange_timestamp = read_i64_le(data, 35);
    let last_traded_price = read_i64_le(data, 43);

    // -- Quote fields (mode 2 or 3) --
    let quote = if matches!(mode, SubscriptionMode::Quote | SubscriptionMode::SnapQuote)
        && data.len() >= QUOTE_PACKET_LEN
    {
        Some(QuoteData {
            last_traded_qty: read_i64_le(data, 51),
            avg_traded_price: read_i64_le(data, 59),
            volume: read_i64_le(data, 67),
            total_buy_qty: read_f64_le(data, 75),
            total_sell_qty: read_f64_le(data, 83),
            open: read_i64_le(data, 91),
            high: read_i64_le(data, 99),
            low: read_i64_le(data, 107),
            close: read_i64_le(data, 115),
        })
    } else {
        None
    };

    // -- SnapQuote-only fields (mode 3) --
    let snap = if mode == SubscriptionMode::SnapQuote && data.len() >= SNAP_QUOTE_PACKET_LEN {
        let best_5 = parse_best_5(&data[147..347]);
        Some(SnapQuoteData {
            last_traded_timestamp: read_i64_le(data, 123),
            open_interest: read_i64_le(data, 131),
            oi_change_pct: read_i64_le(data, 139),
            upper_circuit: read_i64_le(data, 347),
            lower_circuit: read_i64_le(data, 355),
            week_52_high: read_i64_le(data, 363),
            week_52_low: read_i64_le(data, 371),
            best_5_buy: best_5.0,
            best_5_sell: best_5.1,
        })
    } else {
        None
    };

    Ok(ParsedPacket {
        mode,
        exchange,
        token,
        sequence_number,
        exchange_timestamp,
        last_traded_price,
        quote,
        snap,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse the 25-byte null-terminated ASCII token string.
fn parse_token(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Read a Little-Endian `i64` from `data[offset..offset+8]`.
#[inline]
fn read_i64_le(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .expect("slice length mismatch for i64"),
    )
}

/// Read a Little-Endian `f64` from `data[offset..offset+8]`.
#[inline]
fn read_f64_le(data: &[u8], offset: usize) -> f64 {
    f64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .expect("slice length mismatch for f64"),
    )
}

/// Read a Little-Endian `u16` from `data[offset..offset+2]`.
#[inline]
fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        data[offset..offset + 2]
            .try_into()
            .expect("slice length mismatch for u16"),
    )
}

/// Parse the best-5 buy and sell depth entries from a 200-byte slice.
///
/// The slice contains 10 × 20-byte records. Each record:
/// - flag (u16, 2 bytes): 0 = buy, 1 = sell
/// - qty  (i64, 8 bytes)
/// - price (i64, 8 bytes)
/// - num_orders (u16, 2 bytes)
fn parse_best_5(data: &[u8]) -> (Vec<DepthEntry>, Vec<DepthEntry>) {
    let mut buys = Vec::with_capacity(5);
    let mut sells = Vec::with_capacity(5);

    for chunk in data.chunks_exact(20) {
        let entry = DepthEntry {
            flag: read_u16_le(chunk, 0),
            qty: read_i64_le(chunk, 2),
            price: read_i64_le(chunk, 10),
            num_orders: read_u16_le(chunk, 18),
        };
        if entry.flag == 0 {
            buys.push(entry);
        } else {
            sells.push(entry);
        }
    }

    (buys, sells)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal LTP packet (51 bytes) with known values.
    fn make_ltp_packet() -> Vec<u8> {
        let mut buf = vec![0u8; 51];

        // mode = LTP (1)
        buf[0] = 1;
        // exchange = NSE_FO (2)
        buf[1] = 2;
        // token = "26009" (null-padded to 25 bytes)
        let token = b"26009";
        buf[2..2 + token.len()].copy_from_slice(token);
        // sequence_number = 1001 (i64 LE at offset 27)
        buf[27..35].copy_from_slice(&1001i64.to_le_bytes());
        // exchange_timestamp = 1700000000000 (i64 LE at offset 35)
        buf[35..43].copy_from_slice(&1_700_000_000_000i64.to_le_bytes());
        // last_traded_price = 24550 (i64 LE at offset 43) → ₹245.50
        buf[43..51].copy_from_slice(&24550i64.to_le_bytes());

        buf
    }

    #[test]
    fn test_parse_ltp_packet() {
        let data = make_ltp_packet();
        let pkt = parse_binary_packet(&data).unwrap();

        assert_eq!(pkt.mode, SubscriptionMode::Ltp);
        assert_eq!(pkt.exchange, ExchangeType::NseFo);
        assert_eq!(pkt.token, "26009");
        assert_eq!(pkt.sequence_number, 1001);
        assert_eq!(pkt.exchange_timestamp, 1_700_000_000_000);
        assert_eq!(pkt.last_traded_price, 24550);
        assert!(pkt.quote.is_none());
        assert!(pkt.snap.is_none());
    }

    #[test]
    fn test_to_tick_conversion() {
        let data = make_ltp_packet();
        let pkt = parse_binary_packet(&data).unwrap();
        let tick = pkt.to_tick();

        assert_eq!(tick.inst_id, 26009);
        assert!((tick.price - 245.50).abs() < f64::EPSILON);
        assert_eq!(tick.ts_ns, 1_700_000_000_000 * 1_000_000);
        assert_eq!(tick.seq_no, 1001);
        assert_eq!(tick.side, 0);
    }

    #[test]
    fn test_packet_too_short() {
        let data = vec![0u8; 10];
        let err = parse_binary_packet(&data).unwrap_err();
        assert!(matches!(err, ParseError::TooShort { .. }));
    }

    #[test]
    fn test_unknown_mode() {
        let mut data = make_ltp_packet();
        data[0] = 99; // invalid mode
        let err = parse_binary_packet(&data).unwrap_err();
        assert!(matches!(err, ParseError::UnknownMode(99)));
    }
}
