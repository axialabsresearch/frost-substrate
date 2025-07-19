#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]

use crate::pallet;
use crate::wrapper_types::SSMPStateTransition;
use std::{sync::Arc, marker::PhantomData, ops::Deref, fmt::Debug, any::Any};
use parking_lot::RwLock;
use frost_protocol::{
    state::{BlockRef, StateTransition, ChainId, StateRoot, transition::TransitionMetadata},
    finality::predicate::{FinalityPredicate, FinalityVerificationClient, FinalityVerificationError},
    error::Error as ProtocolError,
    routing::router::MessageRouter,
    message::{FrostMessage, MessageType},
};
use sp_runtime::traits::{Block as BlockT, Header as HeaderT, NumberFor, Zero, SaturatedConversion, Saturating};
use sp_api::{HeaderT as ApiHeaderT, ProvideRuntimeApi};
use sp_blockchain::{HeaderBackend, Backend as BlockBackend};
use sp_consensus_grandpa::{AuthorityList, GrandpaJustification, GrandpaApi};
use parity_scale_codec::{Encode, Decode, EncodeLike, WrapperTypeEncode, WrapperTypeDecode, MaxEncodedLen};
use scale_info::TypeInfo;
use frame_system::{EventRecord, Config as SystemConfig, self};
use log::{error, debug, info};
use futures::{StreamExt, Stream};
use finality_grandpa::voter_set::VoterSet;
use sp_core::H256;
use frame_support::{
    Parameter, 
    pallet_prelude::{Member, BoundedVec, ConstU32},
};
use sp_runtime::RuntimeDebug;
use crate::runtime_api::SSMPApi;

use crate::{
    pallet::Config,
    finality::{GrandpaFinality, SubstrateVerificationClient, GrandpaVerificationParams, ChainConfig},
    proof::ProofGenerator,
};
use async_trait::async_trait;

#[derive(Debug)]
pub enum ErrorKind {
    InvalidBlockHash,
    InvalidBlockNumber,
    RuntimeApiError(sp_api::ApiError),
    StateNotFound,
    ProofGenerationFailed,
    MessageRoutingFailed,
}

impl From<ErrorKind> for ProtocolError {
    fn from(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::InvalidBlockHash => ProtocolError::Custom("Invalid block hash".into()),
            ErrorKind::InvalidBlockNumber => ProtocolError::Custom("Invalid block number".into()),
            ErrorKind::RuntimeApiError(e) => ProtocolError::Custom(format!("Runtime API error: {}", e)),
            ErrorKind::StateNotFound => ProtocolError::Custom("State not found".into()),
            ErrorKind::ProofGenerationFailed => ProtocolError::Custom("Failed to generate proof".into()),
            ErrorKind::MessageRoutingFailed => ProtocolError::Custom("Failed to route message".into()),
        }
    }
}

impl From<sp_api::ApiError> for ErrorKind {
    fn from(error: sp_api::ApiError) -> Self {
        ErrorKind::RuntimeApiError(error)
    }
}

/// Target to watch for finality
#[derive(Clone, Encode, Decode, TypeInfo, RuntimeDebug, PartialEq, Eq, MaxEncodedLen)]
pub struct WatchTarget {
    /// Module to watch
    pub module_name: BoundedVec<u8, ConstU32<32>>,
    /// Function to watch
    pub function_name: BoundedVec<u8, ConstU32<32>>,
    /// Optional message hash to watch for
    pub message_hash: Option<BoundedVec<u8, ConstU32<64>>>,
    /// Target chain to route to
    pub target_chain: BoundedVec<u8, ConstU32<32>>,
}

