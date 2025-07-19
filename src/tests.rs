//! This is a test file

#![allow(unused_imports)]
#![allow(unused_variables)]

use crate::mock::{new_test_ext, Test};
use frame_support::assert_ok;
use frost_protocol::{
    message::{FrostMessage, MessageType},
    network::{Peer, PeerInfo, NetworkError, NetworkProtocol, peer::NodeType},
    error::Error as FrostError,
    routing::router::{RouteStatus, RouteMetrics, RouteState},
    state::ChainId,
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use uuid::Uuid;
use std::time::{Duration, SystemTime};

// Mock Network Protocol for testing
pub struct MockNetworkProtocol {
    peers: Arc<Mutex<HashMap<String, PeerInfo>>>,
    messages: Arc<Mutex<Vec<FrostMessage>>>,
    running: Arc<Mutex<bool>>,
}

impl Default for MockNetworkProtocol {
    fn default() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            messages: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(Mutex::new(false)),
        }
    }
}

#[async_trait]
impl NetworkProtocol for MockNetworkProtocol {
    async fn start(&mut self) -> Result<(), FrostError> {
        let mut running = self.running.lock().unwrap();
        *running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), FrostError> {
        let mut running = self.running.lock().unwrap();
        *running = false;
        Ok(())
    }

    async fn broadcast(&self, message: FrostMessage) -> Result<(), FrostError> {
        let mut messages = self.messages.lock().unwrap();
        messages.push(message);
        Ok(())
    }

    async fn send_to(&self, target: &str, message: FrostMessage) -> Result<(), FrostError> {
        let mut messages = self.messages.lock().unwrap();
        messages.push(message);
        Ok(())
    }

    async fn get_peers(&self) -> Result<Vec<String>, FrostError> {
        let peers = self.peers.lock().unwrap();
        Ok(peers.keys().cloned().collect())
    }
}

impl MockNetworkProtocol {
    // Helper methods for testing
    pub async fn add_peer(&self, peer_id: &str) -> Result<(), FrostError> {
        let mut peers = self.peers.lock().unwrap();
        peers.insert(peer_id.to_string(), PeerInfo {
            address: format!("mock://{}", peer_id),
            protocol_version: "1.0.0".to_string(),
            supported_features: vec!["basic".to_string()],
            chain_ids: vec![1, 2],
        });
        Ok(())
    }

    pub async fn remove_peer(&self, peer_id: &str) -> Result<(), FrostError> {
        let mut peers = self.peers.lock().unwrap();
        peers.remove(peer_id);
        Ok(())
    }

    pub fn get_sent_messages(&self) -> Vec<FrostMessage> {
        self.messages.lock().unwrap().clone()
    }

    pub fn is_running(&self) -> bool {
        *self.running.lock().unwrap()
    }
}

// Mock Router for testing
pub struct MockRouter {
    network: Arc<MockNetworkProtocol>,
}

impl Default for MockRouter {
    fn default() -> Self {
        Self {
            network: Arc::new(MockNetworkProtocol::default()),
        }
    }
}

#[async_trait]
impl frost_protocol::routing::router::MessageRouter for MockRouter {
    async fn route_message(&self, message: FrostMessage) -> Result<RouteStatus, FrostError> {
        self.network.send_to("default_target", message).await?;
        Ok(RouteStatus {
            message_id: Uuid::new_v4(),
            route: vec![],
            current_hop: 0,
            estimated_time: Duration::from_secs(0),
            state: RouteState::Completed,
        })
    }

    async fn get_route(&self, from: ChainId, to: ChainId) -> Result<Vec<ChainId>, FrostError> {
        Ok(vec![from, to])
    }

    async fn update_topology(&mut self, _topology: frost_protocol::routing::NetworkTopology) -> Result<(), FrostError> {
        Ok(())
    }

    async fn get_metrics(&self) -> Result<RouteMetrics, FrostError> {
        Ok(RouteMetrics {
            active_routes: 0,
            completed_routes: 0,
            failed_routes: 0,
            average_route_time: 0.0,
            route_success_rate: 1.0,
        })
    }
}

#[test]
fn test_network_protocol_lifecycle() {
    new_test_ext().execute_with(|| {
        let mut network = MockNetworkProtocol::default();
        
        // Test start
        let result = futures::executor::block_on(network.start());
        assert!(result.is_ok());
        assert!(network.is_running());

        // Test stop
        let result = futures::executor::block_on(network.stop());
        assert!(result.is_ok());
        assert!(!network.is_running());
    });
}

#[test]
fn test_router_message_routing() {
    new_test_ext().execute_with(|| {
        let mut router = MockRouter::default();
        let message = FrostMessage::new(
            MessageType::StateTransition,
            vec![1, 2, 3],
            "source".to_string(),
            None,
        );

        // Test routing message
        let result = futures::executor::block_on(router.route_message(message.clone()));
        assert!(result.is_ok());

        // Verify message was sent
        let messages = router.network.get_sent_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload, message.payload);
    });
}

#[test]
fn test_router_get_peers() {
    new_test_ext().execute_with(|| {
        let router = MockRouter::default();
        
        // Add some test peers
        futures::executor::block_on(router.network.add_peer("peer1")).unwrap();
        futures::executor::block_on(router.network.add_peer("peer2")).unwrap();

        // Test getting route between peers
        let route = futures::executor::block_on(router.get_route(
            ChainId::new(1),
            ChainId::new(2)
        )).unwrap();
        assert_eq!(route.len(), 2);
        assert_eq!(route[0], ChainId::new(1));
        assert_eq!(route[1], ChainId::new(2));
    });
} 