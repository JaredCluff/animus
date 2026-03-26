//! Succession policy — deterministic role handoff when a peer yields.
//!
//! `SuccessionPolicy::nominate()` selects the best-capable peer to inherit a
//! yielded role. The algorithm is:
//!
//! 1. Filter: candidate must meet the role's minimum tier requirement
//! 2. Sort: lower tier value first (more capable), tiebreak by lower load
//! 3. Return the top candidate, or `None` if no eligible peer exists

use crate::identity::InstanceId;
use crate::mesh::{MeshRole, VerifiedAttestation};

pub struct SuccessionPolicy;

impl SuccessionPolicy {
    /// Nominate the best successor for a `role` given current mesh attestations.
    ///
    /// - `candidates`: all known verified attestations in the mesh
    /// - `exclude`: the yielding instance (must not be re-nominated for the same role)
    ///
    /// Returns the `InstanceId` of the nominated successor, or `None` if no
    /// eligible peer is available (e.g., all are at `MemoryOnly`/`DeadReckoning`).
    pub fn nominate(
        role: MeshRole,
        candidates: &[&VerifiedAttestation],
        exclude: &InstanceId,
    ) -> Option<InstanceId> {
        let mut eligible: Vec<&&VerifiedAttestation> = candidates
            .iter()
            .filter(|a| {
                a.attestation.instance_id != *exclude &&
                role.can_be_filled_by(a.attestation.cognitive_tier)
            })
            .collect();

        // Sort: best capability (lowest tier value) first; tiebreak by lowest load
        eligible.sort_by(|a, b| {
            let tier_a = a.attestation.cognitive_tier as u8;
            let tier_b = b.attestation.cognitive_tier as u8;
            tier_a.cmp(&tier_b)
                .then_with(|| a.attestation.load.partial_cmp(&b.attestation.load).unwrap_or(std::cmp::Ordering::Equal))
        });

        eligible.first().map(|a| a.attestation.instance_id)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CognitiveTier;
    use crate::mesh::{AttestationFields, CapabilityAttestation, MeshRole, VerifiedAttestation};
    use crate::identity::InstanceId;
    use chrono::Utc;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn make_verified(
        instance_id: InstanceId,
        tier: CognitiveTier,
        load: f32,
    ) -> VerifiedAttestation {
        let signing_key = SigningKey::generate(&mut OsRng);
        let fields = AttestationFields {
            instance_id,
            cognitive_tier: tier,
            active_roles: vec![],
            available_domains: vec![],
            load,
            signed_at: Utc::now(),
        };
        let att = CapabilityAttestation::sign(fields, &signing_key);
        VerifiedAttestation { attestation: att, verified_at: Utc::now() }
    }

    #[test]
    fn nominates_most_capable_candidate() {
        let id1 = InstanceId::new();
        let id2 = InstanceId::new();
        let id3 = InstanceId::new(); // yielding

        let strong = make_verified(id1, CognitiveTier::Strong, 0.3);
        let full   = make_verified(id2, CognitiveTier::Full, 0.5);
        let excluded = make_verified(id3, CognitiveTier::Full, 0.1);

        let candidates = [&strong, &full, &excluded];
        let winner = SuccessionPolicy::nominate(MeshRole::Coordinator, &candidates, &id3);

        // id2 (Full) should beat id1 (Strong) by tier; id3 is excluded
        assert_eq!(winner, Some(id2));
    }

    #[test]
    fn tiebreaks_by_load() {
        let id1 = InstanceId::new();
        let id2 = InstanceId::new();
        let excluded = InstanceId::new();

        let high_load = make_verified(id1, CognitiveTier::Strong, 0.9);
        let low_load  = make_verified(id2, CognitiveTier::Strong, 0.1);

        let candidates = [&high_load, &low_load];
        let winner = SuccessionPolicy::nominate(MeshRole::Coordinator, &candidates, &excluded);

        // Same tier, id2 has lower load
        assert_eq!(winner, Some(id2));
    }

    #[test]
    fn returns_none_when_no_eligible_candidates() {
        let id_degraded = InstanceId::new();
        let excluded = InstanceId::new();

        // Coordinator requires tier ≤ 2 (Full/Strong); MemoryOnly (4) is ineligible
        let degraded = make_verified(id_degraded, CognitiveTier::MemoryOnly, 0.0);

        let candidates = [&degraded];
        let winner = SuccessionPolicy::nominate(MeshRole::Coordinator, &candidates, &excluded);
        assert!(winner.is_none());
    }

    #[test]
    fn excludes_yielding_instance() {
        let id_yielding = InstanceId::new();

        // Only candidate is the one yielding the role
        let att = make_verified(id_yielding, CognitiveTier::Full, 0.0);
        let candidates = [&att];
        let winner = SuccessionPolicy::nominate(MeshRole::Coordinator, &candidates, &id_yielding);
        assert!(winner.is_none());
    }

    #[test]
    fn executor_role_accepts_any_tier() {
        let id1 = InstanceId::new();
        let excluded = InstanceId::new();

        // Executor min_tier = 5 — even DeadReckoning can fill it
        let degraded = make_verified(id1, CognitiveTier::DeadReckoning, 0.5);
        let candidates = [&degraded];
        let winner = SuccessionPolicy::nominate(MeshRole::Executor, &candidates, &excluded);
        assert_eq!(winner, Some(id1));
    }
}
