use std::{sync::Arc, marker::PhantomData};
use frost_protocol::{
    state::{BlockRef, StateTransition, StateProof},
    finality::predicate::FinalityPredicate,
    types::ProofType,
    error::Error as ProtocolError,
};
use sp_runtime::traits::{Block as BlockT, Header as HeaderT};
use sp_consensus_grandpa::{
    AuthorityList,
    GRANDPA_ENGINE_ID,
    GrandpaJustification,
    VersionedAuthorityList,
};
use frame_support::traits::Get;
use codec::{Encode, Decode};
use scale_info::TypeInfo;
use frame_system::EventRecord;
use log::{error, info};

use crate::{
    frost_pallet::Config,
    finality::{GrandpaFinality, SubstrateVerificationClient},
    proof::ProofGenerator,
    routing::MessageRouter,
};

/// Target to watch for finality
#[derive(Clone, Encode, Decode, TypeInfo)]
pub struct WatchTarget {
    /// Module to watch
    pub module_name: Vec<u8>,
    /// Function to watch
    pub function_name: Vec<u8>,
    /// Optional message hash to watch for
    pub message_hash: Option<Vec<u8>>,
    /// Target chain to route to
    pub target_chain: Vec<u8>,
}

/// Finality notification from GRANDPA
#[derive(Debug)]
pub struct FinalityNotification<B: BlockT> {
    /// Hash of the finalized block
    pub hash: B::Hash,
    /// Number of the finalized block
    pub number: <B::Header as HeaderT>::Number,
    /// Justification, if available
    pub justification: Option<GrandpaJustification<B>>,
}

type Result<T> = std::result::Result<T, ProtocolError>;

#[async_trait::async_trait]
pub trait FinalityObserver: Send + Sync {
    async fn on_block_finalized(&self, block: BlockRef) -> Result<()>;
}

/// Core finality observer that manages watch targets and proof generation
pub struct FrostFinalityObserver<T: Config> {
    /// Underlying GRANDPA finality checker
    finality: GrandpaFinality<T::Client, T::Block>,
    /// Registered watch targets
    watch_targets: Vec<WatchTarget>,
    /// Proof generator
    proof_generator: Arc<dyn ProofGenerator>,
    /// Message router
    message_router: Arc<dyn MessageRouter>,
    /// Phantom data
    _phantom: PhantomData<T>,
}

impl<T: Config> FrostFinalityObserver<T> {
    /// Create new observer instance
    pub fn new(
        client: Arc<T::Client>,
        authority_set: Arc<AuthorityList>,
        proof_generator: Arc<dyn ProofGenerator>,
        message_router: Arc<dyn MessageRouter>,
    ) -> Self {
        // Create verification client
        let verification_client = SubstrateVerificationClient::new(
            client.clone(),
            authority_set,
            Default::default(),
        );

        // Create finality checker
        let finality = GrandpaFinality::new(
            client,
            Default::default(),
            Arc::new(verification_client),
        );

        Self {
            finality,
            watch_targets: Vec::new(),
            proof_generator,
            message_router,
            _phantom: PhantomData,
        }
    }

    /// Register a new watch target
    pub fn register_watch_target(&mut self, target: WatchTarget) {
        self.watch_targets.push(target);
    }

    /// Handle finality notification from GRANDPA
    pub async fn handle_finality_notification(
        &self,
        notification: FinalityNotification<T::Block>,
    ) -> Result<()> {
        // Convert to BlockRef
        let block_ref = BlockRef {
            chain_id: 0, // Set appropriate chain ID
            number: notification.number.into(),
            hash: notification.hash.encode(),
        };

        // Verify finality using existing GrandpaFinality
        if !self.finality.is_final(&block_ref).await? {
            return Ok(());
        }

        // Check all watch targets
        for target in &self.watch_targets {
            if self.matches_watch_target(&block_ref, target).await? {
                // Generate state transition
                let transition = self.generate_state_transition(&block_ref, target)?;
                
                // Generate proof using existing generator
                let proof = self.proof_generator
                    .generate_proof(&transition, &Default::default())
                    .await?;

                // Route proof to target chain
                self.message_router.route_message(
                    target.target_chain.clone(),
                    proof.into(),
                )?;
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
        // Get block events
        let events = self.client
            .runtime_api()
            .events_at(&block_ref.hash.into())?;

        // Check if any events match our target
        for event in events {
            if let EventRecord { 
                phase,
                event,
                topics,
            } = event {
                // Check if event is from watched module
                if event.module_name() == target.module_name.as_slice() {
                    // If watching for specific function
                    if event.function_name() == target.function_name.as_slice() {
                        // If watching for specific message hash
                        if let Some(msg_hash) = &target.message_hash {
                            // Check if event contains our message hash
                            if event.data().contains(msg_hash) {
                                return Ok(true);
                            }
                        } else {
                            // No specific message hash required
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// Generate state transition for matched target
    fn generate_state_transition(
        &self,
        block_ref: &BlockRef,
        target: &WatchTarget,
    ) -> Result<StateTransition> {
        // Get block state
        let state = self.client
            .runtime_api()
            .state_at(&block_ref.hash.into())?;

        // Get only the relevant state for this target
        let relevant_state = state
            .get_storage(target.module_name.as_slice())
            .filter(|s| {
                // Only include state relevant to the watched function
                s.function_name() == target.function_name.as_slice()
            });

        // Create minimal state transition
        Ok(StateTransition {
            pre_state: relevant_state.clone(),
            post_state: relevant_state,
            block_ref: block_ref.clone(),
            proof_type: ProofType::Merkle,
        })
    }
}

#[async_trait::async_trait]
impl<T: Config> FinalityObserver for FrostFinalityObserver<T> {
    async fn on_block_finalized(&self, block: BlockRef) -> Result<()> {
        // This gets called by the background worker
        let notification = FinalityNotification {
            hash: block.hash.clone(),
            number: block.number,
            justification: None, // Would be populated from actual notification
        };

        if let Err(e) = self.handle_finality_notification(notification).await {
            // Log error but don't fail
            error!("Error handling finality: {:?}", e);
        }
        Ok(())
    }
}

/// Background worker that subscribes to GRANDPA notifications
pub struct FrostWorker<T: Config> {
    observer: Arc<FrostFinalityObserver<T>>,
}

impl<T: Config> FrostWorker<T> {
    pub fn new(observer: Arc<FrostFinalityObserver<T>>) -> Self {
        Self { observer }
    }

    /// Start the worker
    pub async fn run(&self) {
        // Subscribe to GRANDPA finality notifications
        let mut finality_stream = // ... get finality notification stream

        while let Some(notification) = finality_stream.next().await {
            self.observer.handle_finality_notification(notification).await;
        };
    }
} 