//! # strategy_basis_arb
//!
//! NIFTY futures-vs-spot basis-arb strategy for the Angel One trading system.
//!
//! ## What it does
//! Monitors the rolling z-score of `(NIFTY futures mid − NIFTY spot mid)`.
//! When |z| > threshold:
//! - z > 0 → futures expensive → SELL futures (expect reversion)
//! - z < 0 → futures cheap   → BUY  futures (expect reversion)
//!
//! ## Determinism guarantee
//! - No `SystemTime::now()`, `Instant::now()`, or any wall-clock call.
//! - Same tick stream + same config → bit-identical orders.
//!
//! ## Key types
//! - [`BasisArbStrategy`] — the NautilusTrader `Strategy` impl
//! - [`BasisArbConfig`]   — config loaded from `config/strategy_basis_arb.toml`
//! - [`BasisArbParams`]   — tunable parameters
//! - [`RollingBasis`]     — zero-allocation rolling z-score calculator

pub mod config;
pub mod rolling_basis;
pub mod strategy;

pub use config::{BasisArbConfig, BasisArbParams};
pub use rolling_basis::RollingBasis;
pub use strategy::BasisArbStrategy;
