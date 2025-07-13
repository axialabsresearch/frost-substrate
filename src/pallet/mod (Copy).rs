use frame_support::{
    decl_module, decl_storage, decl_event, decl_error,
    dispatch::DispatchResult,
    ensure,
    traits::{Get, EnsureOrigin},
};
use frame_system::{self as system, ensure_signed};
use sp_std::prelude::*;
use frost_protocol::{
    state::{StateTransition, StateProof, BlockRef, proof::ProofType},
    message::FrostMessage,
};
use sp_runtime::traits::{Hash, Member, Block as BlockT};
use sp_consensus_grandpa::AuthorityList as AuthoritySet;
use sp_api::ProvideRuntimeApi;
use codec::{Encode, Decode};
use serde::{Serialize, Deserialize};
use std::sync::Arc;

// Local imports
use crate::finality::{GrandpaFinality, SubstrateVerificationClient};
use crate::proof::{
    ProofGenerator,
    MerkleProofGenerator,
    BLSProofGenerator,
};

/// Configuration trait for FROST pallet
pub trait Config: frame_system::Config {
    /// The overarching event type
    type RuntimeEvent: From<Event<Self>> + Into<<Self as frame_system::Config>::RuntimeEvent>;
    
    /// Type representing the weight of an extrinsic
    type WeightInfo: Get<u32>;
    
    /// Maximum size of generated proofs
    type MaxProofSize: Get<u32>;
    
    /// Client type for runtime API access
    type Client: ProvideRuntimeApi<Self::Block> + Send + Sync;
    
    /// Block type
    type Block: Member + Encode + Decode;
    
    /// Authority set type for GRANDPA
    type AuthoritySet: AuthoritySet<<Self::Block as BlockT>::Hash, <Self::Block as BlockT>::Number>;
}

/// Proof generation configuration
#[derive(Clone, Encode, Decode, Serialize, Deserialize)]
pub struct ProofGenerationConfig {
    pub proof_type: ProofType,
    pub max_size: u32,
    pub timeout: u32,
}

impl Default for ProofGenerationConfig {
    fn default() -> Self {
        Self {
            proof_type: ProofType::Merkle,
            max_size: 1024 * 1024, // 1MB
            timeout: 30, // 30 seconds
        }
    }
}

decl_storage! {
    trait Store for Module<T: Config> as Frost {
        /// Mapping of processed state transitions
        Transitions get(fn transitions): map hasher(blake2_128_concat) T::Hash => StateTransition;
        
        /// Mapping of generated proofs
        Proofs get(fn proofs): map hasher(blake2_128_concat) T::Hash => StateProof;
        
        /// Mapping of verified messages
        Messages get(fn messages): map hasher(blake2_128_concat) T::Hash => FrostMessage;
        
        /// Latest processed block reference
        LatestBlock get(fn latest_block): Option<BlockRef>;

        /// Proof generation configuration
        ProofConfig get(fn proof_config) config(): ProofGenerationConfig;
    }
}

decl_event!(
    pub enum Event<T: Config> {
        /// New state transition recorded
        TransitionRecorded(<T as frame_system::Config>::Hash),
        /// New proof generated
        ProofGenerated(<T as frame_system::Config>::Hash, ProofType),
        /// Message verified
        MessageVerified(<T as frame_system::Config>::Hash),
        /// State finalized
        StateFinalized(BlockRef),
    }
);

decl_error! {
    pub enum Error for Module<T: Config> {
        /// Invalid state transition
        InvalidTransition,
        /// Invalid proof
        InvalidProof,
        /// Invalid message
        InvalidMessage,
        /// Block not finalized
        BlockNotFinalized,
        /// Proof too large
        ProofTooLarge,
        /// Unsupported proof type
        UnsupportedProofType,
        /// Finality verification failed
        FinalityVerificationFailed,
    }
}

