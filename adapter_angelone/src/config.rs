//! Configuration for the Angel One data client.

use nautilus_model::identifiers::{ClientId, InstrumentId, Venue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for [`AngelOneDataClient`].
///
/// Loaded from `config/instruments.toml` at startup. Credentials are read from
/// environment variables at auth time, not from this struct.
#[derive(Debug, Clone)]
pub struct AngelOneDataClientConfig {
    /// NautilusTrader client identifier (e.g. `"ANGEL_ONE"`).
    pub client_id: ClientId,
    /// Venue for market data instruments (e.g. `"NSE"`).
    pub venue: Venue,
    /// Maps Angel One integer token â†’ NautilusTrader InstrumentId.
    /// Populated from `config/instruments.toml`.
    pub instrument_map: HashMap<u32, InstrumentId>,
    /// Angel One exchange type byte for subscriptions:
    /// 1 = NSE_CM (indices, equities), 2 = NSE_FO (derivatives).
    pub exchange_type: u8,
    /// WebSocket URL (defaults to the Angel One SmartStream endpoint).
    pub ws_url: String,
}

impl AngelOneDataClientConfig {
    pub fn new(
        client_id: ClientId,
        venue: Venue,
        instrument_map: HashMap<u32, InstrumentId>,
        exchange_type: u8,
    ) -> Self {
        Self {
            client_id,
            venue,
            instrument_map,
            exchange_type,
            ws_url: "wss://smartapisocket.angelone.in/smart-stream".to_string(),
        }
    }
}

/// A single entry in `config/instruments.toml`.
///
/// Example:
/// ```toml
/// [[instruments]]
/// token = 26009
/// symbol = "NIFTY50-INDEX"
/// venue = "NSE"
/// ```
#[derive(Debug, Deserialize, Serialize)]
pub struct InstrumentEntry {
    pub token: u32,
    pub symbol: String,
    pub venue: String,
}

/// Top-level shape of `config/instruments.toml`.
#[derive(Debug, Deserialize)]
pub struct InstrumentsConfig {
    pub instruments: Vec<InstrumentEntry>,
}

impl InstrumentsConfig {
    /// Parse from a TOML string.
    pub fn from_toml(content: &str) -> anyhow::Result<Self> {
        toml::from_str(content).map_err(|e| anyhow::anyhow!("Failed to parse instruments.toml: {e}"))
    }

    /// Build the token â†’ InstrumentId mapping.
    pub fn into_map(self) -> HashMap<u32, InstrumentId> {
        self.instruments
            .into_iter()
            .filter_map(|entry| {
                let id_str = format!("{}.{}", entry.symbol, entry.venue);
                match id_str.parse::<InstrumentId>() {
                    Ok(id) => Some((entry.token, id)),
                    Err(e) => {
                        tracing::warn!(
                            token = entry.token,
                            id = %id_str,
                            error = %e,
                            "Skipping invalid instrument entry"
                        );
                        None
                    }
                }
            })
            .collect()
    }
}
