// src/finality/verification.rs
use frost_protocol::{
    finality::{FinalityVerificationClient, FinalityVerificationError, Block, ChainRules},
    state::{BlockRef, StateError},
};
use sp_consensus_grandpa::{
    AuthorityList,
    AuthorityId,
    AuthorityWeight,
    AuthoritySet,
    GRANDPA_ENGINE_ID,
    GrandpaJustification,
    SignedPrecommit,
    Precommit,
    AuthoritySignature,
};
use sp_runtime::{
    generic::BlockId,
    traits::{Block as BlockT, Header as HeaderT, NumberFor, Saturating},
    Justification,
};
use sp_blockchain::{HeaderBackend, BlockBackend, Info};
use sp_core::{H256, crypto::Pair};
use codec::{Decode, Encode};
use async_trait::async_trait;
use std::{
    sync::Arc,
    time::{Duration, Instant},
    collections::HashMap,
};
use log::{debug, warn, error, info};

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
    /// GRANDPA set ID for authority set tracking
    pub set_id: u64,
    /// Enable strict signature verification
    pub strict_signature_verification: bool,
}

impl Default for GrandpaVerificationParams {
    fn default() -> Self {
        Self {
            min_confirmations: 2,
            min_participation: 0.67, // 2/3 supermajority
            max_fork_depth: 50,
            timeout: Duration::from_secs(30),
            set_id: 0,
            strict_signature_verification: true,
        }
    }
}

/// Comprehensive block finality status
#[derive(Debug, Clone)]
pub struct FinalityStatus<N> {
    /// Block number
    pub block_number: N,
    /// Number of confirmations
    pub confirmations: N,
    /// Whether block has GRANDPA justification
    pub has_justification: bool,
    /// Whether block is finalized by the chain
    pub is_finalized: bool,
    /// Validator participation rate in justification
    pub participation_rate: f64,
    /// Number of valid signatures in justification
    pub valid_signatures: u32,
    /// Total number of authorities
    pub total_authorities: u32,
    /// Voting power percentage
    pub voting_power: f64,
}

/// GRANDPA voting round information
#[derive(Debug, Clone)]
pub struct VotingRoundInfo {
    /// Round number
    pub round: u64,
    /// Set ID
    pub set_id: u64,
    /// Target block hash
    pub target_hash: H256,
    /// Target block number
    pub target_number: u64,
    /// Precommit count
    pub precommit_count: u32,
    /// Total voting weight
    pub total_weight: AuthorityWeight,
    /// Valid voting weight
    pub valid_weight: AuthorityWeight,
}

/// Enhanced Substrate verification client
pub struct SubstrateVerificationClient<C, B: BlockT> {
    client: Arc<C>,
    authority_set: Arc<AuthoritySet<B::Hash, NumberFor<B>>>,
    params: GrandpaVerificationParams,
    /// Cache for recent finality checks
    finality_cache: Arc<tokio::sync::RwLock<HashMap<H256, (FinalityStatus<NumberFor<B>>, Instant)>>>,
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
            finality_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Verify GRANDPA justification with comprehensive validation
    pub async fn verify_justification(
        &self,
        justification: &GrandpaJustification<B>,
    ) -> Result<(bool, VotingRoundInfo), FinalityVerificationError> {
        debug!("Verifying GRANDPA justification for target: {:?}", justification.commit.target_hash);

        // Get current authority set
        let authority_list = self.authority_set.current_authorities();
        
        // Initialize voting round info
        let mut round_info = VotingRoundInfo {
            round: justification.round,
            set_id: self.params.set_id,
            target_hash: justification.commit.target_hash,
            target_number: justification.commit.target_number.saturated_into(),
            precommit_count: justification.commit.precommits.len() as u32,
            total_weight: 0,
            valid_weight: 0,
        };

        // Calculate total weight of authority set
        let total_weight: AuthorityWeight = authority_list
            .iter()
            .map(|(_, weight)| *weight)
            .sum();
        round_info.total_weight = total_weight;

        // Verify each precommit
        let mut valid_signatures = 0;
        let mut signed_weight: AuthorityWeight = 0;

        for precommit in &justification.commit.precommits {
            // Find authority in the set
            if let Some((_, weight)) = authority_list.iter()
                .find(|(id, _)| id == &precommit.id) 
            {
                // Verify the precommit
                let is_valid = self.verify_precommit(precommit, &justification.commit).await?;
                
                if is_valid {
                    signed_weight += weight;
                    valid_signatures += 1;
                    debug!("Valid precommit from authority: {:?}, weight: {}", precommit.id, weight);
                } else {
                    warn!("Invalid precommit from authority: {:?}", precommit.id);
                }
            } else {
                warn!("Precommit from unknown authority: {:?}", precommit.id);
            }
        }

        round_info.valid_weight = signed_weight;

        // Check if we have supermajority
        let participation_rate = if total_weight > 0 {
            signed_weight as f64 / total_weight as f64
        } else {
            0.0
        };

        let has_supermajority = participation_rate >= self.params.min_participation;

        info!(
            "Justification verification complete: {}/{} valid signatures, {:.2}% participation, supermajority: {}",
            valid_signatures,
            authority_list.len(),
            participation_rate * 100.0,
            has_supermajority
        );

        Ok((has_supermajority, round_info))
    }

