// src/finality/mod.rs
#![allow(unused_imports)]
#![allow(unused_variables)]

use frost_protocol::finality::predicate::{
    FinalityPredicate,
    FinalityVerificationClient,
    PredicateConfig,
    ChainRules,
    FinalityVerificationError,
    Block as FrostBlock,
};
use frost_protocol::state::{BlockRef as FrostBlockRef, ChainId};
use frost_protocol::finality::error::FinalityError;
use sp_consensus_grandpa::{
    AuthorityList,
    AuthorityId,
    AuthorityWeight,
    GRANDPA_ENGINE_ID,
    GrandpaJustification,
    AuthoritySignature,
    VersionedAuthorityList,
};
use finality_grandpa::{
    voter_set::{VoterSet, VoterInfo},
    round::State as RoundState,
    Chain,
    BlockNumberOps,
    Error as GrandpaError,
    // weights::VoterWeight,
};
use std::collections::HashMap;
use tokio::sync::RwLock as TokioRwLock;

// Use u64 directly instead of VoterWeight
type VoterWeight = u64;

use sp_runtime::{
    generic::BlockId,
    traits::{Block as BlockT, Header as HeaderT, NumberFor, SaturatedConversion, Saturating, AppVerify, Zero},
    Justification,
};
use sp_api::ProvideRuntimeApi;
use sp_blockchain::{HeaderBackend, Backend as BlockBackend, Info};
use sp_core::H256;
use parity_scale_codec::{Decode, Encode};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::marker::PhantomData;
use log::{debug, warn, error};
use parking_lot::RwLock;
use sp_runtime::traits::Verify;
use metrics::{register_counter, register_gauge, Counter, Gauge};
use once_cell::sync::Lazy;

static SIGNATURE_VERIFICATIONS: Lazy<Counter> = Lazy::new(|| {
    register_counter!("grandpa_signature_verifications_total")
});

static VALID_SIGNATURES: Lazy<Counter> = Lazy::new(|| {
    register_counter!("grandpa_valid_signatures_total")
});

static CURRENT_SET_VALIDATORS: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("grandpa_current_set_validators")
});

#[derive(Debug, Clone)]
pub struct AuthoritySet<H, N> {
    pub authorities: AuthorityList,
    pub set_id: u64,
    _phantom: PhantomData<(H, N)>,
}

impl<H, N> AuthoritySet<H, N> {
    pub fn new(authorities: AuthorityList, set_id: u64) -> Self {
        Self {
            authorities,
            set_id,
            _phantom: PhantomData,
        }
    }

    pub fn current_authorities(&self) -> &AuthorityList {
        &self.authorities
    }
}

// Add authority set tracking
#[derive(Debug)]
pub struct AuthoritySetChange<B: BlockT> {
    pub set_id: u64,
    pub authorities: AuthorityList,
    pub scheduled_at: NumberFor<B>,
}

#[derive(Debug)]
pub struct AuthoritySetManager<B: BlockT> {
    current_set: AuthoritySet<B::Hash, NumberFor<B>>,
    pending_changes: Vec<AuthoritySetChange<B>>,
    last_finalized: NumberFor<B>,
}

impl<B: BlockT> AuthoritySetManager<B> {
    pub fn new(initial_set: AuthoritySet<B::Hash, NumberFor<B>>) -> Self {
        CURRENT_SET_VALIDATORS.set(initial_set.current_authorities().len() as f64);
        Self {
            current_set: initial_set,
            pending_changes: Vec::new(),
            last_finalized: NumberFor::<B>::zero(),
        }
    }

    pub fn schedule_change(
        &mut self,
        new_authorities: AuthorityList,
        scheduled_at: NumberFor<B>,
    ) {
        let next_set_id = self.current_set.set_id + 1;
        self.pending_changes.push(AuthoritySetChange {
            set_id: next_set_id,
            authorities: new_authorities,
            scheduled_at,
        });
        
        debug!(
            "Scheduled authority set change {} at block {}",
            next_set_id,
            scheduled_at
        );
    }

