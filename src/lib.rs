#![cfg_attr(not(feature = "std"), no_std)]

use std::{sync::Arc, marker::PhantomData};
use frame_support::{
    pallet_prelude::*,
    traits::Get,
};
use frame_system::pallet_prelude::*;
use sp_runtime::traits::{Hash, Block as BlockT};
use sp_std::{prelude::*, vec::Vec};
use sp_consensus_grandpa::AuthorityList;
use sp_api::ProvideRuntimeApi;

use crate::{
    observer::{FrostFinalityObserver, FrostWorker, WatchTarget},
    routing::SubstrateRouter,
    proof::MerkleProofGenerator,
};

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

        /// Router configuration
        type RouterConfig: Get<routing::RouterConfig>;

        /// Client type for runtime API access
        type Block: BlockT;
        type Client: ProvideRuntimeApi<Self::Block> + Send + Sync + 'static;
    }

    #[pallet::pallet]
    #[pallet::generate_store(pub(super) trait Store)]
    #[pallet::storage_version(STORAGE_VERSION)]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    pub type WatchTargets<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::FrostHash,
        WatchTarget,
        OptionQuery
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
        /// Message routed to target chain
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
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(_n: BlockNumberFor<T>) -> Weight {
            // Initialize observer if needed
            Self::ensure_observer();
            Weight::zero()
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
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
            
            // Store target
            WatchTargets::<T>::insert(target_hash, target.clone());
            
            // Register with observer
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

    impl<T: Config> Pallet<T> {
        /// Get or create observer instance
        fn ensure_observer() -> Option<Arc<FrostFinalityObserver<T>>> {
            static mut OBSERVER: Option<Arc<FrostFinalityObserver<T>>> = None;
            
            unsafe {
                if OBSERVER.is_none() {
                    let client = T::Client::get();
                    let authority_set = Arc::new(AuthorityList::default());
                    let proof_generator = Arc::new(MerkleProofGenerator::new(client.clone()));
                    let router = Arc::new(SubstrateRouter::new(T::RouterConfig::get()));

                    let observer = Arc::new(FrostFinalityObserver::new(
                        client,
                        authority_set,
                        proof_generator,
                        router,
                    ));

                    let worker = FrostWorker::new(observer.clone());
                    tokio::spawn(async move { worker.run().await });

                    OBSERVER = Some(observer);
                }
                OBSERVER.clone()
            }
        }

        /// Get observer instance
        fn observer() -> Option<Arc<FrostFinalityObserver<T>>> {
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
