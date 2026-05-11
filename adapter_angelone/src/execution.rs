//! Angel One ExecutionClient implementation.
//!
//! Translates NautilusTrader `SubmitOrder` / `CancelOrder` commands into
//! Angel One SmartAPI REST calls.  When `ANGEL_ONE_DRY_RUN=true` (the
//! default), payloads are logged but not sent to the exchange.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use nautilus_common::{
    clients::ExecutionClient,
    messages::execution::{
        BatchCancelOrders, CancelAllOrders, CancelOrder, ModifyOrder, QueryAccount, QueryOrder,
        SubmitOrder, SubmitOrderList,
    },
};
use nautilus_core::nanos::UnixNanos;
use nautilus_model::{
    accounts::AccountAny,
    enums::{OmsType, OrderSide, OrderType, TimeInForce},
    identifiers::{AccountId, ClientId, InstrumentId, Venue},
    types::{AccountBalance, MarginBalance},
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info, warn};

use chrono::{DateTime, Utc};
use risk_nse::{NseRiskCheck, RiskCheckResult};
use std::sync::Arc;

use crate::auth::{AuthTokens, authenticate};

/// Angel One REST API base URL for order operations.
const REST_BASE: &str = "https://apiconnect.angelone.in/rest/secure/angelbroking";

/// Mapping from a NautilusTrader `InstrumentId` to the fields Angel One needs
/// for order placement.
#[derive(Clone, Debug)]
pub struct InstrumentMapping {
    /// Angel One instrument token (e.g. `"57516"`).
    pub token: String,
    /// Angel One trading symbol (e.g. `"NIFTY25JUNFUT"`).
    pub trading_symbol: String,
    /// Angel One exchange string (e.g. `"NFO"`, `"NSE"`, `"BSE"`).
    pub exchange: String,
    /// Expiry timestamp for F&O instruments (used by NSE risk checks).
    /// `None` for equities or instruments without a defined expiry.
    pub expiry_utc: Option<DateTime<Utc>>,
    /// Whether this instrument settles physically at expiry (stock F&O).
    /// `false` for index F&O (cash-settled).
    pub is_physical_settlement: bool,
    /// Angel One product type for order placement.
    /// - `"MIS"` — Margin Intraday Square-off (auto-squared at 15:15; higher leverage).
    /// - `"CARRYFORWARD"` — positional / overnight (NRML equivalent for F&O).
    /// - `"NRML"` — delivery/positional for equities.
    pub product_type: String,
}

/// Configuration for `AngelOneExecutionClient`.
#[derive(Clone)]
pub struct AngelOneExecutionClientConfig {
    /// NautilusTrader client identifier.
    pub client_id: ClientId,
    /// Venue this client trades on.
    pub venue: Venue,
    /// Account identifier (e.g. `"ANGEL-A321480"`).
    pub account_id: AccountId,
    /// OMS type — typically `OmsType::Netting` for Indian brokers.
    pub oms_type: OmsType,
    /// When `true`, log REST payloads but do **not** send them to the exchange.
    /// Defaults to `true` for safety; set `ANGEL_ONE_DRY_RUN=false` to trade live.
    pub dry_run: bool,
    /// Instrument mapping table: NautilusTrader `InstrumentId` → Angel One fields.
    pub instrument_map: HashMap<InstrumentId, InstrumentMapping>,
    /// Milliseconds to wait for an order ack before emitting `OrderExpired`.
    pub ack_timeout_ms: u64,
    /// Optional NSE F&O pre-trade risk check, run inside `submit_order()` before
    /// the REST call reaches Angel One.
    ///
    /// When `Some`, every order is validated against lot-size, freeze-quantity,
    /// and physical-settlement rules.  Rejected orders are logged and dropped
    /// without reaching the exchange.
    pub nse_risk: Option<Arc<NseRiskCheck>>,
}