impl WatchTarget {
    pub fn new(
        module_name: Vec<u8>,
        function_name: Vec<u8>,
        message_hash: Option<Vec<u8>>,
        target_chain: Vec<u8>,
    ) -> std::result::Result<Self, &'static str> {
        Ok(Self {
            module_name: BoundedVec::try_from(module_name)
                .map_err(|_| "Module name too long")?,
            function_name: BoundedVec::try_from(function_name)
                .map_err(|_| "Function name too long")?,
            message_hash: if let Some(hash) = message_hash {
                Some(BoundedVec::try_from(hash)
                    .map_err(|_| "Message hash too long")?)
            } else {
                None
            },
            target_chain: BoundedVec::try_from(target_chain)
                .map_err(|_| "Target chain too long")?,
        })
    }
}

/// Finality notification from GRANDPA
pub struct FinalityNotification<B: BlockT> 
where
    B::Header: HeaderT,
{
    /// Hash of the finalized block
    pub hash: <B as BlockT>::Hash,
    /// Number of the finalized block
    pub number: NumberFor<B>,
    /// Justification, if available
    pub justification: Option<GrandpaJustification<B::Header>>,
}

type Result<T> = std::result::Result<T, ProtocolError>;

#[async_trait::async_trait]
pub trait FinalityObserver: Send + Sync {
    async fn on_block_finalized(&self, block: BlockRef) -> Result<()>;
}


/// Core state monitoring observer that manages watch targets and proof generation
pub struct SSMPObserver<T: Config + Send + Sync + 'static> 
where   
<T as pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
<T as frame_system::Config>::Hash: sp_api::HashT,
<<T as pallet::Config>::Client as sp_api::ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api:  
    crate::runtime_api::SSMPApi< 
        <T as pallet::Config>::Block,
        <T as pallet::Config>::RuntimeEvent,
        <T as frame_system::Config>::Hash
    > + sp_consensus_grandpa::GrandpaApi<<T as pallet::Config>::Block>,
{
    /// Registered watch targets
    watch_targets: RwLock<Vec<WatchTarget>>,
    /// Proof generator
    proof_generator: Arc<dyn ProofGenerator>,
    /// Optional message router
    message_router: Option<Arc<dyn frost_protocol::routing::router::MessageRouter + Send + Sync>>,
    /// Client reference
    client: Arc<T::Client>,
    /// Finality checker
    finality: Arc<GrandpaFinality<T::Client, <T as pallet::Config>::Block>>,
    /// Phantom data
    _phantom: PhantomData<T>,
}

#[derive(Debug, Default)]
struct ProcessingStats {
    blocks_processed: u64,
    events_matched: u64,
    proofs_generated: u64,
    messages_routed: u64,
    processing_errors: u64,
    average_processing_time_ms: f64,
}

