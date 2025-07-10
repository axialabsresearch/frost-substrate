use std::marker::PhantomData;
use frost_protocol::{
    routing::router::{
        MessageRouter as ProtocolRouter,
        RouterConfig,
        RouteStatus,
        RouteMetrics,
        RouterHealth,
        EnhancedRouter,
    },
    message::FrostMessage,
    network::NetworkProtocol,
    state::ChainId,
    Result as ProtocolResult,
};
use sp_std::vec::Vec;
use sp_runtime::codec::{Encode, Decode};
use scale_info::TypeInfo;
use async_trait::async_trait;

use crate::frost_pallet::Config;

/// Substrate network protocol implementation
pub struct SubstrateNetwork<T: Config> {
    _phantom: PhantomData<T>,
}

impl<T: Config> SubstrateNetwork<T> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<T: Config> NetworkProtocol for SubstrateNetwork<T> 
where
    T: Send + Sync + 'static
{
    async fn start(&mut self) -> ProtocolResult<()> {
        // Initialize substrate network
        Ok(())
    }

    async fn stop(&mut self) -> ProtocolResult<()> {
        // Cleanup substrate network
        Ok(())
    }

    async fn broadcast(&self, message: FrostMessage) -> ProtocolResult<()> {
        // Use substrate gossip messaging
        Ok(())
    }

    async fn send_to(&self, peer_id: &str, message: FrostMessage) -> ProtocolResult<()> {
        // Use substrate direct messaging
        Ok(())
    }

    async fn get_peers(&self) -> ProtocolResult<Vec<String>> {
        // Get substrate network peers
        Ok(Vec::new())
    }
}

/// Substrate router implementation using frost-protocol
pub struct SubstrateRouter<T: Config> {
    inner: EnhancedRouter<SubstrateNetwork<T>>,
}

impl<T: Config> SubstrateRouter<T> {
    pub fn new(config: RouterConfig) -> Self {
        let network = SubstrateNetwork::new();
        Self {
            inner: EnhancedRouter::new(config, network),
        }
    }

    pub async fn health(&self) -> RouterHealth {
        self.inner.health().await
    }
}

#[async_trait]
impl<T: Config> ProtocolRouter for SubstrateRouter<T> 
where
    T: Send + Sync + 'static
{
    async fn route(&self, message: FrostMessage, target: ChainId) -> ProtocolResult<RouteStatus> {
        self.inner.route(message, target).await
    }

    async fn update_routes(&self, routes: Vec<(ChainId, String)>) -> ProtocolResult<()> {
        self.inner.update_routes(routes).await
    }

    async fn get_routes(&self) -> ProtocolResult<Vec<(ChainId, String)>> {
        self.inner.get_routes().await
    }

    async fn get_metrics(&self) -> ProtocolResult<RouteMetrics> {
        self.inner.get_metrics().await
    }
}

pub use frost_protocol::routing::router::RouterConfig; 