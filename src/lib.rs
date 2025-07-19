#![allow(unused_imports)]
#![allow(unused_variables)]

#![cfg_attr(not(feature = "std"), no_std)]
#![recursion_limit="1024"]

use sp_std::prelude::*;
use frame_support::{
    pallet_prelude::*,
    traits::Get,
    Parameter,
    weights::Weight,
    Blake2_128Concat,
    BoundedVec,
};
use frame_support::{
    pallet_prelude::{Member, ConstU32},
};
use frame_system::pallet_prelude::*;
use sp_runtime::{
    traits::{Hash, Block as BlockT, Header as HeaderT, NumberFor, Zero, SaturatedConversion, Saturating},
    generic::DigestItem,
    codec::{Encode, Decode, EncodeLike},
};
use sp_std::{vec::Vec, prelude::*};
use sp_consensus_grandpa::AuthorityList;
use sp_api::ProvideRuntimeApi;
use sp_core::H256;
use sp_blockchain::{HeaderBackend, Backend as BlockBackend};
use frost_protocol::routing::router::MessageRouter;
use scale_info::TypeInfo;
use once_cell::sync::Lazy;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use crate::runtime_api::SSMPApi;

use crate::{
    observer::{SSMPObserver, SSMPWorker, WatchTarget},
    proof::MerkleProofGenerator,
    routing::RouterAdapter,
};
use frost_protocol::routing::RoutingConfig;
// for use crate::runtime_api::SSMPApi;

