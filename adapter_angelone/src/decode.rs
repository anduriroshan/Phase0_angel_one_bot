//! Binary frame decoder: Angel One SnapQuote Ã¢â€ â€™ NautilusTrader types.
//!
//! The Angel One WebSocket sends 379-byte Little-Endian binary frames for
//! SnapQuote (mode 3). This module decodes those frames into:
//! - [`QuoteTick`] Ã¢â‚¬â€ top-of-book bid/ask (best_5_buy[0] / best_5_sell[0])
//! - [`OrderBookDeltas`] Ã¢â‚¬â€ full top-5 snapshot as a Clear + 10 Add deltas
//!
//! ## Price convention
//! Angel One sends prices as integer paise (Ã¢â€šÂ¹ Ãƒâ€” 100).
//! `Price::from_raw(price_paise, 2)` maps directly: raw=24550, precision=2 Ã¢â€ â€™ Ã¢â€šÂ¹245.50.
//!
//! ## Snapshot Ã¢â€ â€™ delta conversion
//! SnapQuote sends full snapshots, not deltas. We convert each packet into:
//! 1. One `Clear` delta to wipe the old book state.
//! 2. Up to 5 `Add` deltas for bid levels (bestÃ¢â€ â€™worst, index 0 = best).
//! 3. Up to 5 `Add` deltas for ask levels (bestÃ¢â€ â€™worst, index 0 = best).
//! The last delta carries `RecordFlag::F_LAST | RecordFlag::F_SNAPSHOT | RecordFlag::F_MBP`.
//!
//! See: `domain/exchange_protocols.md` (SnapQuote packet layout)

use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{
        OrderBookDelta, OrderBookDeltas, QuoteTick,
        order::BookOrder,
    },
    enums::{BookAction, OrderSide, RecordFlag},
    identifiers::InstrumentId,
    types::{Price, Quantity},
};

use common::schema::{ParsedPacket, DepthEntry};

// ---------------------------------------------------------------------------
// Decode errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("Packet too short: expected {expected}, got {actual}")]
    TooShort { expected: usize, actual: usize },
    #[error("No SnapQuote depth data in packet (mode {mode})")]
    NoDepthData { mode: u8 },
    #[error("No bid levels in SnapQuote packet (instrument {inst_id})")]
    NoBidLevels { inst_id: u32 },
    #[error("No ask levels in SnapQuote packet (instrument {inst_id})")]
    NoAskLevels { inst_id: u32 },
}

// ---------------------------------------------------------------------------
// Public decode functions
// ---------------------------------------------------------------------------

