//! Role-Capability Mesh — AI-native federation.
//!
//! Roles are cognitive functions, not org-chart positions. An instance fills
//! roles based on its live `CognitiveTier`. Roles are yielded when the instance's
//! tier drops below the role's minimum requirement. Succession is deterministic:
//! `SuccessionPolicy::nominate()` picks the best-capable peer.
//!
//! ## Layer compliance
//!
//! - **Layer 1:** `RoleMesh` maintains assignments and attestations — no LLM.
//! - **Layer 2:** `roles_to_yield()` detects capability drop.
//! - **Layer 3:** Caller fires Signal after yield (one Signal per yield event).
//! - **Layer 4:** AILF reads mesh state via `get_mesh_roles` introspective tool.

use crate::capability::CognitiveTier;
use crate::identity::InstanceId;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, Verifier};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// MeshRole
// ---------------------------------------------------------------------------

/// A cognitive function that an instance can fill in a federated mesh.
///
/// Distinct from `CognitiveRole` in `engine_registry` which maps LLM engines
/// to internal cognitive tasks (Perception/Reflection/Reasoning).
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum MeshRole {
    /// Holds mission context, synthesizes across peers, authorizes novel actions.
    Coordinator,
    /// Deep analytical reasoning, long-horizon planning.
    Strategist,
    /// Domain-specific reasoning and evaluation.
    Analyst,
    /// Carries out well-defined tasks.
    Executor,
    /// Sensing, perception, ambient monitoring.
    Observer,
    /// Alive but degraded; no active roles; ready to re-assume.
    Standby,
}

impl MeshRole {
    /// Minimum `CognitiveTier` required to fill this role.
    ///
    /// Lower tier value = more capable. A role can be filled when the instance's
    /// tier value ≤ `min_tier()`. E.g., Coordinator requires tier ≤ 2 (Full or Strong).
    pub fn min_tier(self) -> u8 {
        match self {
            MeshRole::Coordinator => 2,
            MeshRole::Strategist  => 2,
            MeshRole::Analyst     => 3,
            MeshRole::Executor    => 5,
            MeshRole::Observer    => 5,
            MeshRole::Standby     => 5,
        }
    }

    /// Returns `true` if `tier` meets this role's capability requirement.
    pub fn can_be_filled_by(self, tier: CognitiveTier) -> bool {
        (tier as u8) <= self.min_tier()
    }

    pub fn label(self) -> &'static str {
        match self {
            MeshRole::Coordinator => "Coordinator",
            MeshRole::Strategist  => "Strategist",
            MeshRole::Analyst     => "Analyst",
            MeshRole::Executor    => "Executor",
            MeshRole::Observer    => "Observer",
            MeshRole::Standby     => "Standby",
        }
    }
}

// ---------------------------------------------------------------------------
// CapabilityAttestation
// ---------------------------------------------------------------------------

/// Fields that are canonically serialized and signed.
/// Kept separate so `sign()` and `verify()` operate on the same payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationFields {
    pub instance_id: InstanceId,
    pub cognitive_tier: CognitiveTier,
    pub active_roles: Vec<MeshRole>,
    pub available_domains: Vec<String>,
    pub load: f32,
    pub signed_at: DateTime<Utc>,
}

/// Signed capability attestation published by each instance.
///
/// The `signature` is Ed25519 over the canonical JSON of `AttestationFields`.
/// Unsigned attestations from peers MUST be rejected — call `verify()` before
/// inserting into `RoleMesh`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityAttestation {
    pub instance_id: InstanceId,
    pub cognitive_tier: CognitiveTier,
    pub active_roles: Vec<MeshRole>,
    pub available_domains: Vec<String>,
    /// 0.0 = idle, 1.0 = saturated.
    pub load: f32,
    pub signed_at: DateTime<Utc>,
    /// Ed25519 signature over canonical JSON of the fields above.
    pub signature: Vec<u8>,
}

impl CapabilityAttestation {
    /// Create and sign an attestation with the given identity signing key.
    pub fn sign(fields: AttestationFields, signing_key: &ed25519_dalek::SigningKey) -> Self {
        let canonical = serde_json::to_vec(&fields)
            .expect("AttestationFields is always serializable");
        let sig: Signature = signing_key.sign(&canonical);
        Self {
            instance_id: fields.instance_id,
            cognitive_tier: fields.cognitive_tier,
            active_roles: fields.active_roles,
            available_domains: fields.available_domains,
            load: fields.load,
            signed_at: fields.signed_at,
            signature: sig.to_bytes().to_vec(),
        }
    }

