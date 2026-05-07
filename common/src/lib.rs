//! # Common
//!
//! Shared types, enums, and binary protocol parser for the Phase 0 pipeline.
//! All incoming broker data is normalized to the [`Tick`] schema before moving
//! downstream to storage or analytics.

pub mod parser;
pub mod schema;

pub use parser::parse_binary_packet;
pub use schema::{ExchangeType, PnlMessage, SubscriptionMode, Tick};