/// Decode a parsed Angel One packet into a [`QuoteTick`].
///
/// Uses `best_5_sell[0]` as bid and `best_5_buy[0]` as ask.
///
/// Despite the flag names, empirical Angel One data shows best_5_buy[0] > best_5_sell[0]
/// which means flag=0 entries are the ask side (ascending) and flag=1 entries are the
/// bid side (descending). Using best_5_sell[0] as bid gives bid < ask (uncrossed book).
/// Returns `None` if no depth data is available (e.g., index tokens with qty=0).
pub fn packet_to_quote_tick(
    packet: &ParsedPacket,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<Option<QuoteTick>, DecodeError> {
    let snap = match &packet.snap {
        Some(s) => s,
        None => return Ok(None),
    };

    // Empirically: best_5_buy[0] > best_5_sell[0] — flag names are inverted.
    // best_5_sell[0] is the best bid (highest price a buyer will pay).
    // best_5_buy[0] is the best ask (lowest price a seller will accept).
    let bid = snap.best_5_sell.first();
    let ask = snap.best_5_buy.first();

    let (bid, ask) = match (bid, ask) {
        (Some(b), Some(a)) => (b, a),
        _ => return Ok(None),
    };

    // Skip levels where price or qty is zero (common for index tokens).
    if bid.price == 0 && ask.price == 0 {
        return Ok(None);
    }

    // Price: raw = paise, precision = 2  Ã¢â€ â€™  Ã¢â€šÂ¹ = raw / 100.
    let bid_price = Price::from_raw(bid.price, 2);
    let ask_price = Price::from_raw(ask.price, 2);

    // Quantity: lots (integer), precision = 0.
    let bid_size = Quantity::from_raw(bid.qty as u64, 0);
    let ask_size = Quantity::from_raw(ask.qty as u64, 0);

    let ts_event = UnixNanos::from(packet.exchange_timestamp as u64 * 1_000_000);

    let tick = QuoteTick::new(
        instrument_id,
        bid_price,
        ask_price,
        bid_size,
        ask_size,
        ts_event,
        ts_init,
    );

    Ok(Some(tick))
}

/// Decode a parsed Angel One SnapQuote packet into [`OrderBookDeltas`].
///
/// Emits:
/// 1. A `Clear` delta (wipe old state Ã¢â‚¬â€ SnapQuote is always a full snapshot).
/// 2. Up to 5 `Add` deltas for the bid side.
/// 3. Up to 5 `Add` deltas for the ask side.
///
/// The last delta carries `F_LAST | F_SNAPSHOT | F_MBP` flags.
/// Returns `None` if there is no depth data.
pub fn packet_to_order_book_deltas(
    packet: &ParsedPacket,
    instrument_id: InstrumentId,
    last_seq_no: &mut i64,
    ts_init: UnixNanos,
) -> Result<Option<OrderBookDeltas>, DecodeError> {
    let snap = match &packet.snap {
        Some(s) => s,
        None => return Ok(None),
    };

    let seq = packet.sequence_number as u64;
    let ts_event = UnixNanos::from(packet.exchange_timestamp as u64 * 1_000_000);

    // Gap detection: warn if sequence number is non-monotonic.
    let prev_seq = *last_seq_no;
    if prev_seq != 0 && packet.sequence_number != prev_seq + 1 {
        tracing::warn!(
            instrument_id = %instrument_id,
            prev_seq = prev_seq,
            curr_seq = packet.sequence_number,
            "SnapQuote sequence gap detected Ã¢â‚¬â€ SnapQuote is full snapshot so book is still valid"
        );
    }
    *last_seq_no = packet.sequence_number;

    let mut deltas: Vec<OrderBookDelta> = Vec::with_capacity(11);

    // 1. Clear delta Ã¢â‚¬â€ always first for full-snapshot feeds.
    deltas.push(OrderBookDelta::clear(instrument_id, seq, ts_event, ts_init));

    // 2. Bid (buy) levels Ã¢â‚¬â€ index 0 = best bid.
    let non_empty_bids: Vec<&DepthEntry> = snap
        .best_5_buy
        .iter()
        .filter(|e| e.price > 0 && e.qty > 0)
        .collect();

    let non_empty_asks: Vec<&DepthEntry> = snap
        .best_5_sell
        .iter()
        .filter(|e| e.price > 0 && e.qty > 0)
        .collect();

    let total_levels = non_empty_bids.len() + non_empty_asks.len();
    if total_levels == 0 {
        // No usable levels Ã¢â‚¬â€ this happens for index tokens (qty always 0).
        return Ok(None);
    }

    let _last_level_idx = total_levels; // We'll mark the last add delta.

    for (i, entry) in non_empty_bids.iter().enumerate() {
        let is_last = non_empty_asks.is_empty() && i == non_empty_bids.len() - 1;
        let flags = snapshot_flags(is_last);
        let order = BookOrder::new(
            OrderSide::Buy,
            Price::from_raw(entry.price, 2),
            Quantity::from_raw(entry.qty as u64, 0),
            i as u64, // order_id: use level index (aggregated book, no real IDs)
        );
        deltas.push(OrderBookDelta::new(
            instrument_id,
            BookAction::Add,
            order,
            flags,
            seq,
            ts_event,
            ts_init,
        ));
    }

    for (i, entry) in non_empty_asks.iter().enumerate() {
        let is_last = i == non_empty_asks.len() - 1;
        let flags = snapshot_flags(is_last);
        let order = BookOrder::new(
            OrderSide::Sell,
            Price::from_raw(entry.price, 2),
            Quantity::from_raw(entry.qty as u64, 0),
            (non_empty_bids.len() + i) as u64,
        );
        deltas.push(OrderBookDelta::new(
            instrument_id,
            BookAction::Add,
            order,
            flags,
            seq,
            ts_event,
            ts_init,
        ));
    }

    Ok(Some(OrderBookDeltas::new(instrument_id, deltas)))
}

/// Build the flags byte for the last delta in a SnapQuote snapshot.
fn snapshot_flags(is_last: bool) -> u8 {
    let mut flags = RecordFlag::F_SNAPSHOT as u8 | RecordFlag::F_MBP as u8;
    if is_last {
        flags |= RecordFlag::F_LAST as u8;
    }
    flags
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use common::schema::{
        DepthEntry, ExchangeType, ParsedPacket, QuoteData, SnapQuoteData, SubscriptionMode,
    };
    use nautilus_model::identifiers::{InstrumentId, Symbol, Venue};

    fn make_instrument() -> InstrumentId {
        InstrumentId::new(Symbol::new("NIFTY50-INDEX"), Venue::new("NSE"))
    }

    fn make_packet_with_depth() -> ParsedPacket {
        ParsedPacket {
            mode: SubscriptionMode::SnapQuote,
            exchange: ExchangeType::NseCm,
            token: "26009".to_string(),
            sequence_number: 1,
            exchange_timestamp: 1_700_000_000_000, // ms
            last_traded_price: 2_350_050,
            quote: Some(QuoteData {
                last_traded_qty: 10,
                avg_traded_price: 2_350_000,
                volume: 50_000,
                total_buy_qty: 1_000.0,
                total_sell_qty: 900.0,
                open: 2_340_000,
                high: 2_360_000,
                low: 2_335_000,
                close: 2_345_000,
            }),
            snap: Some(SnapQuoteData {
                last_traded_timestamp: 1_700_000_000_000,
                open_interest: 0,
                oi_change_pct: 0,
                upper_circuit: 2_600_000,
                lower_circuit: 2_100_000,
                week_52_high: 2_700_000,
                week_52_low: 1_800_000,
                // In Angel One SnapQuote, flag=0 entries (best_5_buy) are the ASK side
                // (lowest prices sellers will accept), and flag=1 entries (best_5_sell)
                // are the BID side (highest prices buyers will pay).
                // Fixture: bid=23500.00, ask=23501.00 (uncrossed, bid < ask).
                best_5_buy: vec![
                    DepthEntry { flag: 0, qty: 120, price: 2_350_100, num_orders: 4 }, // ask level 1
                    DepthEntry { flag: 0, qty: 250, price: 2_350_150, num_orders: 7 }, // ask level 2
                ],
                best_5_sell: vec![
                    DepthEntry { flag: 1, qty: 100, price: 2_350_000, num_orders: 5 }, // bid level 1
                    DepthEntry { flag: 1, qty: 200, price: 2_349_950, num_orders: 8 }, // bid level 2
                    DepthEntry { flag: 1, qty: 150, price: 2_349_900, num_orders: 3 }, // bid level 3
                ],
            }),
        }
    }

    #[test]
    fn quote_tick_bid_ask_prices() {
        let packet = make_packet_with_depth();
        let inst = make_instrument();
        let ts_init = UnixNanos::from(0);

        let tick = packet_to_quote_tick(&packet, inst, ts_init)
            .expect("decode ok")
            .expect("tick present");

        // bid = best_5_sell[0].price = 2_350_000 paise → ₹23500.00
        assert_eq!(tick.bid_price.raw, 2_350_000);
        assert_eq!(tick.bid_price.precision, 2);

        // ask = best_5_buy[0].price = 2_350_100 paise → ₹23501.00
        assert_eq!(tick.ask_price.raw, 2_350_100);

        assert_eq!(tick.bid_size.raw, 100);
        assert_eq!(tick.ask_size.raw, 120);
        assert_eq!(tick.instrument_id, inst);
    }

    #[test]
    fn quote_tick_none_for_zero_prices() {
        let mut packet = make_packet_with_depth();
        // Set all prices to 0 (index token behaviour).
        if let Some(snap) = packet.snap.as_mut() {
            snap.best_5_buy = vec![DepthEntry { flag: 0, qty: 0, price: 0, num_orders: 0 }];
            snap.best_5_sell = vec![DepthEntry { flag: 1, qty: 0, price: 0, num_orders: 0 }];
        }

        let inst = make_instrument();
        let result = packet_to_quote_tick(&packet, inst, UnixNanos::from(0))
            .expect("decode ok");
        assert!(result.is_none(), "Expected None for zero-price packet");
    }

    #[test]
    fn order_book_deltas_structure() {
        let packet = make_packet_with_depth();
        let inst = make_instrument();
        let mut seq = 0i64;

        let deltas = packet_to_order_book_deltas(&packet, inst, &mut seq, UnixNanos::from(0))
            .expect("decode ok")
            .expect("deltas present");

        // 1 Clear + 3 bids + 2 asks = 6 deltas
        assert_eq!(deltas.deltas.len(), 6);
        assert_eq!(deltas.deltas[0].action, BookAction::Clear);
        assert_eq!(deltas.deltas[1].action, BookAction::Add);
        assert_eq!(deltas.deltas[1].order.side, OrderSide::Buy);
        assert_eq!(deltas.deltas[1].order.price.raw, 2_350_000);
        assert_eq!(deltas.deltas[4].order.side, OrderSide::Sell);
        assert_eq!(deltas.deltas[4].order.price.raw, 2_350_100);

        // Last delta must have F_LAST flag set.
        let last = deltas.deltas.last().unwrap();
        assert!(RecordFlag::F_LAST.matches(last.flags), "Last delta must have F_LAST flag");
        assert!(RecordFlag::F_SNAPSHOT.matches(last.flags));
        assert!(RecordFlag::F_MBP.matches(last.flags));
    }

    #[test]
    fn sequence_gap_logged_but_continues() {
        let mut packet = make_packet_with_depth();
        let inst = make_instrument();
        let mut seq = 5i64; // Simulate seq already at 5.

        packet.sequence_number = 10; // Gap: jumped from 5 to 10.

        // Should succeed despite gap.
        let result = packet_to_order_book_deltas(&packet, inst, &mut seq, UnixNanos::from(0));
        assert!(result.is_ok());
        assert_eq!(seq, 10); // seq updated to current.
    }

    #[test]
    fn no_depth_data_returns_none() {
        let mut packet = make_packet_with_depth();
        packet.snap = None;
        let inst = make_instrument();

        let tick = packet_to_quote_tick(&packet, inst, UnixNanos::from(0))
            .expect("decode ok");
        assert!(tick.is_none());

        let mut seq = 0i64;
        let deltas = packet_to_order_book_deltas(&packet, inst, &mut seq, UnixNanos::from(0))
            .expect("decode ok");
        assert!(deltas.is_none());
    }
}
