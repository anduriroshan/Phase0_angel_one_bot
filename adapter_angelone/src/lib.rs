//! # adapter_angelone
//!
//! NautilusTrader [`DataClient`] and [`ExecutionClient`] implementations for
//! the Angel One SmartAPI (Indian broker).
//!
//! ## What this crate provides
//! - [`AngelOneDataClient`] — subscribes to the Angel One SnapQuote WebSocket,
//!   decodes binary frames, and publishes [`QuoteTick`] and [`OrderBookDeltas`]
//!   into NautilusTrader's `DataEngine`.
//! - [`AngelOneExecutionClient`] — translates NautilusTrader `SubmitOrder` /
//!   `CancelOrder` commands into Angel One SmartAPI REST calls.
//!
//! See: `adr/ADR-007-nautilus-trader-foundation.md`

pub mod auth;
pub mod config;
pub mod data;
pub mod decode;
pub mod execution;
pub mod factories;

pub use config::AngelOneDataClientConfig;
pub use data::AngelOneDataClient;
pub use execution::{AngelOneExecutionClient, AngelOneExecutionClientConfig, InstrumentMapping};
pub use factories::{
    AngelOneDataClientFactory, AngelOneDataLiveConfig,
    AngelOneExecClientFactory, AngelOneExecLiveConfig,
};
