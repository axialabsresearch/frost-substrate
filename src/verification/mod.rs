#![allow(unused_imports)]
#![allow(unused_variables)]

use frost_protocol::state::{
    StateProof,
    StateError,
    proof::{ProofType, VerificationParams},
};
use async_trait::async_trait;
use std::sync::Arc;

/// Core verification trait for FROST proofs
#[async_trait]
pub trait ProofVerifier: Send + Sync {
    /// Verify a state proof
    async fn verify_proof(
        &self,
        proof: &StateProof,
        params: &VerificationParams,
    ) -> Result<bool, StateError>;

    /// Get supported proof types
    fn supported_types(&self) -> Vec<ProofType>;
}

/// Merkle proof verifier implementation
pub struct MerkleVerifier {
    // Add Merkle-specific fields
}

#[async_trait]
impl ProofVerifier for MerkleVerifier {
    async fn verify_proof(
        &self,
        proof: &StateProof,
        params: &VerificationParams,
    ) -> Result<bool, StateError> {
        // Implement Merkle proof verification
        Ok(true) // Placeholder
    }

    fn supported_types(&self) -> Vec<ProofType> {
        vec![ProofType::Basic]
    }
}

/// BLS signature verifier implementation
pub struct BLSVerifier {
    // Add BLS-specific fields
}

#[async_trait]
impl ProofVerifier for BLSVerifier {
    async fn verify_proof(
        &self,
        proof: &StateProof,
        params: &VerificationParams,
    ) -> Result<bool, StateError> {
        // Implement BLS signature verification
        Ok(true) // Placeholder
    }

    fn supported_types(&self) -> Vec<ProofType> {
        vec![ProofType::Signature]
    }
}

/// Verification registry that manages multiple verifiers
pub struct VerificationRegistry {
    verifiers: Vec<Arc<dyn ProofVerifier>>,
}

impl VerificationRegistry {
  pub fn new() -> Self {
      Self {
          verifiers: Vec::new(),
      }
  }

  pub fn register_verifier(&mut self, verifier: Arc<dyn ProofVerifier>) {
      self.verifiers.push(verifier);
  }

  pub async fn verify_proof(
      &self,
      proof: &StateProof,
      params: &VerificationParams,
  ) -> Result<bool, StateError> {
      // Find appropriate verifier
      for verifier in &self.verifiers {
          if verifier.supported_types().contains(&proof.proof_type()) {
              return verifier.verify_proof(proof, params).await;
          }
      }

      Err(StateError::Internal("No suitable verifier found".into()))
  }
}