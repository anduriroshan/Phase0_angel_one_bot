//! Angel One DataClient implementation.
//!
//! [`AngelOneDataClient`] connects to the Angel One SmartStream WebSocket,
//! decodes binary SnapQuote frames, and publishes data into NautilusTrader's
//! data engine via the global `DataEvent` channel.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};

use ahash::AHashSet;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use nautilus_common::{
    clients::DataClient,
    live::{runner::get_data_event_sender, runtime::get_runtime},
    messages::{
        DataEvent,
        data::{
            SubscribeBookDeltas, SubscribeQuotes, UnsubscribeBookDeltas, UnsubscribeQuotes,
        },
    },
};
use nautilus_core::{nanos::UnixNanos, time::get_atomic_clock_realtime};
use nautilus_model::{
    data::{Data, OrderBookDeltas_API},
    identifiers::{ClientId, InstrumentId, Venue},
};
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use common::parse_binary_packet;

use crate::{
    auth::{AuthTokens, authenticate},
    config::AngelOneDataClientConfig,
    decode::{packet_to_order_book_deltas, packet_to_quote_tick},
};

pub struct AngelOneDataClient {
    client_id: ClientId,
    config: AngelOneDataClientConfig,
    data_sender: tokio::sync::mpsc::UnboundedSender<DataEvent>,
    is_connected: AtomicBool,
    cancellation_token: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    auth_tokens: Option<AuthTokens>,
    quote_subs: AHashSet<InstrumentId>,
    book_subs: AHashSet<InstrumentId>,
}
impl AngelOneDataClient {
    pub fn new(config: AngelOneDataClientConfig) -> Self {
        let client_id = config.client_id;
        let data_sender = get_data_event_sender();
        Self {
            client_id,
            config,
            data_sender,
            is_connected: AtomicBool::new(false),
            cancellation_token: CancellationToken::new(),
            tasks: Vec::new(),
            auth_tokens: None,
            quote_subs: AHashSet::new(),
            book_subs: AHashSet::new(),
        }
    }

    fn build_subscribe_payload(&self, exchange_type: u8) -> serde_json::Value {
        let tokens: Vec<String> = self.config.instrument_map.keys().map(|t| t.to_string()).collect();
        json!({ "action": 1, "params": { "mode": 3, "tokenList": [{ "exchangeType": exchange_type, "tokens": tokens }] } })
    }

    fn spawn_ws_task(
        ws_url: String,
        feed_token: String,
        client_id_str: String,
        subscribe_payload: String,
        instrument_map: HashMap<u32, InstrumentId>,
        quote_subs: AHashSet<InstrumentId>,
        book_subs: AHashSet<InstrumentId>,
        data_sender: tokio::sync::mpsc::UnboundedSender<DataEvent>,
        cancellation_token: CancellationToken,
    ) -> JoinHandle<()> {
        get_runtime().spawn(async move {
            if let Err(e) = Self::ws_loop(ws_url, feed_token, client_id_str, subscribe_payload, instrument_map, quote_subs, book_subs, data_sender, cancellation_token).await {
                error!("Angel One WS task exited with error: {e}");
            }
        })
    }