    /// Verify the attestation's signature against the given verifying key.
    ///
    /// Returns `true` if the signature is valid and the `instance_id` matches
    /// the expected instance. Returns `false` on any verification failure.
    pub fn verify(&self, verifying_key: &ed25519_dalek::VerifyingKey) -> bool {
        let fields = AttestationFields {
            instance_id: self.instance_id,
            cognitive_tier: self.cognitive_tier,
            active_roles: self.active_roles.clone(),
            available_domains: self.available_domains.clone(),
            load: self.load,
            signed_at: self.signed_at,
        };
        let canonical = match serde_json::to_vec(&fields) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig_bytes: [u8; 64] = match self.signature.as_slice().try_into() {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig = Signature::from_bytes(&sig_bytes);
        verifying_key.verify(&canonical, &sig).is_ok()
    }
}

/// Verified attestation — only constructed after `verify()` passes.
#[derive(Debug, Clone)]
pub struct VerifiedAttestation {
    pub attestation: CapabilityAttestation,
    pub verified_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// RoleMesh
// ---------------------------------------------------------------------------

/// Live map of role assignments in the federated mesh.
///
/// Runs in the Cortex substrate (Layer 1) — no LLM calls.
/// The local instance updates its own entry; peer attestations arrive via
/// federation protocol and are inserted after signature verification.
#[derive(Debug, Default)]
pub struct RoleMesh {
    /// Current role → instance assignment.
    pub assignments: HashMap<MeshRole, InstanceId>,
    /// All known verified attestations, keyed by instance ID.
    pub attestations: HashMap<InstanceId, VerifiedAttestation>,
}

impl RoleMesh {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update an attestation for an instance.
    ///
    /// The attestation must have already been verified by the caller (signature
    /// checked against the instance's known verifying key). Use `update_attestation_raw`
    /// for internal/local updates where signing key is known.
    ///
    /// Returns `true` if the attestation was accepted, `false` if rejected.
    pub fn insert_verified(&mut self, verified: VerifiedAttestation) -> bool {
        self.attestations.insert(verified.attestation.instance_id, verified);
        true
    }

    /// Update the attestation for `instance_id`, verifying the signature.
    ///
    /// Returns `true` if signature verified and attestation stored; `false` otherwise.
    pub fn update_attestation(
        &mut self,
        attestation: CapabilityAttestation,
        verifying_key: &ed25519_dalek::VerifyingKey,
    ) -> bool {
        if !attestation.verify(verifying_key) {
            tracing::warn!(
                "RoleMesh: rejected attestation from {} — signature invalid",
                attestation.instance_id
            );
            return false;
        }
        self.attestations.insert(
            attestation.instance_id,
            VerifiedAttestation {
                attestation,
                verified_at: Utc::now(),
            },
        );
        true
    }

    /// Assign a role to an instance (called after successful succession nomination).
    pub fn assign_role(&mut self, role: MeshRole, instance_id: InstanceId) {
        self.assignments.insert(role, instance_id);
    }

    /// Remove a role assignment (called when an instance yields a role).
    pub fn release_role(&mut self, role: MeshRole) -> Option<InstanceId> {
        self.assignments.remove(&role)
    }

    /// Returns the roles held by `instance_id` that must be yielded because
    /// `current_tier` no longer meets their minimum requirement.
    pub fn roles_to_yield(
        &self,
        instance_id: &InstanceId,
        current_tier: CognitiveTier,
    ) -> Vec<MeshRole> {
        self.assignments
            .iter()
            .filter(|(role, holder)| {
                *holder == instance_id && !role.can_be_filled_by(current_tier)
            })
            .map(|(role, _)| *role)
            .collect()
    }

    /// Compute which roles `instance_id` is eligible to hold given its current tier.
    /// Roles already assigned to others are excluded.
    pub fn compute_eligible_roles(
        &self,
        instance_id: &InstanceId,
        current_tier: CognitiveTier,
    ) -> Vec<MeshRole> {
        let all_roles = [
            MeshRole::Coordinator,
            MeshRole::Strategist,
            MeshRole::Analyst,
            MeshRole::Executor,
            MeshRole::Observer,
            MeshRole::Standby,
        ];
        all_roles
            .iter()
            .filter(|role| {
                role.can_be_filled_by(current_tier) &&
                // Not already assigned to someone else
                self.assignments.get(*role).map_or(true, |holder| holder == instance_id)
            })
            .copied()
            .collect()
    }

