#![allow(unused_imports)]
#![allow(unused_variables)]

use std::sync::Arc;
use std::time::Duration;
use frost_protocol::{
    network::{
        p2p::{P2PNode, P2PConfig, NodeIdentity},
        NetworkProtocol,
        ProtocolConfig,
    },
    routing::{
        router::{MessageRouter as ProtocolRouter, RouteStatus, RouteMetrics, RouteState},
        topology::NetworkTopology,
        RoutingConfig,
    },
    message::FrostMessage,
    error::Error as ProtocolError,
    state::ChainId,
};
use sp_runtime::traits::{Block as BlockT, Header as HeaderT};
use frame_system;
use futures::future::BoxFuture;
use uuid::Uuid;

use crate::ssmp_pallet;
use tokio::sync::RwLock;

pub struct SubstrateNetwork<T: ssmp_pallet::Config + Send + Sync + 'static> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    node: Arc<P2PNode>,
    config: ProtocolConfig,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: ssmp_pallet::Config + Send + Sync + 'static> SubstrateNetwork<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    pub fn new(config: ProtocolConfig) -> Result<Self, ProtocolError> {
        let p2p_config = P2PConfig {
            listen_addresses: vec![config.listen_addr.clone()],
            bootstrap_peers: config.bootstrap_peers.clone(),
            connection_timeout: Duration::from_secs(30),
            max_connections: 50,
            enable_nat: true,
            enable_mdns: true,
        };

        let node = P2PNode::new(p2p_config).map(Arc::new)?;

        Ok(Self {
            node,
            config,
            _phantom: Default::default(),
        })
    }
}

#[async_trait::async_trait]
impl<T: ssmp_pallet::Config + Send + Sync + 'static> NetworkProtocol for SubstrateNetwork<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    async fn start(&mut self) -> Result<(), ProtocolError> {
        // Start P2P node
        Arc::get_mut(&mut self.node)
            .ok_or_else(|| ProtocolError::Custom("Failed to get mutable reference to P2P node".into()))?
            .start()
            .await
    }

    async fn stop(&mut self) -> Result<(), ProtocolError> {
        // Stop will be handled by Arc drop
        Ok(())
    }

    async fn broadcast(&self, message: FrostMessage) -> Result<(), ProtocolError> {
        self.node.broadcast_message(message).await.map(|_| ())
    }

    async fn send_to(&self, peer_id: &str, message: FrostMessage) -> Result<(), ProtocolError> {
        // Convert peer_id to PeerId and send
        let peer = self.node.get_peer(peer_id).await?;
        self.node.send_message(&peer, message).await.map(|_| ())
    }

    async fn get_peers(&self) -> Result<Vec<String>, ProtocolError> {
        let peers = self.node.connected_peers().await?;
        Ok(peers.into_iter().map(|p| p.address).collect())
    }
}

pub struct SubstrateRouter<T: ssmp_pallet::Config + Send + Sync + 'static> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    config: RoutingConfig,
    network: Arc<SubstrateNetwork<T>>,
    topology: RwLock<NetworkTopology>,
    metrics: RwLock<RouteMetrics>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: ssmp_pallet::Config + Send + Sync + 'static> SubstrateRouter<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    pub fn new(config: RoutingConfig, network: Arc<SubstrateNetwork<T>>) -> Self {
        Self {
            config,
            network,
            topology: RwLock::new(NetworkTopology::default()),
            metrics: RwLock::new(RouteMetrics::default()),
            _phantom: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl<T: ssmp_pallet::Config + Send + Sync + 'static> ProtocolRouter for SubstrateRouter<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    async fn route_message(&self, message: FrostMessage) -> Result<RouteStatus, ProtocolError> {
        let message_id = Uuid::new_v4();
        let start_time = std::time::Instant::now();

        // Get route from topology
        let topology = self.topology.read();
        let route = topology.get_route(
            &message.source_chain,
            &message.target_chain.unwrap_or_default()
        );

        // If no route found, broadcast to all peers
        if route.is_empty() {
            self.network.broadcast(message).await?;
            return Ok(RouteStatus {
                message_id,
                route: Vec::new(),
                current_hop: 0,
                estimated_time: Duration::from_secs(0),
                state: RouteState::Completed,
            });
        }

        // Send to next hop in route
        let next_hop = &route[0];
        self.network.send_to(&next_hop.to_string(), message).await?;

        // Update metrics
        let mut metrics = self.metrics.write();
        metrics.active_routes += 1;
        metrics.completed_routes += 1;
        metrics.average_route_time = (metrics.average_route_time + start_time.elapsed().as_secs_f64()) / 2.0;
        metrics.route_success_rate = metrics.completed_routes as f64 / (metrics.completed_routes + metrics.failed_routes) as f64;

        Ok(RouteStatus {
            message_id,
            route,
            current_hop: 1,
            estimated_time: Duration::from_secs(route.len() as u64 * 2), // Rough estimate
            state: RouteState::InProgress,
        })
    }

    async fn get_route(&self, from: ChainId, to: ChainId) -> Result<Vec<ChainId>, ProtocolError> {
        let topology = self.topology.read();
        Ok(topology.get_route(&from, &to))
    }

    async fn update_topology(&mut self, topology: NetworkTopology) -> Result<(), ProtocolError> {
        *self.topology.write() = topology;
        Ok(())
    }

    async fn get_metrics(&self) -> Result<RouteMetrics, ProtocolError> {
        Ok(self.metrics.read().clone())
    }
}