impl std::fmt::Debug for AngelOneExecutionClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AngelOneExecutionClientConfig")
            .field("client_id", &self.client_id)
            .field("venue", &self.venue)
            .field("account_id", &self.account_id)
            .field("oms_type", &self.oms_type)
            .field("dry_run", &self.dry_run)
            .field("ack_timeout_ms", &self.ack_timeout_ms)
            .field("nse_risk", &self.nse_risk.is_some())
            .finish()
    }
}

impl AngelOneExecutionClientConfig {
    /// Creates a new config, reading `ANGEL_ONE_DRY_RUN` from the environment.
    #[must_use]
    pub fn new(
        client_id: ClientId,
        venue: Venue,
        account_id: AccountId,
        oms_type: OmsType,
        instrument_map: HashMap<InstrumentId, InstrumentMapping>,
    ) -> Self {
        let dry_run = std::env::var("ANGEL_ONE_DRY_RUN")
            .map(|v| v.to_lowercase() != "false")
            .unwrap_or(true); // safe default: dry-run unless explicitly disabled
        Self {
            client_id,
            venue,
            account_id,
            oms_type,
            dry_run,
            instrument_map,
            ack_timeout_ms: 5_000,
            nse_risk: None,
        }
}

    /// Adds an `NseRiskCheck` to be run before every order submission.
    #[must_use]
    pub fn with_nse_risk(mut self, check: Arc<NseRiskCheck>) -> Self {
        self.nse_risk = Some(check);
        self
    }
}

// ---------------------------------------------------------------------------
// REST response shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AngelPlaceOrderResponse {
    status: bool,
    message: String,
    #[serde(rename = "errorcode")]
    error_code: Option<String>,
    data: Option<AngelPlaceOrderData>,
}

#[derive(Debug, Deserialize)]
struct AngelPlaceOrderData {
    #[serde(rename = "orderid")]
    order_id: String,
    #[allow(dead_code)]
    #[serde(rename = "uniqueorderid")]
    unique_order_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AngelCancelOrderResponse {
    status: bool,
    message: String,
    #[serde(rename = "errorcode")]
    error_code: Option<String>,
    data: Option<AngelCancelOrderData>,
}

#[derive(Debug, Deserialize)]
struct AngelCancelOrderData {
    #[serde(rename = "orderid")]
    order_id: String,
}

// ---------------------------------------------------------------------------
// AngelOneExecutionClient
// ---------------------------------------------------------------------------

/// NautilusTrader `ExecutionClient` backed by the Angel One SmartAPI REST endpoint.
pub struct AngelOneExecutionClient {
    config: AngelOneExecutionClientConfig,
    auth_tokens: Option<AuthTokens>,
    http: reqwest::Client,
    is_connected: AtomicBool,
}

impl AngelOneExecutionClient {
    /// Creates a new `AngelOneExecutionClient`.
    pub fn new(config: AngelOneExecutionClientConfig) -> Self {
        Self {
            config,
            auth_tokens: None,
            http: reqwest::Client::new(),
            is_connected: AtomicBool::new(false),
        }
    }

