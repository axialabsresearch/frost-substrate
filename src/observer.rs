#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]

use crate::ssmp_pallet;
use std::{sync::Arc, marker::PhantomData, ops::Deref, fmt::Debug};
use parking_lot::RwLock;
use frost_protocol::{
    state::{BlockRef, StateTransition, ChainId, StateRoot, transition::TransitionMetadata},
    finality::predicate::FinalityPredicate,
    error::Error as ProtocolError,
    routing::router::MessageRouter,
    message::FrostMessage,
};
use sp_runtime::traits::{Block as BlockT, Header as HeaderT, NumberFor, Zero, SaturatedConversion, Saturating};
use sp_api::{HeaderT as ApiHeaderT, ProvideRuntimeApi};
use sp_blockchain::{HeaderBackend, Backend as BlockBackend};
use sp_consensus_grandpa::{AuthorityList, GrandpaJustification};
use parity_scale_codec::{Encode, Decode, EncodeLike, WrapperTypeEncode, WrapperTypeDecode, MaxEncodedLen};
use scale_info::TypeInfo;
use frame_system::EventRecord;
use log::error;
use futures::{StreamExt, Stream};
use finality_grandpa::voter_set::VoterSet;
use sp_core::H256;
use frame_support::{
    Parameter, 
    pallet_prelude::{Member, BoundedVec, ConstU32},
};
use sp_runtime::RuntimeDebug;

use crate::{
    ssmp_pallet::Config,
    finality::{GrandpaFinality, SubstrateVerificationClient, GrandpaVerificationParams, ChainConfig},
    proof::ProofGenerator,
};

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
<T as ssmp_pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
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
    finality: Arc<GrandpaFinality<T::Client, <T as ssmp_pallet::Config>::Block>>,
    /// Phantom data
    _phantom: PhantomData<T>,
}

impl<T: Config + Send + Sync + 'static> SSMPObserver<T> 
where
    <T as ssmp_pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
    NumberFor<<T as ssmp_pallet::Config>::Block>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
{
    /// Create new observer instance
    pub fn new(
        client: Arc<T::Client>,
        proof_generator: Arc<dyn ProofGenerator>,
        message_router: T::MessageRouter,
    ) -> Self where <<<T as ssmp_pallet::Config>::Block as sp_api::BlockT>::Header as sp_api::HeaderT>::Number: From<u64> {
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
    ) -> Result<StateTransition> {
        // Convert block hash properly
        let block_hash_bytes: [u8; 32] = block_ref.hash
            .as_ref()
            .try_into()
            .map_err(|_| ErrorKind::InvalidBlockHash)?;

        // let state = self.client
        //     .runtime_api()
        //     .state_at(&block_hash_bytes.into())
        //     .map_err(ErrorKind::from)?;

        // let relevant_state = state
        //     .get_storage(target.module_name.as_slice())
        //     .ok_or(ErrorKind::StateNotFound)?;

        Ok(StateTransition {
            chain_id: block_ref.chain_id.clone(),
            block_height: block_ref.number,
            pre_state: StateRoot {
                block_ref: Default::default(),
                root_hash: [0; 32],
                metadata: None,
            },
            post_state: Default::default(),
            transition_proof: Default::default(),
            metadata: Default::default(),
        })
    }

    /// Handle finality notification from GRANDPA
    pub async fn handle_finality_notification(
        &self,
        notification: FinalityNotification<<T as ssmp_pallet::Config>::Block>,
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
                    .generate_proof(&transition, &Default::default())
                    .await
                    .map_err(|_| ErrorKind::ProofGenerationFailed)?;

                // Only attempt routing if a router is configured
                if let Some(router) = &self.message_router {
                    router.route_message(FrostMessage::new(
                        frost_protocol::message::MessageType::StateProof,
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

    /// Check if block matches watch target
    async fn matches_watch_target(
        &self,
        block_ref: &BlockRef,
        target: &WatchTarget,
    ) -> Result<bool> {
        // Convert hash for API call
        let block_hash_bytes: [u8; 32] = block_ref.hash
            .as_ref()
            .try_into()
            .map_err(|_| ErrorKind::InvalidBlockHash)?;

        // Get block events - we'll need to implement the actual API call
        // let events = self.client
        //     .runtime_api()
        //     .events_at(&block_hash_bytes.into())?;

        // For now, I'll just use empty vector as placeholder
        let events = Vec::<EventRecord<<T as Config>::RuntimeEvent, <T as frame_system::Config>::Hash>>::new();

        // Check if any events match our target
        for event_record in events {
            let EventRecord { 
                phase: _,
                event,
                topics: _,
            } = event_record;

            // Pattern match on the runtime event instead of calling methods
            // We'll replace this with actual pattern matching based on the RuntimeEvent enum
            match event {
                // Example pattern matching - replace with our actual RuntimeEvent variants
                // RuntimeEvent::YourPallet(pallet_event) => {
                //     match pallet_event {
                //         YourPalletEvent::TargetEvent { data, .. } => {
                //             if let Some(msg_hash) = &target.message_hash {
                //                 if data.contains(msg_hash) {
                //                     return Ok(true);
                //                 }
                //             } else {
                //                 return Ok(true);
                //             }
                //         }
                //         _ => {}
                //     }
                // }
                _ => {
                    // This is a placeholder logic - we'll need to implement actual event matching
                    // based on the specific RuntimeEvent structure
                }
            }
        }

        Ok(false)
    }
}

#[async_trait::async_trait]
impl<T: Config + Send + Sync + 'static> FinalityObserver for SSMPObserver<T> 
where
    <T as ssmp_pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
    NumberFor<<T as ssmp_pallet::Config>::Block>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
{
    async fn on_block_finalized(&self, block: BlockRef) -> Result<()> {
        let hash = <<T as ssmp_pallet::Config>::Block as BlockT>::Hash::decode(&mut &block.hash[..])
            .map_err(|_| ErrorKind::InvalidBlockHash)?;
        let number = NumberFor::<<T as ssmp_pallet::Config>::Block>::try_from(block.number)
            .map_err(|_| ErrorKind::InvalidBlockNumber)?;

        let notification = FinalityNotification::<<T as ssmp_pallet::Config>::Block> {
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
    <T as ssmp_pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
{
    observer: Arc<SSMPObserver<T>>,
}

impl<T: Config + Send + Sync + 'static> SSMPWorker<T> 
where
    <T as ssmp_pallet::Config>::Block: BlockT<Hash = H256> + HeaderT,
{
    pub fn new(observer: Arc<SSMPObserver<T>>) -> Self {
        Self { observer }
    }

    pub async fn run(&self) {
        // let mut finality_stream = self.observer.finality.subscribe_finality();
        // while let Some(notification) = finality_stream.next().await {
        //     if let Err(e) = self.observer.handle_finality_notification(notification).await {
        //         error!("Error handling finality notification: {:?}", e);
        //     }
        // }
    }
} 