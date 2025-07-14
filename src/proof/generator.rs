#![allow(unused_imports)]
#![allow(unused_variables)]

use frost_protocol::state::{
    StateTransition, 
    StateProof, 
    StateError,
    proof::{ProofType, ProofData},
};
use sp_runtime::{
    traits::{Block as BlockT, Header as HeaderT},
    generic::BlockId,
};
use sp_core::H256;
use sp_consensus_grandpa::AuthorityList;
use sp_blockchain::HeaderBackend;
use sp_runtime::traits::NumberFor;
use parity_scale_codec::{Encode, Decode};
use serde::{Serialize, Deserialize};
use serde_json::{self, json};
use rs_merkle::{MerkleTree, Hasher, algorithms::Sha256};
use std::{sync::Arc, time::SystemTime};
use hex;

/// SHA256 hasher for Merkle tree
#[derive(Default, Clone)]
struct MerkleHasher;

impl Hasher for MerkleHasher {
    type Hash = [u8; 32];

    fn hash(data: &[u8]) -> Self::Hash {
        use sha2::{Sha256 as Sha256Hash, Digest};
        let mut hasher = Sha256Hash::new();
        hasher.update(data);
        hasher.finalize().into()
    }
}

/// Proof generation configuration
#[derive(Debug, Clone)]
pub struct ProofGenerationConfig {
    /// Type of proof to generate
    pub proof_type: ProofType,
    /// Maximum proof size in bytes
    pub max_proof_size: u32,
    /// Proof expiration time in seconds
    pub proof_ttl: u64,
}

impl Default for ProofGenerationConfig {
    fn default() -> Self {
        Self {
            proof_type: ProofType::Custom("Merkle".to_string()),
            max_proof_size: 1024 * 1024, // 1MB
            proof_ttl: 3600 * 24, // 24 hours
        }
    }
}

/// Core proof generator trait
#[async_trait::async_trait]
pub trait ProofGenerator: Send + Sync {
    /// Generate proof for state transition
    async fn generate_proof(
        &self,
        transition: &StateTransition,
        config: &ProofGenerationConfig,
    ) -> Result<StateProof, StateError>;

    /// Get supported proof types
    fn supported_types(&self) -> Vec<ProofType>;
}

/// Merkle proof generator implementation
pub struct MerkleProofGenerator<C, B> {
    client: Arc<C>,
    _phantom: std::marker::PhantomData<B>,
}