decl_module! {
    pub struct Module<T: Config> for enum Call where origin: T::Origin {
        type Error = Error<T>;

        fn deposit_event() = default;

        /// Record a new state transition
        #[weight = T::WeightInfo::get()]
        pub fn record_transition(
            origin,
            transition: StateTransition,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            
            // Validate transition
            ensure!(transition.validate(), Error::<T>::InvalidTransition);
            
            // Verify finality using GRANDPA
            let finality_client = SubstrateVerificationClient::new(
                T::Client::get(),
                T::AuthoritySet::get(),
                Default::default(),
            );
            
            let finality = GrandpaFinality::new(
                T::Client::get(),
                Default::default(),
                Arc::new(finality_client),
            );

            // Check if source block is finalized
            ensure!(
                finality.is_final(&transition.pre_state.block_ref)
                    .map_err(|_| Error::<T>::FinalityVerificationFailed)?,
                Error::<T>::BlockNotFinalized
            );
            
            // Generate hash
            let transition_hash = T::Hashing::hash_of(&transition);
            
            // Store transition
            Transitions::<T>::insert(transition_hash, transition);
            
            // Emit event
            Self::deposit_event(Event::TransitionRecorded(transition_hash));
            
            Ok(())
        }

        /// Generate proof for a state transition
        #[weight = T::WeightInfo::get()]
        pub fn generate_proof(
            origin,
            transition_hash: T::Hash,
            proof_type: ProofType,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            
            // Get transition
            let transition = Transitions::<T>::get(transition_hash)
                .ok_or(Error::<T>::InvalidTransition)?;
            
            // Get proof generator based on type
            let generator: Box<dyn ProofGenerator> = match proof_type {
                ProofType::Merkle => Box::new(MerkleProofGenerator::new(
                    T::Client::get(),
                )),
                ProofType::Signature => Box::new(BLSProofGenerator::new(
                    T::Client::get(),
                    T::AuthoritySet::get(),
                )),
                _ => return Err(Error::<T>::UnsupportedProofType.into()),
            };

            // Get proof config
            let mut config = ProofConfig::get();
            config.proof_type = proof_type;
            
            // Generate proof
            let proof = generator
                .generate_proof(&transition, &config)
                .map_err(|_| Error::<T>::InvalidProof)?;
            
            // Validate proof size
            ensure!(
                proof.encoded_size() <= T::MaxProofSize::get() as usize,
                Error::<T>::ProofTooLarge
            );
            
            // Store proof
            Proofs::<T>::insert(transition_hash, proof.clone());
            
            // Emit event
            Self::deposit_event(Event::ProofGenerated(transition_hash, proof_type));
            
            Ok(())
        }

        /// Verify a FROST message
        #[weight = T::WeightInfo::get()]
        pub fn verify_message(
            origin,
            message: FrostMessage,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            
            // Validate message
            ensure!(message.validate(), Error::<T>::InvalidMessage);
            
            // Verify proof in message
            let generator: Box<dyn ProofGenerator> = match message.proof.proof_type() {
                ProofType::Merkle => Box::new(MerkleProofGenerator::new(
                    T::Client::get(),
                )),
                ProofType::Signature => Box::new(BLSProofGenerator::new(
                    T::Client::get(),
                    T::AuthoritySet::get(),
                )),
                _ => return Err(Error::<T>::UnsupportedProofType.into()),
            };

            // Verify proof
            ensure!(
                generator.verify_proof(&message.proof, &ProofConfig::get())
                    .map_err(|_| Error::<T>::InvalidProof)?,
                Error::<T>::InvalidProof
            );
            
            // Generate hash
            let message_hash = T::Hashing::hash_of(&message);
            
            // Store message
            Messages::<T>::insert(message_hash, message);
            
            // Emit event
            Self::deposit_event(Event::MessageVerified(message_hash));
            
            Ok(())
        }
    }
}

impl<T: Config> Module<T> {
    /// Internal function to update latest block
    fn update_latest_block(block_ref: BlockRef) {
        LatestBlock::put(block_ref);
        Self::deposit_event(Event::StateFinalized(block_ref));
    }
} 