#![allow(unused_imports)]
#![allow(unused_variables)]

use std::sync::Arc;
use std::time::Duration;
use frost_protocol::{
    network::NetworkProtocol,
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

use crate::frost_pallet;

pub struct SubstrateNetwork<T: frost_pallet::Config + Send + Sync + 'static> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    _phantom: std::marker::PhantomData<T>,
}

impl<T: frost_pallet::Config + Send + Sync + 'static> SubstrateNetwork<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    pub fn new() -> Self {
        Self {
            _phantom: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl<T: frost_pallet::Config + Send + Sync + 'static> NetworkProtocol for SubstrateNetwork<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    async fn start(&mut self) -> Result<(), ProtocolError> {
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ProtocolError> {
        Ok(())
    }

    async fn broadcast(&self, _message: FrostMessage) -> Result<(), ProtocolError> {
        Ok(())
    }

    async fn send_to(&self, _peer_id: &str, _message: FrostMessage) -> Result<(), ProtocolError> {
        Ok(())
    }

    async fn get_peers(&self) -> Result<Vec<String>, ProtocolError> {
        Ok(Vec::new())
    }
}

pub struct SubstrateRouter<T: frost_pallet::Config + Send + Sync + 'static> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    config: RoutingConfig,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: frost_pallet::Config + Send + Sync + 'static> SubstrateRouter<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    pub fn new(config: RoutingConfig) -> Self {
        Self {
            config,
            _phantom: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl<T: frost_pallet::Config + Send + Sync + 'static> ProtocolRouter for SubstrateRouter<T> 
where
    <T as frame_system::Config>::Block: BlockT,
    <<T as frame_system::Config>::Block as BlockT>::Header: HeaderT,
{
    async fn route_message(&self, _message: FrostMessage) -> Result<RouteStatus, ProtocolError> {
        Ok(RouteStatus {
            message_id: Uuid::new_v4(),
            route: Vec::new(),
            current_hop: 0,
            estimated_time: Duration::from_secs(0),
            state: RouteState::InProgress,
        })
    }

    async fn get_route(&self, _from: ChainId, _to: ChainId) -> Result<Vec<ChainId>, ProtocolError> {
        Ok(Vec::new())
    }

    async fn update_topology(&mut self, _topology: NetworkTopology) -> Result<(), ProtocolError> {
        Ok(())
    }

    async fn get_metrics(&self) -> Result<RouteMetrics, ProtocolError> {
        Ok(RouteMetrics {
            active_routes: 0,
            completed_routes: 0,
            failed_routes: 0,
            average_route_time: 0.0,
            route_success_rate: 0.0,
        })
    }
}