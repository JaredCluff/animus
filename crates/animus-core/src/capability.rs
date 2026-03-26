//! Cognitive capability types — honest self-assessment for the AILF.
//!
//! `CognitiveTier` is the live assessment of what this AILF instance can currently do.
//! `CapabilityState` holds all probe metrics; it is updated by the `CapabilityProbe`
//! watcher (Cortex substrate, Layer 1/2) without any LLM involvement.
//!
//! The AILF reasoning thread can read these via the `get_capability_state` introspective tool.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Honest assessment of an AILF instance's current cognitive capability.
///
/// Orderable: higher discriminant = less capable.
/// `(tier as u8)` maps directly to the tier number (1 = Full, 5 = Dead Reckoning).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum CognitiveTier {
    /// Cloud + local reasoning + full healthy memory.
    Full = 1,
    /// Local reasoning only + full healthy memory.
    Strong = 2,
    /// Lightweight or slow reasoning + full memory.
    Reduced = 3,
    /// No active reasoning; VectorFS intact and queryable.
    MemoryOnly = 4,
    /// Acting from last known state; reasoning and/or memory degraded.
    DeadReckoning = 5,
}

impl CognitiveTier {
    /// Returns `true` if this tier can fill a role that requires `min_tier`.
    ///
    /// Lower tier value = more capable, so a tier can fill a role if its value ≤ min_tier.
    pub fn can_fill_role(self, min_tier: u8) -> bool {
        (self as u8) <= min_tier
    }

    /// Short human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            CognitiveTier::Full          => "Full",
            CognitiveTier::Strong        => "Strong",
            CognitiveTier::Reduced       => "Reduced",
            CognitiveTier::MemoryOnly    => "MemoryOnly",
            CognitiveTier::DeadReckoning => "DeadReckoning",
        }
    }
}

/// Live capability state published by `CapabilityProbe`.
///
/// Written by the Cortex substrate (Layer 1) on each probe cycle.
/// Read by the AILF reasoning thread via `get_capability_state` (Layer 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityState {
    /// Current assessed cognitive tier.
    pub tier: CognitiveTier,
    /// Whether the primary model endpoint responded within the probe timeout.
    pub reasoning_available: bool,
    /// Whether the embedding service is available.
    pub embedding_available: bool,
    /// Whether VectorFS is healthy (no I/O errors during probe).
    pub vectorfs_healthy: bool,
    /// Memory pressure: 0.0 (healthy) – 1.0 (full).
    pub memory_pressure: f32,
    /// Identifier of the model that was probed.
    pub active_model: Option<String>,
    /// Round-trip TCP latency to the model endpoint from the last probe (milliseconds).
    pub latency_ms: Option<u64>,
    /// When this state was last written by the probe.
    pub last_probed: DateTime<Utc>,
}

impl Default for CapabilityState {
    /// Conservative default: assume `MemoryOnly` until the first probe completes.
    ///
    /// This prevents the AILF from assuming full capability before its first self-check.
    fn default() -> Self {
        Self {
            tier: CognitiveTier::MemoryOnly,
            reasoning_available: false,
            embedding_available: false,
            vectorfs_healthy: true,
            memory_pressure: 0.0,
            active_model: None,
            latency_ms: None,
            last_probed: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_ordering_more_capable_is_lower() {
        assert!(CognitiveTier::Full < CognitiveTier::Strong);
        assert!(CognitiveTier::Strong < CognitiveTier::Reduced);
        assert!(CognitiveTier::Reduced < CognitiveTier::MemoryOnly);
        assert!(CognitiveTier::MemoryOnly < CognitiveTier::DeadReckoning);
    }

    #[test]
    fn can_fill_role_respects_min_tier() {
        // Full (1) can fill any role (min_tier 1–5)
        assert!(CognitiveTier::Full.can_fill_role(1));
        assert!(CognitiveTier::Full.can_fill_role(5));

        // Strong (2) can fill roles requiring tier ≤ 2
        assert!(CognitiveTier::Strong.can_fill_role(2));
        assert!(!CognitiveTier::Strong.can_fill_role(1));

        // DeadReckoning (5) can only fill roles requiring tier 5
        assert!(CognitiveTier::DeadReckoning.can_fill_role(5));
        assert!(!CognitiveTier::DeadReckoning.can_fill_role(4));
    }

    #[test]
    fn tier_labels_are_stable() {
        assert_eq!(CognitiveTier::Full.label(), "Full");
        assert_eq!(CognitiveTier::Strong.label(), "Strong");
        assert_eq!(CognitiveTier::Reduced.label(), "Reduced");
        assert_eq!(CognitiveTier::MemoryOnly.label(), "MemoryOnly");
        assert_eq!(CognitiveTier::DeadReckoning.label(), "DeadReckoning");
    }

    #[test]
    fn default_state_is_memory_only() {
        let state = CapabilityState::default();
        assert_eq!(state.tier, CognitiveTier::MemoryOnly);
        assert!(!state.reasoning_available);
        assert!(!state.embedding_available);
        assert!(state.vectorfs_healthy);
        assert_eq!(state.memory_pressure, 0.0);
        assert!(state.active_model.is_none());
        assert!(state.latency_ms.is_none());
    }

    #[test]
    fn capability_state_serde_roundtrip() {
        let state = CapabilityState {
            tier: CognitiveTier::Strong,
            reasoning_available: true,
            embedding_available: true,
            vectorfs_healthy: true,
            memory_pressure: 0.15,
            active_model: Some("ollama:llama3".to_string()),
            latency_ms: Some(450),
            last_probed: Utc::now(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: CapabilityState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.tier, CognitiveTier::Strong);
        assert_eq!(decoded.latency_ms, Some(450));
        assert!(decoded.reasoning_available);
    }
}
