use std::sync::Arc;
use tokio::sync::RwLock;
use async_trait::async_trait;
use frost_protocol::{
    routing::router::{MessageRouter as FrostMessageRouter, EnhancedRouter, RouteStatus},
    network::NetworkProtocol,
    message::FrostMessage,
    Error as FrostError,
    state::ChainId,
};

/// Adapter to convert frost-protocol router to our router interface
pub struct RouterAdapter<N: NetworkProtocol> {
    inner: Arc<RwLock<EnhancedRouter<N>>>,
}

impl<N: NetworkProtocol> RouterAdapter<N> {
    pub fn new(inner: EnhancedRouter<N>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }
}

#[async_trait]
impl<N: NetworkProtocol + 'static> FrostMessageRouter for RouterAdapter<N> {
    async fn route_message(&self, message: FrostMessage) -> Result<RouteStatus, FrostError> {
        self.inner.read().await.route_message(message).await
    }

    async fn get_route(&self, from: ChainId, to: ChainId) -> Result<Vec<ChainId>, FrostError> {
        self.inner.read().await.get_route(from, to).await
    }

    async fn update_topology(&mut self, topology: frost_protocol::routing::NetworkTopology) -> Result<(), FrostError> {
        self.inner.write().await.update_topology(topology).await
    }

    async fn get_metrics(&self) -> Result<frost_protocol::routing::router::RouteMetrics, FrostError> {
        self.inner.read().await.get_metrics().await
    }
}

// Re-export the router trait
pub use frost_protocol::routing::router::MessageRouter; 