    async fn ws_loop(
        ws_url: String,
        feed_token: String,
        client_id_str: String,
        subscribe_payload: String,
        instrument_map: HashMap<u32, InstrumentId>,
        quote_subs: AHashSet<InstrumentId>,
        book_subs: AHashSet<InstrumentId>,
        data_sender: tokio::sync::mpsc::UnboundedSender<DataEvent>,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<()> {
        let request = {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let mut req = ws_url.as_str().into_client_request()?;
            req.headers_mut().insert("Authorization", feed_token.parse()?);
            req.headers_mut().insert("x-client-code", client_id_str.parse()?);
            req.headers_mut().insert("x-feed-token", feed_token.parse()?);
            req
        };
        info!("Connecting to Angel One SmartStream WebSocket...");
        let (mut ws_stream, _) = connect_async(request).await?;
        info!("Angel One SmartStream WebSocket connected");
        ws_stream.send(Message::Text(subscribe_payload.into())).await?;
        info!("Sent Angel One subscribe payload");
        let mut seq_map: HashMap<u32, i64> = HashMap::new();
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    info!("Angel One WS task cancelled");
                    let _ = ws_stream.close(None).await;
                    return Ok(());
                }
                msg = ws_stream.next() => {
                    match msg {
                        Some(Ok(Message::Binary(bytes))) => {
                            let ts_init = UnixNanos::from(get_atomic_clock_realtime().get_time_ns().as_u64());
                            Self::handle_binary_frame(&bytes, &instrument_map, &quote_subs, &book_subs, &data_sender, &mut seq_map, ts_init);
                        }
                        Some(Ok(Message::Text(text))) => { info!("Angel One WS text: {text}"); }
                        Some(Ok(Message::Ping(ping))) => { let _ = ws_stream.send(Message::Pong(ping)).await; }
                        Some(Ok(Message::Close(frame))) => { warn!("Angel One WS closed: {frame:?}"); return Ok(()); }
                        Some(Err(e)) => { error!("Angel One WS error: {e}"); return Err(e.into()); }
                        None => { warn!("Angel One WS stream ended"); return Ok(()); }
                        _ => {}
                    }
                }
            }
        }
    }

    fn handle_binary_frame(
        bytes: &[u8],
        instrument_map: &HashMap<u32, InstrumentId>,
        quote_subs: &AHashSet<InstrumentId>,
        book_subs: &AHashSet<InstrumentId>,
        data_sender: &tokio::sync::mpsc::UnboundedSender<DataEvent>,
        seq_map: &mut HashMap<u32, i64>,
        ts_init: UnixNanos,
    ) {
        let packet = match parse_binary_packet(bytes) {
            Ok(p) => p,
            Err(e) => { warn!("Failed to parse Angel One binary frame: {e}"); return; }
        };
        let token_num: u32 = match packet.token.trim_matches('\0').parse() {
            Ok(n) => n,
            Err(_) => { warn!("Unparseable Angel One token: {:?}", packet.token); return; }
        };
        let instrument_id = match instrument_map.get(&token_num) {
            Some(id) => *id,
            None => return,
        };
        if quote_subs.contains(&instrument_id) {
            match packet_to_quote_tick(&packet, instrument_id, ts_init) {
                Ok(Some(tick)) => { if let Err(e) = data_sender.send(DataEvent::Data(Data::Quote(tick))) { error!("Failed to send QuoteTick: {e}"); } }
                Ok(None) => {}
                Err(e) => warn!("QuoteTick decode error: {e}"),
            }
        }
        if book_subs.contains(&instrument_id) {
            let seq = seq_map.entry(token_num).or_insert(0);
            match packet_to_order_book_deltas(&packet, instrument_id, seq, ts_init) {
                Ok(Some(deltas)) => {
                    if let Err(e) = data_sender.send(DataEvent::Data(Data::Deltas(OrderBookDeltas_API::new(deltas)))) {
                        error!("Failed to send OrderBookDeltas: {e}");
                    }
                }
                Ok(None) => {}
                Err(e) => warn!("OrderBookDeltas decode error: {e}"),
            }
        }
    }
}

#[async_trait(?Send)]
impl DataClient for AngelOneDataClient {
    fn client_id(&self) -> ClientId { self.client_id }
    fn venue(&self) -> Option<Venue> { Some(self.config.venue) }
    fn start(&mut self) -> anyhow::Result<()> { info!("Angel One DataClient starting"); Ok(()) }
    fn stop(&mut self) -> anyhow::Result<()> {
        info!("Angel One DataClient stopping");
        self.cancellation_token.cancel();
        self.is_connected.store(false, Ordering::Relaxed);
        Ok(())
    }
    fn reset(&mut self) -> anyhow::Result<()> {
        info!("Angel One DataClient resetting");
        self.cancellation_token.cancel();
        for task in self.tasks.drain(..) { task.abort(); }
        self.auth_tokens = None;
        self.is_connected.store(false, Ordering::Relaxed);
        self.cancellation_token = CancellationToken::new();
        Ok(())
    }
    fn dispose(&mut self) -> anyhow::Result<()> { self.stop() }
    fn is_connected(&self) -> bool { self.is_connected.load(Ordering::Relaxed) }
    fn is_disconnected(&self) -> bool { !self.is_connected() }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.is_connected() { return Ok(()); }
        self.cancellation_token = CancellationToken::new();
        let tokens = authenticate().await?;
        self.auth_tokens = Some(tokens.clone());
        let exchange_type = self.config.exchange_type;
        let subscribe_payload = self.build_subscribe_payload(exchange_type).to_string();
        let handle = Self::spawn_ws_task(
            self.config.ws_url.clone(), tokens.feed_token.clone(), tokens.client_id.clone(),
            subscribe_payload, self.config.instrument_map.clone(),
            self.quote_subs.clone(), self.book_subs.clone(),
            self.data_sender.clone(), self.cancellation_token.clone(),
        );
        self.tasks.push(handle);
        self.is_connected.store(true, Ordering::Relaxed);
        info!("Angel One DataClient connected (client_id={})", self.client_id);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> { self.stop() }

    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        let id = cmd.instrument_id;
        self.quote_subs.insert(id);
        info!("Subscribed quotes: {id}");
        Ok(())
    }

    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        self.quote_subs.remove(&cmd.instrument_id);
        Ok(())
    }

    fn subscribe_book_deltas(&mut self, cmd: SubscribeBookDeltas) -> anyhow::Result<()> {
        let id = cmd.instrument_id;
        self.book_subs.insert(id);
        info!("Subscribed book deltas: {id}");
        Ok(())
    }

    fn unsubscribe_book_deltas(&mut self, cmd: &UnsubscribeBookDeltas) -> anyhow::Result<()> {
        self.book_subs.remove(&cmd.instrument_id);
        Ok(())
    }
}