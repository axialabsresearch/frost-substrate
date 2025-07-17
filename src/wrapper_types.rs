#![cfg_attr(not(feature = "std"), no_std)]
#![allow(unused_imports)]
#![allow(unused_variables)]

use frost_protocol::{
    state::{StateTransition, StateProof, ChainId, StateRoot, transition::TransitionMetadata},
    message::FrostMessage,
};
use parity_scale_codec::{Encode, Decode};
use scale_info::TypeInfo;
use serde::{Serialize, Deserialize};
use serde_json::Value;

/// SSMP-specific state transition wrapper
#[derive(Clone, Debug, Encode, Decode, TypeInfo)]
pub struct SSMPStateTransition(pub StateTransition);

impl SSMPStateTransition {
    pub fn new(
        chain_id: ChainId,
        block_height: u64,
        pre_state: StateRoot,
        post_state: StateRoot,
        module_name: Vec<u8>,
        function_name: Vec<u8>,
        target_chain: Vec<u8>,
    ) -> Self {
        let metadata = TransitionMetadata {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            version: 0,
            proof_type: frost_protocol::state::proof::ProofType::Basic,
            chain_specific: Some(serde_json::json!({
                "module_name": String::from_utf8_lossy(&module_name),
                "function_name": String::from_utf8_lossy(&function_name),
                "target_chain": String::from_utf8_lossy(&target_chain),
            })),
        };

        Self(StateTransition {
            chain_id,
            block_height,
            pre_state,
            post_state,
            transition_proof: None,
            metadata,
        })
    }
}

impl From<SSMPStateTransition> for StateTransition {
    fn from(ssmp: SSMPStateTransition) -> Self {
        ssmp.0
    }
}

impl AsRef<StateTransition> for SSMPStateTransition {
    fn as_ref(&self) -> &StateTransition {
        &self.0
    }
}

/// Substrate wrapper for StateProof
#[derive(Clone, Debug, Encode, Decode, TypeInfo)]
pub struct SubstrateStateProof(pub StateProof);

impl From<StateProof> for SubstrateStateProof {
    fn from(proof: StateProof) -> Self {
        Self(proof)
    }
}

impl AsRef<StateProof> for SubstrateStateProof {
    fn as_ref(&self) -> &StateProof {
        &self.0
    }
}

/// Substrate wrapper for FrostMessage
#[derive(Clone, Debug, Encode, Decode, TypeInfo)]
pub struct SubstrateFrostMessage(pub FrostMessage);

impl From<FrostMessage> for SubstrateFrostMessage {
    fn from(message: FrostMessage) -> Self {
        Self(message)
    }
}

impl AsRef<FrostMessage> for SubstrateFrostMessage {
    fn as_ref(&self) -> &FrostMessage {
        &self.0
    }
} 