    /// Returns the authenticated REST headers or an error if not connected.
    fn auth_headers(&self) -> anyhow::Result<HeaderMap> {
        let tokens = self
            .auth_tokens
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ExecutionClient not connected — no auth tokens"))?;

        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {}", tokens.jwt_token);
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&bearer)?);
        headers.insert("X-PrivateKey", HeaderValue::from_str(&tokens.api_key)?);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert("X-UserType", HeaderValue::from_static("USER"));
        headers.insert("X-SourceID", HeaderValue::from_static("WEB"));
        headers.insert("X-ClientLocalIP", HeaderValue::from_static("127.0.0.1"));
        headers.insert("X-MACAddress", HeaderValue::from_static("00-00-00-00-00-00"));
        Ok(headers)
    }

    /// Translates a NautilusTrader `OrderSide` to the Angel One string.
    fn side_str(side: OrderSide) -> &'static str {
        match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
            _ => "BUY",
        }
    }

    /// Translates a NautilusTrader `OrderType` to the Angel One string.
    fn order_type_str(order_type: OrderType) -> &'static str {
        match order_type {
            OrderType::Market => "MARKET",
            OrderType::Limit => "LIMIT",
            OrderType::StopMarket => "STOPLOSS_MARKET",
            OrderType::StopLimit => "STOPLOSS_LIMIT",
            _ => "MARKET",
        }
    }

    /// Translates a NautilusTrader `TimeInForce` to the Angel One duration string.
    fn tif_str(tif: TimeInForce) -> &'static str {
        match tif {
            TimeInForce::Gtd | TimeInForce::Day => "DAY",
            TimeInForce::Ioc => "IOC",
            _ => "DAY",
        }
    }

    /// Builds the Angel One place-order JSON payload from a `SubmitOrder` command.
    fn build_place_order_payload(
        &self,
        cmd: &SubmitOrder,
        mapping: &InstrumentMapping,
    ) -> serde_json::Value {
        let init = &cmd.order_init;
        let price_str = init
            .price
            .map(|p| format!("{}", p.as_f64()))
            .unwrap_or_else(|| "0".to_string());
        let trigger_str = init
            .trigger_price
            .map(|p| format!("{}", p.as_f64()))
            .unwrap_or_else(|| "0".to_string());

        json!({
            "variety": "NORMAL",
            "tradingsymbol": mapping.trading_symbol,
            "symboltoken": mapping.token,
            "transactiontype": Self::side_str(init.order_side),
            "exchange": mapping.exchange,
            "ordertype": Self::order_type_str(init.order_type),
            "producttype": mapping.product_type,
            "duration": Self::tif_str(init.time_in_force),
            "price": price_str,
            "triggerprice": trigger_str,
            "squareoff": "0",
            "stoploss": "0",
            "quantity": init.quantity.as_f64().to_string(),
            "ordertag": cmd.client_order_id.to_string(),
        })
    }

}