impl<T: Config + SystemConfig + Send + Sync + 'static> SSMPObserver<T> 
where
    <T as frame_system::Config>::Hash: sp_api::HashT,
    NumberFor<<T as pallet::Config>::Block>: Into<u64> + From<u64> + SaturatedConversion + Saturating + Zero + Copy,
    <<T as pallet::Config>::Block as BlockT>::Header: HeaderT<Number = u32>,
    <<<T as pallet::Config>::Block as BlockT>::Header as HeaderT>::Number: From<u64> + From<f64>,
    T::Client: ProvideRuntimeApi<<T as pallet::Config>::Block> + HeaderBackend<<T as pallet::Config>::Block> + Send + Sync + 'static,
    <T::Client as ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api: 
        SSMPApi<<T as pallet::Config>::Block, <T as pallet::Config>::RuntimeEvent, <T as pallet::Config>::Hash>
        + sp_api::Core<<T as pallet::Config>::Block>
        + sp_consensus_grandpa::GrandpaApi<<T as pallet::Config>::Block>,
{
    /// Create new observer instance 
    pub fn new(
        client: Arc<T::Client>,
        proof_generator: Arc<dyn ProofGenerator>,
        message_router: T::MessageRouter,
    ) -> Self {
        let params = GrandpaVerificationParams::default();
        let chain_config = ChainConfig::default();
        let voter_set = VoterSet::new(Vec::new()).expect("Empty voter set is valid");
        
        let verification_client = Arc::new(SubstrateVerificationClient::new(
            client.clone(),
            voter_set.clone(),
            params.clone(),
            chain_config.clone(),
        ));

        let finality = Arc::new(GrandpaFinality::new(
            client.clone(),
            Default::default(),
            verification_client,
            Vec::new(), // Empty initial authority list
        ));

        Self {
            watch_targets: RwLock::new(Vec::new()),
            proof_generator,
            message_router: message_router.into().map(|router| {
                let router: Arc<dyn frost_protocol::routing::router::MessageRouter + Send + Sync> = router;
                router
            }),
            client,
            finality,
            _phantom: PhantomData,
        }
    }

    /// Create a new observer instance with proper authority initialization
    pub fn new_with_runtime_authorities(
        client: Arc<T::Client>,
        proof_generator: Arc<dyn ProofGenerator>,
        message_router: T::MessageRouter,
        chain_config: ChainConfig,
    ) -> Result<Self> 
    where
        T::Client: ProvideRuntimeApi<<T as pallet::Config>::Block>,
        <T::Client as ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api: GrandpaApi<<T as pallet::Config>::Block>,
        <<<T as pallet::Config>::Block as sp_api::BlockT>::Header as sp_api::HeaderT>::Number: From<u64>
    {
        let params = GrandpaVerificationParams::default();
        
        let finality = Arc::new(GrandpaFinality::new_from_runtime(
            client.clone(),
            Default::default(),
            params,
            chain_config,
        ).map_err(|e| ProtocolError::Custom(format!("Failed to initialize finality: {}", e)))?);
        
        // Monitoring event processing channels (We'll apply this to SSMPObserver struct)
        // event_tx: mpsc::UnboundedSender<EventProcessingResult>
        // event_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<EventProcessingResult>>>,
        // let (event_tx, event_rx) = mpsc::unbounded_channel();

        Ok(Self {
            watch_targets: RwLock::new(Vec::new()),
            proof_generator,
            message_router: message_router.into().map(|router| {
                let router: Arc<dyn frost_protocol::routing::router::MessageRouter + Send + Sync> = router;
                router
            }),
            client,
            finality,
            _phantom: PhantomData,
        })
    }

    /// Event matching with proper runtime API calls
    async fn matches_watch_target(
        &self,
        block_ref: &BlockRef,
        target: &WatchTarget,
    ) -> Result<bool> 
    where
        <T as frame_system::Config>::Hash: sp_api::HashT,
    {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        let events = self.client
            .runtime_api()
            .events(block_hash)
            .map_err(|e| ProtocolError::Custom(format!("Failed to fetch events: {}", e)))?;
    
        for event_record in events {
            if self.event_matches_target(&event_record.event, target)? {
                return Ok(true);
            }
        }
    
        Ok(false)
    }

    /// Register a new watch target
    pub fn register_watch_target(&self, target: WatchTarget) {
        let mut targets = self.watch_targets.write();
        targets.push(target);
    }

    /// Generate state transition for matched target
    fn generate_state_transition(
        &self,
        block_ref: &BlockRef,
        target: &WatchTarget,
    ) -> Result<SSMPStateTransition> {
        // Convert block hash properly
        let block_hash_bytes: [u8; 32] = block_ref.hash
            .as_ref()
            .try_into()
            .map_err(|_| ErrorKind::InvalidBlockHash)?;

        // Create pre and post state roots
        let pre_state = StateRoot {
            block_ref: Default::default(),
            root_hash: [0; 32],
            metadata: None,
        };
        let post_state = Default::default();

        Ok(SSMPStateTransition::new(
            block_ref.chain_id.clone(),
            block_ref.number,
            pre_state,
            post_state,
            target.module_name.to_vec(),
            target.function_name.to_vec(),
            target.target_chain.to_vec(),
        ))
    }
    /// Handle finality notification from GRANDPA
    pub async fn handle_finality_notification(
        &self,
        notification: FinalityNotification<<T as pallet::Config>::Block>,
    ) -> Result<()> {
        // Convert hash properly
        let hash_bytes: [u8; 32] = notification.hash
            .as_ref()
            .try_into()
            .map_err(|_| ErrorKind::InvalidBlockHash)?;

        // Convert block number properly
        let block_number: u64 = notification.number
            .try_into()
            .map_err(|_| ErrorKind::InvalidBlockNumber)?;

        let block_ref = BlockRef {
            chain_id: ChainId::default(),
            number: block_number,
            hash: hash_bytes,
        };

        if !self.finality.is_final(&block_ref).await? {
            return Ok(());
        }

        // Clone targets to avoid holding the lock across await points
        let targets = {
            let guard = self.watch_targets.read();
            guard.clone()
        };

        for target in targets.iter() {
            if self.matches_watch_target(&block_ref, target).await? {
                let transition = self.generate_state_transition(&block_ref, target)?;
                let proof = self.proof_generator
                    .generate_proof(&transition.into(), &Default::default())
                    .await
                    .map_err(|_| ErrorKind::ProofGenerationFailed)?;

                // Only attempt routing if a router is configured
                if let Some(router) = &self.message_router {
                    router.route_message(FrostMessage::new(
                        MessageType::StateTransition,
                        proof.encode(),
                        "source".to_string(),
                        None,
                    ))
                    .await
                    .map_err(|_| ErrorKind::MessageRoutingFailed)?;
                }
            }
        }

        Ok(())
    }

    /// Get runtime events for a specific block
    async fn get_block_events(
        &self,
        block_hash: H256,
    ) -> Result<Vec<EventRecord<<T as frame_system::Config>::RuntimeEvent, <T as frame_system::Config>::Hash>>> 
    where
        T::Client: ProvideRuntimeApi<<T as pallet::Config>::Block>,
        <T::Client as ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api: 
    SSMPApi<
        <T as pallet::Config>::Block, // The Block type
        <T as pallet::Config>::RuntimeEvent, // The Event type
        <T as frame_system::Config>::Hash // The BlockHash type (Self::Hash in Config)
    > + sp_api::Core<<T as pallet::Config>::Block>,
    {
        let events = self.client
            .runtime_api()
            .events(block_hash)
            .map_err(|e| ProtocolError::Custom(format!("Failed to get events: {}", e)))?;

        Ok(events)
    }

    /// Check if a specific event matches the watch target
    fn event_matches_target(
        &self,
        event: &<T as pallet::Config>::RuntimeEvent,
        target: &WatchTarget,
    ) -> Result<bool> {
        // This is where you'd implement your specific event matching logic
        // The exact implementation depends on your RuntimeEvent structure
        
        // Example pattern matching (you'll need to adapt this to your specific events):
        match event {
            // If your pallet has a specific event variant:
            // T::RuntimeEvent::YourPallet(pallet_event) => {
            //     match pallet_event {
            //         your_pallet::Event::MessageSent { message_hash, target_chain, .. } => {
            //             // Check if target chain matches
            //             let target_chain_str = String::from_utf8_lossy(&target.target_chain);
            //             if target_chain.as_str() != target_chain_str {
            //                 return Ok(false);
            //             }
            //             
            //             // Check message hash if specified
            //             if let Some(expected_hash) = &target.message_hash {
            //                 return Ok(message_hash.as_ref() == expected_hash.as_slice());
            //             }
            //             
            //             Ok(true)
            //         }
            //         _ => Ok(false),
            //     }
            // }
            _ => {
                // For now, we'll use a generic approach
                // You should replace this with specific event matching logic
                self.generic_event_match(event, target)
            }
        }
    }

    /// Generic event matching fallback
    fn generic_event_match(
        &self,
        event: &<T as pallet::Config>::RuntimeEvent,
        target: &WatchTarget,
    ) -> Result<bool> {
        // This is a placeholder implementation
        // In a real scenario, you'd need to:
        // 1. Serialize the event to bytes
        // 2. Check if it contains the target module/function identifiers
        // 3. Optionally check message hash
        
        let event_bytes = event.encode();
        let module_name = &target.module_name;
        let function_name = &target.function_name;
        
        // Simple byte matching - you might want something more sophisticated
        let contains_module = event_bytes.windows(module_name.len())
            .any(|window| window == module_name.as_slice());
        let contains_function = event_bytes.windows(function_name.len())
            .any(|window| window == function_name.as_slice());
        
        if !contains_module || !contains_function {
            return Ok(false);
        }
        
        // Check message hash if specified
        if let Some(expected_hash) = &target.message_hash {
            let contains_hash = event_bytes.windows(expected_hash.len())
                .any(|window| window == expected_hash.as_slice());
            return Ok(contains_hash);
        }
        
        Ok(true)
    }
}

