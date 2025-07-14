#![cfg_attr(not(feature = "std"), no_std)]
#![allow(unused_imports)]
#![allow(unused_variables)]

use frost_protocol::{
    state::{StateTransition, StateProof},
    message::FrostMessage,
};
use parity_scale_codec::{Encode, Decode};
use scale_info::TypeInfo;

/// Substrate wrapper for StateTransition
#[derive(Clone, Debug, Encode, Decode, TypeInfo)]
pub struct SubstrateStateTransition(pub StateTransition);

impl From<StateTransition> for SubstrateStateTransition {
    fn from(transition: StateTransition) -> Self {
        Self(transition)
    }
}

impl AsRef<StateTransition> for SubstrateStateTransition {
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