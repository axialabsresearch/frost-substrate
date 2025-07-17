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

use crate::{
    observer::{SSMPObserver, SSMPWorker, WatchTarget},
    proof::MerkleProofGenerator,
    routing::RouterAdapter,
};
use frost_protocol::routing::RoutingConfig;

#[frame_support::pallet]
pub mod ssmp_pallet {
    use super::*;

    pub const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

    #[pallet::config]
    pub trait Config: frame_system::Config {
        /// The overarching event type
        type RuntimeEvent: Parameter 
            + Member 
            + From<Event<Self>> 
            + IsType<<Self as frame_system::Config>::RuntimeEvent>
            + std::fmt::Debug
            + Clone
            + PartialEq
            + Eq
            + EncodeLike
            + TypeInfo
            + Send
            + Sync;

        /// Type for block hash
        type FrostHash: Hash + Member + Parameter + MaxEncodedLen + TypeInfo + From<<Self::FrostHash as Hash>::Output> + Clone + Copy;

        /// The block type - must be the same as frame_system::Config::Block
        type Block: BlockT<Hash = H256> + HeaderT;

        /// Client type for runtime API access
        type Client: HeaderBackend<<Self as Config>::Block>
            + BlockBackend<<Self as Config>::Block>
            + Send 
            + Sync 
            + Default
            + 'static;

        /// Optional message router type
        /// Set to () to disable routing functionality
        type MessageRouter: Into<Option<Arc<RouterAdapter<Self::NetworkProtocol>>>> + Default + 'static;

        /// Network protocol implementation
        type NetworkProtocol: frost_protocol::NetworkProtocol + Default + 'static;
    }

    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    #[pallet::storage_version(STORAGE_VERSION)]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    pub type WatchTargets<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::FrostHash,
        WatchTarget,
        OptionQuery,
        GetDefault
    >;

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
        T: Send + Sync,
        T::Client: Clone,
        T::MessageRouter: Into<Option<Arc<RouterAdapter<T::NetworkProtocol>>>>,
        <<T as Config>::Block as BlockT>::Header: HeaderT<Number = u32>,
        NumberFor<<T as Config>::Block>: Into<u64> + From<u64> + SaturatedConversion + Saturating + Zero + Copy,
    {
        fn on_initialize(_n: BlockNumberFor<T>) -> Weight {
            // Initialize observer if needed
            let _ = Self::ensure_observer();
            Weight::zero()
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T>
    where
        T: Send + Sync,
        T::Client: Clone,
        T::MessageRouter: Into<Option<Arc<RouterAdapter<T::NetworkProtocol>>>>,
        <<T as Config>::Block as BlockT>::Header: HeaderT<Number = u32>,
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
                target_hash: target_hash,
            });
            
            Ok(())
        }
    }

    impl<T> Pallet<T>
    where
        T: Config + Send + Sync,
        T::Client: Clone,
        T::MessageRouter: Into<Option<Arc<RouterAdapter<T::NetworkProtocol>>>>,
        <<T as Config>::Block as BlockT>::Header: HeaderT<Number = u32>,
        NumberFor<<T as Config>::Block>: Into<u64> + From<u64> + SaturatedConversion + Saturating + Zero + Copy,
    {
        /// Get or create observer instance
        fn ensure_observer() -> Option<Arc<SSMPObserver<T>>> {
            type ObserverMap = HashMap<TypeId, Box<dyn std::any::Any + Send + Sync>>;
            static OBSERVERS: Lazy<RwLock<ObserverMap>> = Lazy::new(|| RwLock::new(HashMap::new()));
            
            let type_id = TypeId::of::<T>();
            
            let mut observers = match OBSERVERS.write() {
                Ok(guard) => guard,
                Err(_) => return None,
            };
            
            if let Some(observer) = observers.get(&type_id) {
                if let Some(observer) = observer.downcast_ref::<Arc<SSMPObserver<T>>>() {
                    return Some(observer.clone());
                }
            }
            
            let client = T::Client::default();
            
            let proof_gen = Arc::new(MerkleProofGenerator::new(
                Arc::new(client.clone())
            ));
            
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
        fn observer() -> Option<Arc<SSMPObserver<T>>> {
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