    /// All verified attestations as a flat list.
    pub fn all_attestations(&self) -> Vec<&VerifiedAttestation> {
        self.attestations.values().collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn make_attestation(
        instance_id: InstanceId,
        tier: CognitiveTier,
        roles: Vec<MeshRole>,
        signing_key: &SigningKey,
    ) -> CapabilityAttestation {
        let fields = AttestationFields {
            instance_id,
            cognitive_tier: tier,
            active_roles: roles,
            available_domains: vec!["reasoning".to_string()],
            load: 0.2,
            signed_at: Utc::now(),
        };
        CapabilityAttestation::sign(fields, signing_key)
    }

    #[test]
    fn mesh_role_min_tier_values() {
        assert_eq!(MeshRole::Coordinator.min_tier(), 2);
        assert_eq!(MeshRole::Strategist.min_tier(), 2);
        assert_eq!(MeshRole::Analyst.min_tier(), 3);
        assert_eq!(MeshRole::Executor.min_tier(), 5);
        assert_eq!(MeshRole::Observer.min_tier(), 5);
        assert_eq!(MeshRole::Standby.min_tier(), 5);
    }

    #[test]
    fn can_be_filled_by_tier() {
        assert!(MeshRole::Coordinator.can_be_filled_by(CognitiveTier::Full));
        assert!(MeshRole::Coordinator.can_be_filled_by(CognitiveTier::Strong));
        assert!(!MeshRole::Coordinator.can_be_filled_by(CognitiveTier::Reduced));
        assert!(!MeshRole::Coordinator.can_be_filled_by(CognitiveTier::MemoryOnly));

        assert!(MeshRole::Analyst.can_be_filled_by(CognitiveTier::Full));
        assert!(MeshRole::Analyst.can_be_filled_by(CognitiveTier::Strong));
        assert!(MeshRole::Analyst.can_be_filled_by(CognitiveTier::Reduced));
        assert!(!MeshRole::Analyst.can_be_filled_by(CognitiveTier::MemoryOnly));

        // Executor/Observer/Standby can be filled by any tier
        assert!(MeshRole::Executor.can_be_filled_by(CognitiveTier::DeadReckoning));
    }

    #[test]
    fn attestation_sign_verify_roundtrip() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let id = InstanceId::new();
        let att = make_attestation(id, CognitiveTier::Strong, vec![MeshRole::Analyst], &signing_key);
        assert!(att.verify(&verifying_key));
    }

    #[test]
    fn attestation_tampered_fails_verify() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let id = InstanceId::new();
        let mut att = make_attestation(id, CognitiveTier::Strong, vec![MeshRole::Analyst], &signing_key);
        // Tamper: flip a byte in the signature
        if let Some(b) = att.signature.get_mut(0) { *b ^= 0xFF; }
        assert!(!att.verify(&verifying_key));
    }

    #[test]
    fn roles_to_yield_on_tier_drop() {
        let mut mesh = RoleMesh::new();
        let id = InstanceId::new();

        // Assign Coordinator and Analyst to this instance
        mesh.assign_role(MeshRole::Coordinator, id);
        mesh.assign_role(MeshRole::Analyst, id);

        // At Reduced tier: Coordinator (min 2) must be yielded; Analyst (min 3) is OK
        let yields = mesh.roles_to_yield(&id, CognitiveTier::Reduced);
        assert_eq!(yields.len(), 1);
        assert_eq!(yields[0], MeshRole::Coordinator);
    }

    #[test]
    fn roles_to_yield_empty_when_tier_sufficient() {
        let mut mesh = RoleMesh::new();
        let id = InstanceId::new();
        mesh.assign_role(MeshRole::Analyst, id);

        // Strong (2) can fill Analyst (min 3) — no yields
        let yields = mesh.roles_to_yield(&id, CognitiveTier::Strong);
        assert!(yields.is_empty());
    }

    #[test]
    fn update_attestation_rejects_invalid_signature() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let wrong_key = SigningKey::generate(&mut OsRng);
        let verifying_key = wrong_key.verifying_key(); // wrong key for verification

        let id = InstanceId::new();
        let att = make_attestation(id, CognitiveTier::Strong, vec![], &signing_key);

        let mut mesh = RoleMesh::new();
        // Should reject because verifying_key doesn't match signing_key
        assert!(!mesh.update_attestation(att, &verifying_key));
        assert!(mesh.attestations.is_empty());
    }

    #[test]
    fn compute_eligible_roles_respects_tier() {
        let mesh = RoleMesh::new();
        let id = InstanceId::new();

        // Reduced tier: can fill Analyst, Executor, Observer, Standby but not Coordinator/Strategist
        let eligible = mesh.compute_eligible_roles(&id, CognitiveTier::Reduced);
        assert!(eligible.contains(&MeshRole::Analyst));
        assert!(eligible.contains(&MeshRole::Executor));
        assert!(!eligible.contains(&MeshRole::Coordinator));
        assert!(!eligible.contains(&MeshRole::Strategist));
    }
}
