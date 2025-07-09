use frost_protocol::{
    finality::{FinalityVerificationClient, FinalityVerificationError, Block, ChainRules},
    state::{BlockRef, StateError},
};
use sp_consensus_grandpa::{
    AuthorityList,
    AuthorityId,
    AuthorityWeight,
    GRANDPA_ENGINE_ID,
    GrandpaJustification,
};
use sp_runtime::{
    generic::BlockId,
    traits::{Block as BlockT, Header as HeaderT, NumberFor},
    Justification,
};
use sp_core::H256;
use codec::{Decode, Encode};
use std::{sync::Arc, time::Duration};

/// GRANDPA-specific verification parameters
#[derive(Debug, Clone)]
pub struct GrandpaVerificationParams {
    /// Minimum number of block confirmations
    pub min_confirmations: u32,
    /// Required validator participation (0.0 - 1.0)
    pub min_participation: f64,
    /// Maximum allowed fork depth
    pub max_fork_depth: u32,
    /// Verification timeout
    pub timeout: Duration,
}

impl Default for GrandpaVerificationParams {
    fn default() -> Self {
        Self {
            min_confirmations: 2,
            min_participation: 0.67, // 2/3 validator participation required
            max_fork_depth: 50,
            timeout: Duration::from_secs(30),
        }
    }
}

/// Substrate-specific verification client
pub struct SubstrateVerificationClient<C, B: BlockT> {
    client: Arc<C>,
    authority_set: Arc<AuthoritySet<B::Hash, NumberFor<B>>>,
    params: GrandpaVerificationParams,
}

impl<C, B: BlockT> SubstrateVerificationClient<C, B> {
    pub fn new(
        client: Arc<C>,
        authority_set: Arc<AuthoritySet<B::Hash, NumberFor<B>>>,
        params: GrandpaVerificationParams,
    ) -> Self {
        Self {
            client,
            authority_set,
            params,
        }
    }

    /// Verify GRANDPA justification
    async fn verify_justification(
        &self,
        justification: GrandpaJustification<B>,
    ) -> Result<bool, FinalityVerificationError> {
        // Get current authority set
        let authority_set = self.authority_set.current_authorities();
        
        // Verify justification signatures
        let signed_weight = justification
            .commit
            .precommits
            .iter()
            .filter_map(|signed| {
                if signed.verify_signature(&authority_set) {
                    Some(authority_set.weight(&signed.id))
                } else {
                    None
                }
            })
            .sum::<AuthorityWeight>();

        // Calculate total weight
        let total_weight: AuthorityWeight = authority_set
            .iter()
            .map(|(_, weight)| weight)
            .sum();

        // Check if we have enough participation
        let participation = signed_weight as f64 / total_weight as f64;
        if participation < self.params.min_participation {
            return Ok(false);
        }

        Ok(true)
    }

    /// Get block finality status
    async fn get_finality_status(
        &self,
        block_hash: B::Hash,
    ) -> Result<FinalityStatus, FinalityVerificationError> {
        // Get block header
        let header = self.client
            .header(BlockId::Hash(block_hash))?
            .ok_or_else(|| FinalityVerificationError("Block not found".into()))?;

        // Get latest finalized block
        let latest_finalized = self.client.info().finalized_number;

        // Calculate confirmations
        let confirmations = latest_finalized.saturating_sub(header.number());

        // Get justification if available
        let justification = self.client
            .justification(BlockId::Hash(block_hash))?;

        Ok(FinalityStatus {
            block_number: header.number(),
            confirmations,
            has_justification: justification.is_some(),
            is_finalized: confirmations >= self.params.min_confirmations.into(),
        })
    }
}

#[async_trait::async_trait]
impl<C, B: BlockT> FinalityVerificationClient for SubstrateVerificationClient<C, B>
where
    C: HeaderBackend<B> + BlockBackend<B> + Send + Sync,
    B: BlockT<Hash = H256>,
{
    async fn verify_block_hash(&self, block_ref: &BlockRef) -> Result<bool, StateError> {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        // Check if block exists
        let header = self.client
            .header(BlockId::Hash(block_hash))
            .map_err(|e| StateError::Internal(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| StateError::Internal("Block not found".into()))?;

        // Verify block number matches
        if header.number() != block_ref.number.into() {
            return Ok(false);
        }

        Ok(true)
    }

    async fn get_finality_confidence(&self, block_ref: &BlockRef) -> Result<f64, StateError> {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        // Get finality status
        let status = self.get_finality_status(block_hash)
            .await
            .map_err(|e| StateError::Internal(format!("Failed to get finality status: {}", e)))?;

        // If block is finalized, return full confidence
        if status.is_finalized {
            return Ok(1.0);
        }

        // If block has justification, verify it
        if status.has_justification {
            let justification = self.client
                .justification(BlockId::Hash(block_hash))
                .map_err(|e| StateError::Internal(format!("Failed to get justification: {}", e)))?
                .ok_or_else(|| StateError::Internal("Justification not found".into()))?;

            // Decode and verify justification
            let grandpa_justification = GrandpaJustification::<B>::decode(&mut &justification[..])
                .map_err(|e| StateError::Internal(format!("Failed to decode justification: {}", e)))?;

            let is_valid = self.verify_justification(grandpa_justification)
                .await
                .map_err(|e| StateError::Internal(format!("Failed to verify justification: {}", e)))?;

            if is_valid {
                return Ok(0.9); // High confidence but not finalized
            }
        }

        // Calculate confidence based on confirmations
        let confidence = (status.confirmations as f64) / (self.params.min_confirmations as f64);
        Ok(confidence.min(0.8)) // Cap at 0.8 if not finalized
    }

    async fn verify_chain_rules(
        &self,
        block_ref: &BlockRef,
        rules: &ChainRules,
    ) -> Result<bool, StateError> {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        // Get finality status
        let status = self.get_finality_status(block_hash)
            .await
            .map_err(|e| StateError::Internal(format!("Failed to get finality status: {}", e)))?;

        // Check minimum confirmations
        if status.confirmations < rules.min_confirmations.into() {
            return Ok(false);
        }

        // Check fork depth
        let fork_depth = self.client
            .info()
            .best_number
            .saturating_sub(status.block_number);
        if fork_depth > rules.max_fork_depth.into() {
            return Ok(false);
        }

        // If we have a justification, verify validator participation
        if status.has_justification {
            let justification = self.client
                .justification(BlockId::Hash(block_hash))
                .map_err(|e| StateError::Internal(format!("Failed to get justification: {}", e)))?
                .ok_or_else(|| StateError::Internal("Justification not found".into()))?;

            let grandpa_justification = GrandpaJustification::<B>::decode(&mut &justification[..])
                .map_err(|e| StateError::Internal(format!("Failed to decode justification: {}", e)))?;

            let is_valid = self.verify_justification(grandpa_justification)
                .await
                .map_err(|e| StateError::Internal(format!("Failed to verify justification: {}", e)))?;

            if !is_valid {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

/// Block finality status
#[derive(Debug, Clone)]
struct FinalityStatus {
    block_number: NumberFor<B>,
    confirmations: NumberFor<B>,
    has_justification: bool,
    is_finalized: bool,
} 