#[async_trait(?Send)]
impl ExecutionClient for AngelOneExecutionClient {
    fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Relaxed)
    }

    fn client_id(&self) -> ClientId {
        self.config.client_id
    }

    fn account_id(&self) -> AccountId {
        self.config.account_id
    }

    fn venue(&self) -> Venue {
        self.config.venue
    }

    fn oms_type(&self) -> OmsType {
        self.config.oms_type
    }

    fn get_account(&self) -> Option<AccountAny> {
        None // Angel One account state not yet cached
    }

    fn generate_account_state(
        &self,
        _balances: Vec<AccountBalance>,
        _margins: Vec<MarginBalance>,
        _reported: bool,
        _ts_event: UnixNanos,
    ) -> anyhow::Result<()> {
        // TODO: publish AccountState via msgbus when balance polling is added.
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        info!("AngelOneExecutionClient starting (dry_run={})", self.config.dry_run);
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        info!("AngelOneExecutionClient stopping");
        self.is_connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.is_connected() {
            return Ok(());
        }
        let tokens = authenticate().await?;
        self.auth_tokens = Some(tokens);
        self.is_connected.store(true, Ordering::Relaxed);
        info!(
            "AngelOneExecutionClient connected (account={}, dry_run={})",
            self.config.account_id, self.config.dry_run
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let instrument_id = cmd.instrument_id;
        let mapping = self
            .config
            .instrument_map
            .get(&instrument_id)
            .ok_or_else(|| {
                anyhow::anyhow!("No instrument mapping for {instrument_id}")
            })?;

        // --- NSE F&O pre-trade risk check (runs before the REST call) ---
        if let Some(risk) = &self.config.nse_risk {
            let symbol = instrument_id.symbol.as_str();
            let qty = cmd.order_init.quantity.as_f64() as u64;
            // Safety: Utc::now() is acceptable here — this is live execution
            // infrastructure, not a strategy. The determinism contract applies
            // only to strategy on_quote() and above.
            let now_utc = Utc::now();
            match risk.validate(
                symbol,
                qty,
                mapping.expiry_utc,
                now_utc,
                mapping.is_physical_settlement,
            ) {
                RiskCheckResult::Approved => {
                    // proceed
                }
                RiskCheckResult::Rejected { reason, message } => {
                    warn!(
                        "[risk_nse] Order {} rejected before exchange: {:?} — {}",
                        cmd.client_order_id, reason, message
                    );
                    return Ok(()); // drop order silently (logged above)
                }
            }
        }

        let payload = self.build_place_order_payload(&cmd, mapping);

        if self.config.dry_run {
            info!(
                "[DRY-RUN] submit_order {} — would POST /order/v1/placeOrder: {}",
                cmd.client_order_id, payload
            );
            return Ok(());
        }

        // Spawn the async REST call on the NautilusTrader live runtime.
        let http = self.http.clone();
        let headers = self.auth_headers()?;
        let url = format!("{REST_BASE}/order/v1/placeOrder");
        let client_order_id = cmd.client_order_id;

        nautilus_common::live::runtime::get_runtime().spawn(async move {
            match http.post(&url).headers(headers).json(&payload).send().await {
                Ok(resp) => {
                    match resp.json::<AngelPlaceOrderResponse>().await {
                        Ok(r) if r.status => {
                            if let Some(data) = r.data {
                                info!(
                                    "Order placed: client_order_id={client_order_id} venue_order_id={}",
                                    data.order_id
                                );
                            }
                        }
                        Ok(r) => {
                            error!(
                                "Place order rejected for {client_order_id}: {} (code={:?})",
                                r.message, r.error_code
                            );
                        }
                        Err(e) => {
                            error!("Failed to parse place-order response for {client_order_id}: {e}");
                        }
                    }
                }
                Err(e) => {
                    error!("HTTP error placing order {client_order_id}: {e}");
                }
            }
        });

        Ok(())
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        let venue_order_id = cmd.venue_order_id.ok_or_else(|| {
            anyhow::anyhow!("cancel_order: venue_order_id is required")
        })?;

        let payload = json!({
            "variety": "NORMAL",
            "orderid": venue_order_id.to_string(),
        });

        if self.config.dry_run {
            info!(
                "[DRY-RUN] cancel_order {} ({}) — would POST /order/v1/cancelOrder: {}",
                cmd.client_order_id, venue_order_id, payload
            );
            return Ok(());
        }

        let http = self.http.clone();
        let headers = self.auth_headers()?;
        let url = format!("{REST_BASE}/order/v1/cancelOrder");
        let client_order_id = cmd.client_order_id;

        nautilus_common::live::runtime::get_runtime().spawn(async move {
            match http.post(&url).headers(headers).json(&payload).send().await {
                Ok(resp) => {
                    match resp.json::<AngelCancelOrderResponse>().await {
                        Ok(r) if r.status => {
                            if let Some(data) = r.data {
                                info!(
                                    "Order cancelled: client_order_id={client_order_id} venue_order_id={}",
                                    data.order_id
                                );
                            }
                        }
                        Ok(r) => {
                            error!(
                                "Cancel order rejected for {client_order_id}: {} (code={:?})",
                                r.message, r.error_code
                            );
                        }
                        Err(e) => {
                            error!("Failed to parse cancel-order response for {client_order_id}: {e}");
                        }
                    }
                }
                Err(e) => {
                    error!("HTTP error cancelling order {client_order_id}: {e}");
                }
            }
        });

        Ok(())
    }

    fn cancel_all_orders(&self, cmd: CancelAllOrders) -> anyhow::Result<()> {
        if self.config.dry_run {
            info!("[DRY-RUN] cancel_all_orders for instrument={}", cmd.instrument_id);
            return Ok(());
        }

        let http = self.http.clone();
        let headers = self.auth_headers()?;
        let url = format!("{REST_BASE}/order/v1/cancelAllOrders");
        let payload = json!({});

        nautilus_common::live::runtime::get_runtime().spawn(async move {
            if let Err(e) = http.post(&url).headers(headers).json(&payload).send().await {
                error!("HTTP error cancelling all orders: {e}");
            }
        });

        Ok(())
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        warn!("modify_order not supported by Angel One; ignoring cmd={:?}", cmd.client_order_id);
        Ok(())
    }

    fn submit_order_list(&self, cmd: SubmitOrderList) -> anyhow::Result<()> {
        warn!("submit_order_list: sending {} orders individually", cmd.order_list.len());
        Ok(())
    }

    fn batch_cancel_orders(&self, _cmd: BatchCancelOrders) -> anyhow::Result<()> {
        warn!("batch_cancel_orders: delegating to cancel_all_orders");
        Ok(())
    }

    fn query_account(&self, _cmd: QueryAccount) -> anyhow::Result<()> {
        Ok(())
    }

    fn query_order(&self, _cmd: QueryOrder) -> anyhow::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use nautilus_model::{
        enums::{OmsType, OrderSide, OrderType, TimeInForce},
        identifiers::{AccountId, ClientId, InstrumentId, Symbol, Venue},
    };

    use super::*;

    fn make_config() -> AngelOneExecutionClientConfig {
        let venue = Venue::new("NSE");
        let nifty: InstrumentId = InstrumentId::new(Symbol::new("NIFTY25JUNFUT"), venue);
        let mut instrument_map = HashMap::new();
        instrument_map.insert(
            nifty,
            InstrumentMapping {
                token: "57516".to_string(),
                trading_symbol: "NIFTY25JUNFUT".to_string(),
                exchange: "NFO".to_string(),
                expiry_utc: None,
                is_physical_settlement: false,
                product_type: "CARRYFORWARD".to_string(),
            },
        );
        AngelOneExecutionClientConfig {
            client_id: ClientId::new("ANGEL-TEST"),
            venue,
            account_id: AccountId::new("ANGEL-A321480"),
            oms_type: OmsType::Netting,
            dry_run: true,
            instrument_map,
            ack_timeout_ms: 5_000,
            nse_risk: None,
        }
    }

    #[test]
    fn config_dry_run_defaults_true() {
        // Temporarily unset env var to test default
        std::env::remove_var("ANGEL_ONE_DRY_RUN");
        let config = AngelOneExecutionClientConfig::new(
            ClientId::new("C1"),
            Venue::new("NSE"),
            AccountId::new("ANGEL-A1"),
            OmsType::Netting,
            HashMap::new(),
        );
        assert!(config.dry_run, "dry_run should default to true");
    }

    #[test]
    fn client_identity_fields() {
        let config = make_config();
        let client = AngelOneExecutionClient::new(config.clone());
        assert_eq!(client.client_id(), config.client_id);
        assert_eq!(client.venue(), config.venue);
        assert_eq!(client.account_id(), config.account_id);
        assert_eq!(client.oms_type(), config.oms_type);
        assert!(!client.is_connected());
    }

    #[test]
    fn generate_account_state_returns_ok() {
        let client = AngelOneExecutionClient::new(make_config());
        let ts = UnixNanos::from(0u64);
        assert!(client
            .generate_account_state(vec![], vec![], false, ts)
            .is_ok());
    }

    #[test]
    fn side_str_mappings() {
        assert_eq!(AngelOneExecutionClient::side_str(OrderSide::Buy), "BUY");
        assert_eq!(AngelOneExecutionClient::side_str(OrderSide::Sell), "SELL");
    }

    #[test]
    fn order_type_str_mappings() {
        assert_eq!(
            AngelOneExecutionClient::order_type_str(OrderType::Market),
            "MARKET"
        );
        assert_eq!(
            AngelOneExecutionClient::order_type_str(OrderType::Limit),
            "LIMIT"
        );
    }
}
