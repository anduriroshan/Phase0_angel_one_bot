//! # strategy_intraday_vwap
//!
//! VWAP mean-reversion intraday strategy for NSE equity stocks.
//!
//! ## Signal
//! Enters when the stock price deviates significantly from its session
//! mean (VWAP approximation).  Exits when price reverts or at 14:45 IST.
//!
//! ## Key types
//! - [`IntradayVwapStrategy`] — NautilusTrader `Strategy` impl
//! - [`IntradayVwapConfig`]   — full config (base + params + instrument IDs)
//! - [`IntradayVwapParams`]   — TOML-loadable parameters
//! - [`SessionVwap`]          — per-instrument session state (deterministic)

pub mod config;
pub mod strategy;
pub mod vwap;

pub use config::{IntradayVwapConfig, IntradayVwapParams};
pub use strategy::IntradayVwapStrategy;
pub use vwap::SessionVwap;