    pub fn process_finalized_block(&mut self, number: NumberFor<B>) {
        self.last_finalized = number;
        
        // Process any pending changes that should be activated
        while let Some(change) = self.pending_changes.first() {
            if change.scheduled_at <= number {
                let change = self.pending_changes.remove(0);
                self.apply_change(change);
            } else {
                break;
            }
        }
    }

    fn apply_change(&mut self, change: AuthoritySetChange<B>) {
        debug!(
            "Applying authority set change {} with {} authorities",
            change.set_id,
            change.authorities.len()
        );

        self.current_set = AuthoritySet::new(
            change.authorities,
            change.set_id,
        );

        CURRENT_SET_VALIDATORS.set(self.current_set.current_authorities().len() as f64);
    }

    pub fn current_authorities(&self) -> &AuthorityList {
        self.current_set.current_authorities()
    }

    pub fn set_id(&self) -> u64 {
        self.current_set.set_id
    }
}

// Add new struct for chain configuration
#[derive(Clone, Debug)]
pub struct ChainConfig {
    pub chain_id: String,
    pub network_type: NetworkType,
    pub authority_set_id: u64,
}

#[derive(Clone, Debug)]
pub enum NetworkType {
    Mainnet,
    Testnet(String),
    Development,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            chain_id: "substrate-default".to_string(),
            network_type: NetworkType::Development,
            authority_set_id: 0,
        }
    }
}

impl ChainConfig {
    pub fn get_chain_id(&self) -> String {
        match &self.network_type {
            NetworkType::Mainnet => self.chain_id.clone(),
            NetworkType::Testnet(name) => format!("{}-testnet-{}", self.chain_id, name),
            NetworkType::Development => format!("{}-dev", self.chain_id),
        }
    }
}

/// GRANDPA-based finality implementation for FROST
pub struct GrandpaFinality<C, B: BlockT> {
    client: Arc<C>,
    config: PredicateConfig,
    verification_client: Arc<dyn FinalityVerificationClient>,
    voter_set: Arc<VoterSet<AuthorityId>>,
    _phantom: PhantomData<B>,
}

