// FROST Substrate Implementation
//! This crate provides a Substrate implementation of the FROST protocol,
//! enabling cross-chain state verification using GRANDPA finality.

#![allow(unused_imports)]
#![allow(unused_variables)]
#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::{
    pallet_prelude::*,
    traits::Get,
};
use frame_system::pallet_prelude::*;
use sp_runtime::traits::{Hash, BlakeTwo256};
use sp_std::{prelude::*, vec::Vec};
use scale_info::TypeInfo;

// Module declarations
pub mod finality;
pub mod verification;
pub mod proof;
pub mod wrapper_types;

// Re-exports
pub use finality::GrandpaFinality;
pub use verification::*;
pub use proof::*;
pub use wrapper_types::*;

// Import types from frost-protocol
use frost_protocol::state::{
    StateTransition,
    StateProof,
    BlockRef,
    proof::ProofType,
};
use frost_protocol::message::FrostMessage;

/// Wrapper type for state transitions
#[derive(Clone, Encode, Decode, RuntimeDebug, TypeInfo)]
pub struct SubstrateStateTransition(StateTransition);

impl From<StateTransition> for SubstrateStateTransition {
    fn from(transition: StateTransition) -> Self {
        Self(transition)
    }
}

/// Wrapper type for state proofs
#[derive(Clone, Encode, Decode, RuntimeDebug, TypeInfo)]
pub struct SubstrateStateProof(StateProof);

impl From<StateProof> for SubstrateStateProof {
    fn from(proof: StateProof) -> Self {
        Self(proof)
    }
}

/// Wrapper type for FROST messages
#[derive(Clone, Encode, Decode, RuntimeDebug, TypeInfo)]
pub struct SubstrateFrostMessage(FrostMessage);

impl From<FrostMessage> for SubstrateFrostMessage {
    fn from(msg: FrostMessage) -> Self {
        Self(msg)
    }
}

#[frame_support::pallet]
pub mod frost_pallet {
    use super::*;

    pub const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

    #[pallet::config]
    pub trait Config: frame_system::Config {
        /// The overarching event type
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        
        /// Type for block hash
        type FrostHash: Hash + Member + Parameter + MaxEncodedLen + TypeInfo + From<<Self::FrostHash as Hash>::Output> + Clone;
        
        /// Minimum number of block confirmations required
        #[pallet::constant]
        type MinConfirmations: Get<u32>;
        
        /// Maximum size of proof data
        #[pallet::constant]
        type MaxProofSize: Get<u32>;
    }

    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    #[pallet::storage_version(STORAGE_VERSION)]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    pub type Transitions<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::FrostHash,
        SubstrateStateTransition,
        OptionQuery
    >;

    #[pallet::storage]
    pub type Proofs<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::FrostHash,
        SubstrateStateProof,
        OptionQuery
    >;

    #[pallet::storage]
    pub type Messages<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::FrostHash,
        SubstrateFrostMessage,
        OptionQuery
    >;

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// New state transition processed
        TransitionProcessed {
            transition_hash: T::FrostHash,
        },
        /// New proof generated
        ProofGenerated {
            proof_hash: T::FrostHash,
        },
        /// New message received
        MessageReceived {
            msg_hash: T::FrostHash,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        /// Invalid state transition
        InvalidTransition,
        /// Invalid proof
        InvalidProof,
        /// Invalid message
        InvalidMessage,
        /// Proof size exceeds limit
        ProofTooLarge,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::weight(10_000)]
        pub fn submit_transition(
            origin: OriginFor<T>,
            transition: StateTransition,
        ) -> DispatchResult {
            ensure_signed(origin)?;
            
            // Convert to substrate type
            let substrate_transition = SubstrateStateTransition::from(transition);
            
            // Generate transition hash
            let transition_hash = T::FrostHash::hash(&substrate_transition.encode());
            let key_hash = T::FrostHash::from(transition_hash);
            
            // Store transition
            Transitions::<T>::insert(key_hash.clone(), substrate_transition);
            
            // Emit event
            Self::deposit_event(Event::TransitionProcessed { 
                transition_hash: key_hash.clone() 
            });
            
            Ok(())
        }

        #[pallet::weight(10_000)]
        pub fn submit_proof(
            origin: OriginFor<T>,
            proof: StateProof,
        ) -> DispatchResult {
            ensure_signed(origin)?;
            
            // Convert to substrate type
            let substrate_proof = SubstrateStateProof::from(proof);
            
            // Check proof size
            ensure!(
                substrate_proof.0.proof.data.len() <= T::MaxProofSize::get() as usize,
                Error::<T>::ProofTooLarge
            );
            
            // Generate proof hash
            let proof_hash = T::FrostHash::hash(&substrate_proof.encode());
            let key_hash = T::FrostHash::from(proof_hash);
            
            // Store proof
            Proofs::<T>::insert(key_hash.clone(), substrate_proof);
            
            // Emit event
            Self::deposit_event(Event::ProofGenerated { 
                proof_hash: key_hash.clone() 
            });
            
            Ok(())
        }

        #[pallet::weight(10_000)]
        pub fn submit_message(
            origin: OriginFor<T>,
            message: FrostMessage,
        ) -> DispatchResult {
            ensure_signed(origin)?;
            
            // Convert to substrate type
            let substrate_message = SubstrateFrostMessage::from(message);
            
            // Generate message hash
            let msg_hash = T::FrostHash::hash(&substrate_message.encode());
            let key_hash = T::FrostHash::from(msg_hash);
            
            // Store message
            Messages::<T>::insert(key_hash.clone(), substrate_message);
            
            // Emit event
            Self::deposit_event(Event::MessageReceived { 
                msg_hash: key_hash.clone() 
            });
            
            Ok(())
        }
    }
}

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;