impl<C, B> MerkleProofGenerator<C, B> {
    pub fn new(client: Arc<C>) -> Self {
        Self {
            client,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Generate Merkle tree from state data
    fn generate_merkle_tree(&self, data: &[u8]) -> Result<MerkleTree<MerkleHasher>, StateError> {
        // Split data into 32-byte chunks
        let chunks: Vec<[u8; 32]> = data
            .chunks(32)
            .map(|chunk| {
                let mut buffer = [0u8; 32];
                buffer[..chunk.len()].copy_from_slice(chunk);
                buffer
            })
            .collect();

        // Create Merkle tree
        Ok(MerkleTree::<MerkleHasher>::from_leaves(&chunks))
    }
}

#[async_trait::async_trait]
impl<C, B> ProofGenerator for MerkleProofGenerator<C, B>
where
    C: HeaderBackend<B> + Send + Sync,
    B: BlockT<Hash = H256>,
    B::Header: HeaderT<Number = u32>,
{
    async fn generate_proof(
        &self,
        transition: &StateTransition,
        config: &ProofGenerationConfig,
    ) -> Result<StateProof, StateError> {
        // Ensure we support the requested proof type
        if config.proof_type != ProofType::Custom("Merkle".to_string()) {
            return Err(StateError::Internal("Unsupported proof type".into()));
        }

        // Convert block hashes to H256
        let source_hash = H256::from_slice(&transition.pre_state.block_ref.hash);
        let target_hash = H256::from_slice(&transition.post_state.block_ref.hash);

        // Get block headers for source and target
        let source_header = self.client
            .header(source_hash)
            .map_err(|e| StateError::Internal(format!("Failed to get source header: {}", e)))?
            .ok_or_else(|| StateError::Internal("Source block not found".into()))?;

        let target_header = self.client
            .header(target_hash)
            .map_err(|e| StateError::Internal(format!("Failed to get target header: {}", e)))?
            .ok_or_else(|| StateError::Internal("Target block not found".into()))?;

        // Generate Merkle tree from state data
        let merkle_tree = self.generate_merkle_tree(&transition.transition_proof.clone().unwrap_or_default())?;

        // Get root hash
        let root = merkle_tree.root().ok_or_else(|| {
            StateError::Internal("Failed to get Merkle root".into())
        })?;

        // Create proof data
        let proof_data = ProofData {
            proof_type: ProofType::Custom("Merkle".to_string()),
            data: root.to_vec(),
            metadata: Some(serde_json::to_value({
                let metadata = json!({
                    "source_block": {
                        "number": source_header.number(),
                        "hash": hex::encode(source_header.hash()),
                        "parent_hash": hex::encode(source_header.parent_hash()),
                    },
                    "target_block": {
                        "number": target_header.number(),
                        "hash": hex::encode(target_header.hash()),
                        "parent_hash": hex::encode(target_header.parent_hash()),
                    },
                    "merkle_root": hex::encode(&root),
                });
                metadata
            }).map_err(|e| StateError::Internal(format!("Failed to serialize metadata: {}", e)))?,),
            generated_at: SystemTime::now(),
            expires_at: Some(
                SystemTime::now() + std::time::Duration::from_secs(config.proof_ttl)
            ),
            version: 1,
        };

        // Validate proof size
        let encoded = serde_json::to_vec(&proof_data)
            .map_err(|e| StateError::Internal(format!("Failed to encode proof: {}", e)))?;
        if encoded.len() as u32 > config.max_proof_size {
            return Err(StateError::Internal(format!(
                "Proof size {} exceeds maximum allowed size {}",
                encoded.len(),
                config.max_proof_size
            )));
        }

        // Create state proof
        Ok(StateProof::new(transition.clone(), proof_data))
    }

    fn supported_types(&self) -> Vec<ProofType> {
        vec![ProofType::Custom("Merkle".to_string())]
    }
}

/// BLS signature proof generator implementation
pub struct BLSProofGenerator<C, B> {
    client: Arc<C>,
    authority_set: Arc<AuthorityList>,
    _phantom: std::marker::PhantomData<B>,
}

impl<C, B> BLSProofGenerator<C, B> {
    pub fn new(
        client: Arc<C>,
        authority_set: Arc<AuthorityList>,
    ) -> Self {
        Self {
            client,
            authority_set,
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<C, B> ProofGenerator for BLSProofGenerator<C, B>
where
    C: HeaderBackend<B> + Send + Sync,
    B: BlockT<Hash = H256>,
    B::Header: HeaderT<Number = u32>,
{
    async fn generate_proof(
        &self,
        transition: &StateTransition,
        config: &ProofGenerationConfig,
    ) -> Result<StateProof, StateError> {
        // Ensure we support the requested proof type
        if config.proof_type != ProofType::Signature {
            return Err(StateError::Internal("Unsupported proof type".into()));
        }

        // Convert block hashes to H256
        let source_hash = H256::from_slice(&transition.pre_state.block_ref.hash);
        let target_hash = H256::from_slice(&transition.post_state.block_ref.hash);

        // Get block headers
        let source_header = self.client
            .header(source_hash)
            .map_err(|e| StateError::Internal(format!("Failed to get source header: {}", e)))?
            .ok_or_else(|| StateError::Internal("Source block not found".into()))?;

        let target_header = self.client
            .header(target_hash)
            .map_err(|e| StateError::Internal(format!("Failed to get target header: {}", e)))?
            .ok_or_else(|| StateError::Internal("Target block not found".into()))?;

        // Create signature data
        let signature_data = {
            let mut data = Vec::new();
            data.extend_from_slice(&transition.pre_state.block_ref.hash);
            data.extend_from_slice(&transition.post_state.block_ref.hash);
            data
        };

        // Generate BLS signature
        // Note: This is a placeholder. In practice, you would:
        // 1. Get the actual BLS signatures from validators
        // 2. Aggregate the signatures
        // 3. Include validator set info
        let bls_signature = vec![0u8; 96]; // Placeholder 96-byte BLS signature

        // Create proof data
        let proof_data = ProofData {
            proof_type: ProofType::Signature,
            data: bls_signature,
            metadata: Some(serde_json::to_value({
                let metadata = json!({
                    "validator_set": self.authority_set
                        .iter()
                        .map(|(id, weight)| {
                            json!({
                                "id": hex::encode(id),
                                "weight": weight,
                            })
                        })
                        .collect::<Vec<_>>(),
                    "source_block": {
                        "number": source_header.number(),
                        "hash": hex::encode(source_header.hash()),
                        "parent_hash": hex::encode(source_header.parent_hash()),
                    },
                    "target_block": {
                        "number": target_header.number(),
                        "hash": hex::encode(target_header.hash()),
                        "parent_hash": hex::encode(target_header.parent_hash()),
                    },
                });
                metadata
            }).map_err(|e| StateError::Internal(format!("Failed to serialize metadata: {}", e)))?,),
            generated_at: SystemTime::now(),
            expires_at: Some(
                SystemTime::now() + std::time::Duration::from_secs(config.proof_ttl)
            ),
            version: 1,
        };

        // Validate proof size
        let encoded = serde_json::to_vec(&proof_data)
            .map_err(|e| StateError::Internal(format!("Failed to encode proof: {}", e)))?;
        if encoded.len() as u32 > config.max_proof_size {
            return Err(StateError::Internal(format!(
                "Proof size {} exceeds maximum allowed size {}",
                encoded.len(),
                config.max_proof_size
            )));
        }

        // Create state proof
        Ok(StateProof::new(transition.clone(), proof_data))
    }

    fn supported_types(&self) -> Vec<ProofType> {
        vec![ProofType::Signature]
    }
} 