impl<C, B> GrandpaFinality<C, B>
where
    B: BlockT<Hash = H256>,
    B::Header: HeaderT<Hash = H256>,
    NumberFor<B>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
    C: HeaderBackend<B> + BlockBackend<B> + Send + Sync,
{
    pub fn new(
        client: Arc<C>,
        config: PredicateConfig,
        verification_client: Arc<dyn FinalityVerificationClient>,
        authorities: AuthorityList,
    ) -> Self {
        let voter_set = VoterSet::new(
            authorities.iter()
                .map(|(id, weight)| (id.clone(), *weight))
                .collect::<Vec<_>>()
        ).expect("Valid authority set");
        Self {
            client,
            config,
            verification_client,
            voter_set: Arc::new(voter_set),
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<C, B> FinalityPredicate for GrandpaFinality<C, B>
where
    B: BlockT<Hash = H256>,
    B::Header: HeaderT<Hash = H256>,
    NumberFor<B>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
    C: HeaderBackend<B> + BlockBackend<B> + Send + Sync,
{
    async fn is_final(&self, block_ref: &FrostBlockRef) -> Result<bool, FinalityError> {
        debug!("Checking finality for block: {:?}", block_ref);

        // 1. Verify block exists and hash matches
        let exists = self.verification_client
            .verify_block_hash(block_ref)
            .await
            .map_err(|e| FinalityError::ChainError(format!("Block verification failed: {}", e)))?;

        if !exists {
            return Err(FinalityError::ChainError("Block not found".to_string()));
        }

        // 2. Get GRANDPA finality confidence
        let finality_confidence = self.verification_client
            .get_finality_confidence(block_ref)
            .await
            .map_err(|e| FinalityError::ConsensusError {
                details: format!("Failed to get finality confidence: {}", e),
                required_power: (self.config.confidence_threshold * 100.0) as u64,
                actual_power: 0,
            })?;

        debug!("Finality confidence: {}, threshold: {}", finality_confidence, self.config.confidence_threshold);

        // 3. Check against configured threshold
        if finality_confidence < self.config.confidence_threshold {
            return Ok(false);
        }

        // 4. Verify GRANDPA-specific chain rules
        let rules = ChainRules {
            min_confirmations: self.config.min_confirmations,
            confidence_threshold: self.config.confidence_threshold,
            max_fork_depth: 50,
            min_participation: 0.67, // 2/3 validator participation required
            chain_params: serde_json::json!({
                "finality_protocol": "GRANDPA",
                "voting_rounds": 2,
                "supermajority_threshold": 0.67,
            }),
        };

        let rules_valid = self.verification_client
            .verify_chain_rules(block_ref, &rules)
            .await
            .map_err(|e| FinalityError::ConsensusError {
                details: format!("Chain rule verification failed: {}", e),
                required_power: (rules.min_participation * 100.0) as u64,
                actual_power: (finality_confidence * 100.0) as u64,
            })?;

        Ok(rules_valid)
    }

    async fn wait_for_finality(&self, block_ref: &FrostBlockRef) -> Result<(), FinalityError> {
        let start = Instant::now();
        let mut retry_count = 0;

        debug!("Waiting for finality of block: {:?}", block_ref);

        while !self.is_final(block_ref).await? {
            if start.elapsed() > self.config.evaluation_timeout {
                return Err(FinalityError::Timeout {
                    block_ref: block_ref.clone(),
                    timeout_secs: self.config.evaluation_timeout,
                    retry_count,
                });
            }

            retry_count += 1;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        debug!("Block finalized after {} retries", retry_count);
        Ok(())
    }
}

/// GRANDPA-specific verification parameters
#[derive(Clone, Debug)]
pub struct GrandpaVerificationParams {
    /// Minimum number of block confirmations
    pub min_confirmations: u32,
    /// Required validator participation (0.0 - 1.0)
    pub min_participation: f64,
    /// Maximum allowed fork depth
    pub max_fork_depth: u32,
    /// Verification timeout
    pub timeout: Duration,
    /// GRANDPA voter set ID
    pub set_id: u64,
    /// Strict signature verification
    pub strict_signature_verification: bool,
}

impl Default for GrandpaVerificationParams {
    fn default() -> Self {
        Self {
            min_confirmations: 2,
            min_participation: 0.67,
            max_fork_depth: 50,
            timeout: Duration::from_secs(30),
            set_id: 0,
            strict_signature_verification: true,
        }
    }
}

/// Voting round information
#[derive(Debug, Clone)]
pub struct VotingRoundInfo {
    pub round: u64,
    pub set_id: u64,
    pub target_hash: H256,
    pub target_number: u64,
    pub precommit_count: u32,
    pub total_weight: u64,
    pub valid_weight: u64,
}

/// Block finality status
#[derive(Debug, Clone)]
pub struct FinalityStatus<N> {
    pub block_number: N,
    pub confirmations: N,
    pub has_justification: bool,
    pub is_finalized: bool,
    pub participation_rate: f64,
}

/// Substrate-specific verification client implementation
pub struct SubstrateVerificationClient<C, B: BlockT> {
    client: Arc<C>,
    voter_set: Arc<RwLock<VoterSet<AuthorityId>>>,
    params: GrandpaVerificationParams,
    chain_config: ChainConfig,
    finality_cache: Arc<tokio::sync::RwLock<HashMap<H256, (FinalityStatus<NumberFor<B>>, Instant)>>>,
    _phantom: PhantomData<B>,
}

#[derive(Clone, Debug)]
pub struct SubstrateBlockRef {
    pub inner: FrostBlockRef,
    pub timestamp: u64,
}

impl From<SubstrateBlockRef> for FrostBlockRef {
    fn from(block: SubstrateBlockRef) -> Self {
        block.inner
    }
}

impl AsRef<FrostBlockRef> for SubstrateBlockRef {
    fn as_ref(&self) -> &FrostBlockRef {
        &self.inner
    }
}

// Update trait bounds to resolve Hash ambiguity
impl<C, B> SubstrateVerificationClient<C, B>
where
    B: BlockT<Hash = H256>,
    B::Header: HeaderT<Hash = H256>,
    NumberFor<B>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
    C: HeaderBackend<B> + BlockBackend<B> + Send + Sync,
{
    pub fn new(
        client: Arc<C>,
        voter_set: VoterSet<AuthorityId>,
        params: GrandpaVerificationParams,
        chain_config: ChainConfig,
    ) -> Self {
        Self {
            client,
            voter_set: Arc::new(RwLock::new(voter_set)),
            params,
            chain_config,
            finality_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            _phantom: PhantomData,
        }
    }

    /// Verify GRANDPA justification signatures and voting power
    async fn verify_justification(
        &self,
        justification: &GrandpaJustification<B::Header>,
    ) -> Result<(bool, f64), GrandpaFinalityVerificationError> {
        // Get current authority set
        let authority_set = self.voter_set.read();
        
        // Verify justification signatures and calculate voting power
        let mut signed_weight = 0u64;
        let mut valid_signatures = 0;
    
        for signed_precommit in &justification.commit.precommits {
            // Find authority in the set
            if let Some(voter_info) = authority_set.get(&signed_precommit.id) {
                // Verify the signature
                let encoded_precommit = signed_precommit.precommit.encode();
                
                if signed_precommit.signature.verify(&encoded_precommit[..], &signed_precommit.id) {
                    // Get raw u64 weight
                    let weight = voter_info.weight().0.get();
                    signed_weight = signed_weight.saturating_add(weight);
                    valid_signatures += 1;
                    
                    // Update metrics
                    if let Some(counter) = once_cell::sync::Lazy::<Counter>::get(&SIGNATURE_VERIFICATIONS) {
                        counter.increment(1);
                    }
                    if let Some(counter) = once_cell::sync::Lazy::<Counter>::get(&VALID_SIGNATURES) {
                        counter.increment(1);
                    }
                }
            }
        }

        // Calculate total weight of the authority set
        let total_weight = authority_set.total_weight().0.get();

        // Calculate participation rate using the raw u64 values
        let participation_rate = if total_weight > 0 {
            signed_weight as f64 / total_weight as f64
        } else {
            0.0
        };
    
        Ok((participation_rate >= self.params.min_participation, participation_rate))
    }

    /// Get comprehensive finality status for a block
    async fn get_finality_status(
        &self,
        block_hash: H256,
    ) -> Result<FinalityStatus<NumberFor<B>>, GrandpaFinalityVerificationError> {
        // Get block header
        let header = self.client
            .header(block_hash)
            .map_err(|e| GrandpaFinalityVerificationError::ChainError(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| GrandpaFinalityVerificationError::BlockNotFound(format!("{:?}", block_hash)))?;

        // Get chain info
        let info = self.client.info();
        let latest_finalized = info.finalized_number;

        // Calculate confirmations
        let confirmations = latest_finalized.saturating_sub(*header.number());

        // Check if block is already finalized
        let is_finalized = *header.number() <= latest_finalized;

        // Get justification if available
        let justification = self.client
            .justifications(block_hash)
            .map_err(|e| GrandpaFinalityVerificationError::ChainError(format!("Failed to get justification: {}", e)))?
            .and_then(|j| j.into_justification(GRANDPA_ENGINE_ID));

        let mut participation_rate = 0.0;
        let has_justification = justification.is_some();

        // If we have a justification, verify it
        if let Some(justification_bytes) = justification {
            match GrandpaJustification::<B::Header>::decode(&mut &justification_bytes[..]) {
                Ok(justification) => {
                    match self.verify_justification(&justification).await {
                        Ok((_, rate)) => participation_rate = rate,
                        Err(e) => warn!("Failed to verify justification: {:?}", e),
                    }
                }
                Err(e) => warn!("Failed to decode justification: {}", e),
            }
        }

        Ok(FinalityStatus {
            block_number: *header.number(),
            confirmations,
            has_justification,
            is_finalized,
            participation_rate,
        })
    }

    // Add helper method for chain ID
    fn get_chain_id(&self) -> String {
        match &self.chain_config.network_type {
            NetworkType::Mainnet => self.chain_config.chain_id.clone(),
            NetworkType::Testnet(name) => format!("{}-testnet-{}", self.chain_config.chain_id, name),
            NetworkType::Development => format!("{}-dev", self.chain_config.chain_id),
        }
    }

    // Add helper method for block inclusion verification
    async fn verify_merkle_proof(
        &self,
        block_hash: H256,
        proof: &[u8],
    ) -> Result<bool, GrandpaFinalityVerificationError> {
        // Get the block header
        let header = self.client
            .header(block_hash)
            .map_err(|e| GrandpaFinalityVerificationError::ChainError(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| GrandpaFinalityVerificationError::BlockNotFound(format!("{:?}", block_hash)))?;

        // Verify the proof against the state root
        let state_root = header.state_root();
        
        // In a real implementation, this would verify a merkle proof
        // For now, we'll do a basic check
        if proof.is_empty() {
            return Ok(false);
        }

        // TODO: Implement actual merkle proof verification
        // This would use something like:
        // sp_trie::verify_trie_proof(state_root, &proof, &key, &value)
        
        Ok(true)
    }

    // Update handle_authority_set_update to use proper types
    pub async fn handle_authority_set_update(
        &self,
        new_set: AuthorityList,
        set_id: u64,
    ) -> Result<(), GrandpaFinalityVerificationError> {
        let mut voter_set = self.voter_set.write();
        
        let new_voter_set = VoterSet::new(
            new_set.iter()
                .map(|(id, weight)| (id.clone(), *weight))
                .collect::<Vec<(AuthorityId, u64)>>()
        ).ok_or_else(|| GrandpaFinalityVerificationError::AuthoritySetError(
            "Failed to create voter set".to_string()
        ))?;

        *voter_set = new_voter_set;
        
        debug!(
            "Updated authority set to set {} with {} authorities",
            set_id,
            new_set.len()
        );

        Ok(())
    }

    /// Get finality status with caching
    async fn get_finality_status_cached(
        &self,
        block_hash: H256,
    ) -> Result<FinalityStatus<NumberFor<B>>, GrandpaFinalityVerificationError> {
        // Check cache first
        {
            let cache = self.finality_cache.read().await;
            if let Some((status, timestamp)) = cache.get(&block_hash) {
                if timestamp.elapsed() < Duration::from_secs(5) {
                    return Ok(status.clone());
                }
            }
        }

        // Get fresh status
        let status = self.get_finality_status(block_hash).await?;
        
        // Update cache
        {
            let mut cache = self.finality_cache.write().await;
            cache.insert(block_hash, (status.clone(), Instant::now()));
            
            // Clean old entries
            cache.retain(|_, (_, timestamp)| timestamp.elapsed() < Duration::from_secs(30));
        }

        Ok(status)
    }

    // Update get_chain_head to return SubstrateBlockRef for internal use
    async fn get_chain_head_internal(&self) -> Result<SubstrateBlockRef, GrandpaFinalityVerificationError> {
        let info = self.client.info();
        let hash_array: [u8; 32] = info.best_hash.as_ref()
            .try_into()
            .map_err(|_| GrandpaFinalityVerificationError::InvalidInput("Invalid hash length".to_string()))?;

        Ok(SubstrateBlockRef {
            inner: FrostBlockRef {
                hash: hash_array,
                number: info.best_number.saturated_into::<u64>(),
                chain_id: ChainId::new(&self.chain_config.get_chain_id()),
            },
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        })
    }

    // Helper method to safely convert u64 to NumberFor<B>
    fn u64_to_block_number(&self, value: u64) -> NumberFor<B> {
        NumberFor::<B>::saturated_from(value)
    }
    
    // Helper method to safely convert NumberFor<B> to u64
    fn block_number_to_u64(&self, value: NumberFor<B>) -> u64 {
        value.saturated_into::<u64>()
    }
}

#[async_trait]
impl<C, B> FinalityVerificationClient for SubstrateVerificationClient<C, B>
where
    B: BlockT<Hash = H256>,
    B::Header: HeaderT<Hash = H256>,
    NumberFor<B>: Into<u64> + SaturatedConversion + Saturating + Zero + Copy,
    C: HeaderBackend<B> + BlockBackend<B> + Send + Sync,
{
    async fn get_chain_head(&self) -> Result<FrostBlockRef, FinalityVerificationError> {
        let info = self.client.info();
        let hash_array: [u8; 32] = info.best_hash.as_ref()
            .try_into()
            .map_err(|_| GrandpaFinalityVerificationError::InvalidInput("Invalid hash length".to_string()))?;

        Ok(FrostBlockRef {
            hash: hash_array,
            number: info.best_number.saturated_into::<u64>(),
            chain_id: ChainId::new(&self.chain_config.get_chain_id()),
        })
    }

    async fn verify_block_hash(&self, block_ref: &FrostBlockRef) -> Result<bool, FinalityVerificationError> {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        // Check if block exists
        let header = self.client
            .header(block_hash)
            .map_err(|e| GrandpaFinalityVerificationError::ChainError(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| GrandpaFinalityVerificationError::BlockNotFound(format!("{:?}", block_hash)))?;
    
        // Use saturated conversion instead of From<u64>
        let expected_number = NumberFor::<B>::saturated_from(block_ref.number);
        Ok(*header.number() == expected_number)
    }

    async fn get_finality_confidence(&self, block_ref: &FrostBlockRef) -> Result<f64, FinalityVerificationError> {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        // Get finality status
        let status = self.get_finality_status(block_hash).await
            .map_err(|e| GrandpaFinalityVerificationError::ConsensusError(e.to_string()))?;
    
        // If block is already finalized, return maximum confidence
        if status.is_finalized {
            return Ok(1.0);
        }
    
        // Calculate confidence based on multiple factors:
        // 1. Number of confirmations (weight: 40%)
        // 2. Validator participation (weight: 40%)
        // 3. Base confidence (weight: 20%)
    
        // 1. Confirmation confidence (0.0 - 1.0)
        let confirmations_u64 = status.confirmations.saturated_into::<u64>();
        let confirmation_confidence = if self.params.min_confirmations > 0 {
            (confirmations_u64 as f64) / (self.params.min_confirmations as f64)
        } else {
            0.0
        }.min(1.0);
    
        // 2. Participation confidence (0.0 - 1.0)
        let participation_confidence = if status.has_justification {
            (status.participation_rate / self.params.min_participation).min(1.0)
        } else {
            0.0
        };
    
        // Calculate weighted confidence
        let confidence = 
            (confirmation_confidence * 0.4) +
            (participation_confidence * 0.4) +
            0.2; // Base confidence
    
        // Cap non-finalized blocks at 0.95
        let final_confidence = if status.is_finalized {
            confidence
        } else {
            confidence.min(0.95)
        };
    
        Ok(final_confidence)
    }

    async fn verify_chain_rules(
        &self,
        block_ref: &FrostBlockRef,
        rules: &ChainRules,
    ) -> Result<bool, FinalityVerificationError> {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        // Get finality status
        let status = self.get_finality_status(block_hash).await
            .map_err(|e| GrandpaFinalityVerificationError::ConsensusError(e.to_string()))?;
    
        // Check minimum confirmations using saturated conversion
        let min_confirmations_block_number = NumberFor::<B>::saturated_from(rules.min_confirmations as u64);
        if status.confirmations < min_confirmations_block_number {
            debug!("Insufficient confirmations: {} < {}", status.confirmations, rules.min_confirmations);
            return Ok(false);
        }
    
        // Check fork depth
        let info = self.client.info();
        let fork_depth = info.best_number.saturating_sub(status.block_number);
        let max_fork_depth_block_number = NumberFor::<B>::saturated_from(rules.max_fork_depth as u64);
        if fork_depth > max_fork_depth_block_number {
            debug!("Fork depth too large: {} > {}", fork_depth, rules.max_fork_depth);
            return Ok(false);
        }
    
        // Check participation rate if justification exists
        if status.has_justification && status.participation_rate < rules.min_participation {
            debug!("Insufficient participation: {:.2}% < {:.2}%", 
                   status.participation_rate * 100.0, rules.min_participation * 100.0);
            return Ok(false);
        }
    
        Ok(true)
    }

    async fn get_block(&self, block_ref: &FrostBlockRef) -> Result<FrostBlock, FinalityVerificationError> {
        let block_hash = H256::from_slice(&block_ref.hash);
        
        // Get block header
        let header = self.client
            .header(block_hash)
            .map_err(|e| GrandpaFinalityVerificationError::ChainError(format!("Failed to get header: {}", e)))?
            .ok_or_else(|| GrandpaFinalityVerificationError::BlockNotFound(format!("{:?}", block_hash)))?;

        Ok(FrostBlock {
            hash: block_ref.hash.clone(),
            number: block_ref.number,
        })
    }

    async fn get_latest_finalized_block(&self) -> Result<u64, FinalityVerificationError> {
        let info = self.client.info();
        Ok(info.finalized_number.saturated_into::<u64>())
    }

    async fn verify_block_inclusion(
        &self,
        block_ref: &FrostBlockRef,
        proof: &[u8],
    ) -> Result<bool, FinalityVerificationError> {
        let block_hash = H256::from_slice(&block_ref.hash);

        // First verify the block exists
        let exists = self.verify_block_hash(block_ref).await?;
        if !exists {
            return Ok(false);
        }

        // Then verify the inclusion proof
        self.verify_merkle_proof(block_hash, proof)
            .await
            .map_err(Into::into)
    }
}

// Custom error types for better error handling
#[derive(Debug, thiserror::Error)]
pub enum GrandpaFinalityVerificationError {
    #[error("Chain error: {0}")]
    ChainError(String),
    
    #[error("Block not found: {0}")]
    BlockNotFound(String),
    
    #[error("Consensus error: {0}")]
    ConsensusError(String),
    
    #[error("Timeout error: {0}")]
    TimeoutError(String),
    
    #[error("Invalid justification: {0}")]
    InvalidJustification(String),

    #[error("Invalid proof: {0}")]
    InvalidProof(String),

    #[error("Authority set error: {0}")]
    AuthoritySetError(String),

    #[error("Signature verification failed: {0}")]
    SignatureVerificationError(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

impl From<GrandpaFinalityVerificationError> for FinalityVerificationError {
    fn from(err: GrandpaFinalityVerificationError) -> Self {
        FinalityVerificationError(err.to_string())
    }
}

impl From<GrandpaFinalityVerificationError> for FinalityError {
    fn from(err: GrandpaFinalityVerificationError) -> Self {
        match err {
            GrandpaFinalityVerificationError::ConsensusError(msg) => FinalityError::ConsensusError {
                details: msg,
                required_power: 0,
                actual_power: 0,
            },
            _ => FinalityError::ChainError(err.to_string()),
        }
    }
}

// Helper traits for better type safety
pub trait GrandpaFinalityProvider<B: BlockT> {
    fn get_voter_set(&self) -> &RwLock<VoterSet<AuthorityId>>;
    fn get_verification_params(&self) -> &GrandpaVerificationParams;
}

impl<C, B> GrandpaFinalityProvider<B> for SubstrateVerificationClient<C, B>
where
    B: BlockT<Hash = H256>,
{
    fn get_voter_set(&self) -> &RwLock<VoterSet<AuthorityId>> {
        &self.voter_set
    }

    fn get_verification_params(&self) -> &GrandpaVerificationParams {
        &self.params
    }
}