#[async_trait::async_trait]
impl<T: Config + SystemConfig + Send + Sync + 'static> FinalityObserver for SSMPObserver<T>
where
    <T as pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
    NumberFor<<T as pallet::Config>::Block>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
    T::Client: ProvideRuntimeApi<<T as pallet::Config>::Block>,
    T::Client: HeaderBackend<<T as pallet::Config>::Block>,
    T::Client: BlockBackend<<T as pallet::Config>::Block>,
    T::Client: Send + Sync + 'static, 
    <<T as pallet::Config>::Client as sp_api::ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api:
        crate::runtime_api::SSMPApi<
            <T as pallet::Config>::Block,
            <T as pallet::Config>::RuntimeEvent,
            <T as frame_system::Config>::Hash
        >,
{
    async fn on_block_finalized(&self, block: BlockRef) -> Result<()> {
        let hash = <<T as pallet::Config>::Block as BlockT>::Hash::decode(&mut &block.hash[..])
            .map_err(|_| ErrorKind::InvalidBlockHash)?;
        let number = NumberFor::<<T as pallet::Config>::Block>::try_from(block.number)
            .map_err(|_| ErrorKind::InvalidBlockNumber)?;

        let notification = FinalityNotification::<<T as pallet::Config>::Block> {
            hash,
            number,
            justification: None,
        };

        self.handle_finality_notification(notification).await
    }
}