mod wrapper_types;
pub use wrapper_types::{SSMPStateTransition, SubstrateStateProof, SubstrateFrostMessage};

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;

    pub const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// New watch target registered
        WatchTargetRegistered {
            target_hash: T::FrostHash,
        },
        /// State transition observed and proof generated
        ProofGenerated {
            proof_hash: T::FrostHash,
            target_chain: Vec<u8>,
        },
        /// Message routed to target chain (only if routing is enabled)
        #[cfg(feature = "routing")]
        MessageRouted {
            msg_hash: T::FrostHash,
            target_chain: Vec<u8>,
        },
    }

    #[pallet::config]
    pub trait Config: frame_system::Config {
        /// The overarching event type
        type RuntimeEvent: Parameter
            + Member
            + From<Event<Self>>
            + IsType<<Self as frame_system::Config>::RuntimeEvent>
            + Encode
            + Decode
            + TypeInfo
            + EncodeLike;

        /// Type for block hash
        type Hash: sp_api::HashT;

        /// Type for block hash
        type FrostHash: Hash + Member + Parameter + MaxEncodedLen + TypeInfo + From<<Self::FrostHash as Hash>::Output> + Clone + Copy;

        /// The block type - must be the same as frame_system::Config::Block
        type Block: BlockT<Hash = H256> + sp_api::HeaderT;

        /// Client type for runtime API access
        type Client: HeaderBackend<<Self as pallet::Config>::Block>
            + BlockBackend<<Self as pallet::Config>::Block>
            + ProvideRuntimeApi<<Self as pallet::Config>::Block>
            + Send
            + Sync
            + Default
            + Clone
            + 'static
        where
            <Self::Client as ProvideRuntimeApi<<Self as pallet::Config>::Block>>::Api:
                SSMPApi<<Self as pallet::Config>::Block, <Self as pallet::Config>::RuntimeEvent, <Self as pallet::Config>::Hash>
                + sp_api::Core<<Self as pallet::Config>::Block>
                + sp_consensus_grandpa::GrandpaApi<<Self as pallet::Config>::Block>;

        /// Optional message router type
        type MessageRouter: Into<Option<Arc<RouterAdapter<Self::NetworkProtocol>>>> + Default + 'static;

        /// Network protocol implementation
        type NetworkProtocol: frost_protocol::NetworkProtocol + Default + 'static;
    }


    #[pallet::pallet]
    #[pallet::storage_version(STORAGE_VERSION)]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    pub type WatchTargets<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::FrostHash,
        WatchTarget,
        OptionQuery
    >;

    #[pallet::error]
    pub enum Error<T> {
        /// Invalid watch target
        InvalidWatchTarget,
        /// Invalid proof
        InvalidProof,
        /// Proof size exceeds limit
        ProofTooLarge,
        /// Routing failed
        RoutingFailed,
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T>
    where
        <T as frame_system::Config>::Hash: sp_api::HashT,
        NumberFor<<T as Config>::Block>: Into<u64> + From<u64> + SaturatedConversion + Saturating + Zero + Copy,
    {
        // Initialize observer if needed
        fn on_initialize(_n: BlockNumberFor<T>) -> Weight {
            let _ = Self::ensure_observer();
            Weight::zero()
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T>
    where
        <T as frame_system::Config>::Hash: sp_api::HashT,
        NumberFor<<T as Config>::Block>: Into<u64> + From<u64> + SaturatedConversion + Saturating + Zero + Copy,
    {
        #[pallet::weight(10_000)]
        pub fn register_watch_target(
            origin: OriginFor<T>,
            target: WatchTarget,
        ) -> DispatchResult {
            ensure_signed(origin)?;
            
            // Validate target
            ensure!(
                !target.module_name.is_empty() && !target.function_name.is_empty(),
                Error::<T>::InvalidWatchTarget
            );
            
            // Generate target hash
            let target_hash = T::FrostHash::hash(&target.encode()).into();
            
            WatchTargets::<T>::insert(&target_hash, target.clone());
            
            if let Some(observer) = Self::observer() {
                observer.register_watch_target(target);
            }
            
            // Emit event
            Self::deposit_event(Event::WatchTargetRegistered {
                target_hash,
            });
            
            Ok(())
        }
    }

    impl<T: Config> Pallet<T>
    where
        T: Send + Sync,
        T::Client: Clone,
        T::MessageRouter: Into<Option<Arc<RouterAdapter<T::NetworkProtocol>>>>,
        <<T as Config>::Block as BlockT>::Header: HeaderT<Number = u32>,
        NumberFor<<T as Config>::Block>: Into<u64> + From<u64> + SaturatedConversion + Saturating + Zero + Copy,
        <T::Client as ProvideRuntimeApi<<T as Config>::Block>>::Api:
            SSMPApi<<T as Config>::Block, <T as pallet::Config>::RuntimeEvent, <T as pallet::Config>::Hash>
            + sp_api::Core<<T as Config>::Block>
            + sp_consensus_grandpa::GrandpaApi<<T as Config>::Block>,
            std::vec::Vec<frame_system::EventRecord<<T as pallet::Config>::RuntimeEvent, 
            <T as pallet::Config>::Hash>>: parity_scale_codec::Decode,
    {
        /// Get or create observer instance
        fn ensure_observer() -> Option<Arc<SSMPObserver<T>>> 
        where
            // Add the required API bound here
            <T::Client as ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api:
                SSMPApi<
                    <T as pallet::Config>::Block, 
                    <T as pallet::Config>::RuntimeEvent, 
                    <T as frame_system::Config>::Hash> 
                        + sp_api::Core<<T as pallet::Config>::Block>
                        + sp_consensus_grandpa::GrandpaApi<<T as pallet::Config>::Block>, 
                    <T as frame_system::Config>::Hash: sp_api::HashT,
        {
            static OBSERVERS: Lazy<RwLock<HashMap<TypeId, Box<dyn std::any::Any + Send + Sync>>>> = 
                Lazy::new(|| RwLock::new(HashMap::new()));
            
            let type_id = TypeId::of::<T>();
            let mut observers = OBSERVERS.write().ok()?;
            
            if let Some(observer) = observers.get(&type_id) {
                if let Some(observer) = observer.downcast_ref::<Arc<SSMPObserver<T>>>() {
                    return Some(observer.clone());
                }
            }
            
            let client = T::Client::default();
            let proof_gen = Arc::new(
                MerkleProofGenerator::<<T as pallet::Config>::Client, <T as pallet::Config>::Block>::new(
                    Arc::new(client.clone())
                )
            );
            let router = T::MessageRouter::default();

            let new_observer = Arc::new(SSMPObserver::new(
                Arc::new(client),
                proof_gen,
                router,
            ));
            
            observers.insert(type_id, Box::new(new_observer.clone()));
            Some(new_observer)
        }

        /// Get observer instance
        fn observer() -> Option<Arc<SSMPObserver<T>>>
        where
            <T::Client as ProvideRuntimeApi<<T as pallet::Config>::Block>>::Api:
                SSMPApi<
                    <T as pallet::Config>::Block, 
                    <T as pallet::Config>::RuntimeEvent, 
                    <T as frame_system::Config>::Hash> + sp_api::Core<<T as pallet::Config>::Block>,
                    <T as frame_system::Config>::Hash: sp_api::HashT,
        {
            // This function calls ensure_observer, so its bounds are implicitly handled by ensure_observer's bounds.
            // No explicit bounds needed here unless it has direct uses of T::Client that require specific API traits.
            Self::ensure_observer()
        }
    }
} 

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

pub mod routing;
pub mod observer;
pub mod proof;
pub mod finality;
pub mod runtime_api;
