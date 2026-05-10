//! Factory implementations for the NautilusTrader `LiveNodeBuilder`.
//!
//! Wraps `AngelOneDataClientConfig` and `AngelOneExecutionClientConfig` as
//! `ClientConfig` trait objects, then implements the corresponding factory
//! traits so the `LiveNodeBuilder` can instantiate clients from config.
//!
//! # Usage
//! ```rust,ignore
//! use adapter_angelone::factories::{
//!     AngelOneDataClientFactory, AngelOneDataLiveConfig,
//!     AngelOneExecClientFactory, AngelOneExecLiveConfig,
//! };
//!
//! let mut node = LiveNode::builder(trader_id, Environment::Live)?
//!     .add_data_client(None, Box::new(AngelOneDataClientFactory), Box::new(data_cfg))?
//!     .add_exec_client(None, Box::new(AngelOneExecClientFactory), Box::new(exec_cfg))?
//!     .build()?;
//! ```

use std::{any::Any, cell::RefCell, rc::Rc};

use nautilus_common::{
    cache::CacheView,
    clients::{DataClient, ExecutionClient},
    clock::Clock,
    factories::{ClientConfig, DataClientFactory, ExecutionClientFactory},
};

use crate::{
    AngelOneDataClient, AngelOneDataClientConfig,
    execution::{AngelOneExecutionClient, AngelOneExecutionClientConfig},
};

// ---------------------------------------------------------------------------
// ClientConfig wrappers
// ---------------------------------------------------------------------------

/// [`ClientConfig`] wrapper for [`AngelOneDataClientConfig`].
///
/// Required because `ClientConfig` must be a trait object passable through
/// `LiveNodeBuilder`; we cannot implement foreign traits on foreign types.
#[derive(Debug)]
pub struct AngelOneDataLiveConfig(pub AngelOneDataClientConfig);

impl ClientConfig for AngelOneDataLiveConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// [`ClientConfig`] wrapper for [`AngelOneExecutionClientConfig`].
#[derive(Debug)]
pub struct AngelOneExecLiveConfig(pub AngelOneExecutionClientConfig);

impl ClientConfig for AngelOneExecLiveConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// DataClientFactory
// ---------------------------------------------------------------------------

/// Factory that produces [`AngelOneDataClient`] instances from
/// [`AngelOneDataLiveConfig`].
#[derive(Debug, Default)]
pub struct AngelOneDataClientFactory;

impl DataClientFactory for AngelOneDataClientFactory {
    fn create(
        &self,
        _name: &str,
        config: &dyn ClientConfig,
        _cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn DataClient>> {
        let cfg = config
            .as_any()
            .downcast_ref::<AngelOneDataLiveConfig>()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AngelOneDataClientFactory: expected AngelOneDataLiveConfig, \
                     got a different type. Ensure the correct config is registered."
                )
            })?;
        Ok(Box::new(AngelOneDataClient::new(cfg.0.clone())))
    }

    fn name(&self) -> &str {
        "ANGEL_ONE"
    }

    fn config_type(&self) -> &str {
        "AngelOneDataLiveConfig"
    }
}

// ---------------------------------------------------------------------------
// ExecutionClientFactory
// ---------------------------------------------------------------------------

/// Factory that produces [`AngelOneExecutionClient`] instances from
/// [`AngelOneExecLiveConfig`].
#[derive(Debug, Default)]
pub struct AngelOneExecClientFactory;

impl ExecutionClientFactory for AngelOneExecClientFactory {
    fn create(
        &self,
        _name: &str,
        config: &dyn ClientConfig,
        _cache: CacheView,
    ) -> anyhow::Result<Box<dyn ExecutionClient>> {
        let cfg = config
            .as_any()
            .downcast_ref::<AngelOneExecLiveConfig>()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AngelOneExecClientFactory: expected AngelOneExecLiveConfig, \
                     got a different type. Ensure the correct config is registered."
                )
            })?;
        Ok(Box::new(AngelOneExecutionClient::new(cfg.0.clone())))
    }

    fn name(&self) -> &str {
        "ANGEL_ONE"
    }

    fn config_type(&self) -> &str {
        "AngelOneExecLiveConfig"
    }
}