/// Background worker that subscribes to GRANDPA notifications
pub struct SSMPWorker<T: Config + Send + Sync + 'static> 
where
    <T as pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
    <T::Client as sp_api::ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api:
        crate::runtime_api::SSMPApi<
            <T as pallet::Config>::Block,
            <T as pallet::Config>::RuntimeEvent,
            <T as frame_system::Config>::Hash
        > + sp_api::Core<<T as pallet::Config>::Block>
        + sp_consensus_grandpa::GrandpaApi<<T as pallet::Config>::Block>,
{
    observer: Arc<SSMPObserver<T>>,
}

impl<T: Config + Send + Sync + 'static> SSMPWorker<T> 
where
    <T as pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
    NumberFor<<T as pallet::Config>::Block>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
    <T::Client as sp_api::ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api:
        crate::runtime_api::SSMPApi<
            <T as pallet::Config>::Block,
            <T as pallet::Config>::RuntimeEvent,
            <T as frame_system::Config>::Hash
        > + sp_api::Core<<T as pallet::Config>::Block>
        + sp_consensus_grandpa::GrandpaApi<<T as pallet::Config>::Block>,
{
    pub fn new(observer: Arc<SSMPObserver<T>>) -> Self {
        Self { observer }
    }

    pub async fn run(&self) {
        // let mut finality_stream = self.observer.finality.subscribe_finality();
        // while let Some(notification) = finality_stream.next().await {
        //     if let Err(e) = self.observer.handle_finality_notification(notification).await {
        //         error!("Error handling finality notifcation: {:?}", e);
        //     }
        // }
    }

    /// Run the worker with proper finality stream
    pub async fn run_with_finality_stream<S>(&self, mut finality_stream: S) 
    where
        S: Stream<Item = FinalityNotification<<T as pallet::Config>::Block>> + Unpin,
    {
        while let Some(notification) = finality_stream.next().await {
            if let Err(e) = self.observer.handle_finality_notification(notification).await {
                error!("Error handling finality notification: {:?}", e);
            }
        }
    }
}