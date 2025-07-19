use sp_api::ApiExt;
use frame_system::EventRecord;
use frame_support::pallet_prelude::Member;
use sp_runtime::traits::Hash as HashT;
use parity_scale_codec::{Encode, Decode};
use scale_info::TypeInfo;
use sp_runtime::traits::Block as BlockT;

sp_api::decl_runtime_apis! {
    pub trait SSMPApi<PalletRuntimeEvent, SystemHash> 
    where
        PalletRuntimeEvent: Member 
            + Encode 
            + Decode 
            + TypeInfo 
            + parity_scale_codec::EncodeLike,
        SystemHash: HashT,
        Vec<EventRecord<PalletRuntimeEvent, SystemHash>>: Decode,
    {
        fn events(at: <Block as BlockT>::Hash) -> Vec<EventRecord<PalletRuntimeEvent, SystemHash>>;
    }
}