    /// Verify a single precommit signature and content
    async fn verify_precommit(
        &self,
        precommit: &SignedPrecommit<B::Hash, NumberFor<B>, AuthoritySignature, AuthorityId>,
        commit: &sp_consensus_grandpa::Commit<B::Hash, NumberFor<B>>,
    ) -> Result<bool, FinalityVerificationError> {
        // Check precommit target matches commit target
        if precommit.precommit.target_hash != commit.target_hash {
            debug!("Precommit target hash mismatch");
            return Ok(false);
        }

        if precommit.precommit.target_number != commit.target_number {
            debug!("Precommit target number mismatch");
            return Ok(false);
        }

        // If strict signature verification is enabled, verify cryptographic signature
        if self.params.strict_signature_verification {
            // In a real implementation, this would verify the Ed25519 signature
            // For now, we'll do basic validation
            if !self.verify_signature_cryptographically(precommit).await? {
                debug!("Cryptographic signature verification failed");
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Verify signature cryptographically (placeholder for actual implementation)
    async fn verify_signature_cryptographically(
        &self,
        precommit: &SignedPrecommit<B::Hash, NumberFor<B>, AuthoritySignature, AuthorityId>,
    ) -> Result<bool, FinalityVerificationError> {
        // In a real implementation, this would:
        // 1. Reconstruct the signed message according to GRANDPA protocol
        // 2. Verify the Ed25519 signature using the authority's public key
        // 3. Check the signature format and encoding
        
        // For demonstration, we'll assume signature verification passes
        // In production, use: sp_consensus_grandpa::verify_signature or similar
        Ok(true)
    }

    /// Get comprehensive finality status with caching
    pub async fn get_finality_status(
        &self,
        block_hash: B::Hash,
    ) -> Result<FinalityStatus<NumberFor<B>>, FinalityVerificationError>
    where
        C: HeaderBackend<B> + BlockBackend<B>,
        B::Hash: From<H256> + Into<H256>,
    {
        let h256_hash: H256 = block_hash.into();
        
        // Check cache first
        {
            let cache = self.finality_cache.read().await;
            if let Some((status, timestamp)) = cache.get(&h256_hash) {
                // Use cached result if less than 5 seconds old
                if timestamp.elapsed() < Duration::from_secs(5) {
                    return Ok(status.clone());
                }
            }
        }

        // Get fresh status
        let status = self.compute_finality_status(block_hash).await?;
        
        // Update cache
        {
            let mut cache = self.finality_cache.write().await;
            cache.insert(h256_hash, (status.clone(), Instant::now()));
            
            // Clean old entries
            cache.retain(|_, (_, timestamp)| timestamp.elapsed() < Duration::from_secs(30));
        }

        Ok(status)
    }

    /// Compute finality status without caching
    async fn compute_finality_status(
        &self,
        block_hash: B::Hash,
    ) -> Result<FinalityStatus<NumberFor<B>>, FinalityVerificationError>
    where
        C: HeaderBackend<B> + BlockBackend<B>,
    {
        // Get block header
        let header = self.client
            .header(BlockId::Hash(block_hash))
            .map_err(|e| FinalityVerificationError::ChainError(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| FinalityVerificationError::BlockNotFound(format!("Block not found: {:?}", block_hash)))?;

        // Get chain info
        let info = self.client.info();
        let latest_finalized = info.finalized_number;
        let best_number = info.best_number;

        // Calculate confirmations
        let confirmations = latest_finalized.saturating_sub(*header.number());
        let is_finalized = *header.number() <= latest_finalized;

        // Get justification if available
        let justification = self.client
            .justification(BlockId::Hash(block_hash))
            .map_err(|e| FinalityVerificationError::ChainError(format!("Failed to get justification: {}", e)))?;

        let mut participation_rate = 0.0;
        let mut valid_signatures = 0;
        let mut total_authorities = 0;
        let mut voting_power = 0.0;

        // If we have justification, analyze it
        if let Some(justification_data) = justification {
            if let Ok(grandpa_justification) = GrandpaJustification::<B>::decode(&mut &justification_data[..]) {
                match self.verify_justification(&grandpa_justification).await {
                    Ok((_, voting_info)) => {
                        participation_rate = if voting_info.total_weight > 0 {
                            voting_info.valid_weight as f64 / voting_info.total_weight as f64
                        } else {
                            0.0
                        };
                        voting_power = participation_rate;
                        valid_signatures = voting_info.precommit_count;
                        total_authorities = self.authority_set.current_authorities().len() as u32;
                    }
                    Err(e) => {
                        warn!("Failed to verify justification: {}", e);
                    }
                }
            }
        }

        Ok(FinalityStatus {
            block_number: *header.number(),
            confirmations,
            has_justification: justification.is_some(),
            is_finalized,
            participation_rate,
            valid_signatures,
            total_authorities,
            voting_power,
        })
    }
}

#[async_trait]
impl<C, B: BlockT> FinalityVerificationClient for SubstrateVerificationClient<C, B>
where
    C: HeaderBackend<B> + BlockBackend<B> + Send + Sync,
    B::Hash: From<H256> + Into<H256>,
{
    async fn verify_block_hash(&self, block_ref: &BlockRef) -> Result<bool, FinalityVerificationError> {
        let block_hash = B::Hash::from(H256::from_slice(&block_ref.hash));
        
        // Check if block exists
        let header = self.client
            .header(BlockId::Hash(block_hash))
            .map_err(|e| FinalityVerificationError::ChainError(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| FinalityVerificationError::BlockNotFound("Block not found".into()))?;

        // Verify block number matches
        if *header.number() != block_ref.number.into() {
            return Ok(false);
        }

        Ok(true)
    }

    async fn get_finality_confidence(&self, block_ref: &BlockRef) -> Result<f64, FinalityVerificationError> {
        let block_hash = B::Hash::from(H256::from_slice(&block_ref.hash));
        
        // Get finality status
        let status = self.get_finality_status(block_hash).await?;

        // If block is finalized, return full confidence
        if status.is_finalized {
            return Ok(1.0);
        }

        // If block has justification and good participation, return high confidence
        if status.has_justification && status.participation_rate >= self.params.min_participation {
            return Ok(0.9);
        }

        // Calculate confidence based on confirmations
        let confidence = if self.params.min_confirmations > 0 {
            (status.confirmations.saturated_into::<u32>() as f64) / (self.params.min_confirmations as f64)
        } else {
            0.0
        };

        Ok(confidence.min(0.8)) // Cap at 0.8 if not finalized
    }

    async fn verify_chain_rules(
        &self,
        block_ref: &BlockRef,
        rules: &ChainRules,
    ) -> Result<bool, FinalityVerificationError> {
        let block_hash = B::Hash::from(H256::from_slice(&block_ref.hash));
        
        // Get finality status
        let status = self.get_finality_status(block_hash).await?;

        // Check minimum confirmations
        if status.confirmations.saturated_into::<u32>() < rules.min_confirmations {
            debug!("Insufficient confirmations: {} < {}", status.confirmations.saturated_into::<u32>(), rules.min_confirmations);
            return Ok(false);
        }

        // Check fork depth
        let info = self.client.info();
        let fork_depth = info.best_number.saturating_sub(status.block_number);
        if fork_depth.saturated_into::<u32>() > rules.max_fork_depth {
            debug!("Fork depth too large: {} > {}", fork_depth.saturated_into::<u32>(), rules.max_fork_depth);
            return Ok(false);
        }

        // Check confidence threshold
        let confidence = self.get_finality_confidence(block_ref).await?;
        if confidence < rules.confidence_threshold {
            debug!("Confidence below threshold: {} < {}", confidence, rules.confidence_threshold);
            return Ok(false);
        }

        // Check validator participation if we have justification
        if status.has_justification && status.participation_rate < rules.min_participation {
            debug!("Participation below threshold: {} < {}", status.participation_rate, rules.min_participation);
            return Ok(false);
        }

        Ok(true)
    }

    async fn get_block(&self, block_ref: &BlockRef) -> Result<Block, FinalityVerificationError> {
        let block_hash = B::Hash::from(H256::from_slice(&block_ref.hash));
        
        // Get block header
        let header = self.client
            .header(BlockId::Hash(block_hash))
            .map_err(|e| FinalityVerificationError::ChainError(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| FinalityVerificationError::BlockNotFound("Block not found".into()))?;

        // Get block body if available
        let body = self.client
            .block_body(BlockId::Hash(block_hash))
            .map_err(|e| FinalityVerificationError::ChainError(format!("Failed to get body: {}", e)))?
            .unwrap_or_default();

        // Convert to FROST Block format
        Ok(Block {
            hash: block_ref.hash.clone(),
            number: block_ref.number,
            parent_hash: header.parent_hash().encode(),
            timestamp: 0, // Would need to extract from block or use current time
            transactions: body.len() as u32,
            metadata: serde_json::json!({
                "substrate_header": header.encode(),
                "extrinsics_count": body.len(),
            }),
        })
    }

    async fn get_latest_finalized_block(&self) -> Result<u64, FinalityVerificationError> {
        let info = self.client.info();
        Ok(info.finalized_number.saturated_into())
    }

    async fn get_chain_head(&self) -> Result<BlockRef, FinalityVerificationError> {
        let info = self.client.info();
        let best_hash: H256 = info.best_hash.into();
        
        Ok(BlockRef {
            hash: best_hash.as_bytes().to_vec(),
            number: info.best_number.saturated_into(),
        })
    }

    async fn verify_block_inclusion(
        &self, 
        block_ref: &BlockRef, 
        _proof: &[u8]
    ) -> Result<bool, FinalityVerificationError> {
        // For basic implementation, just verify block exists
        self.verify_block_hash(block_ref).await
    }
}