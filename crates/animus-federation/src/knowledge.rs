use animus_core::{InstanceId, PolicyId, Principal, Segment};
use crate::protocol::SegmentAnnouncement;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FederationScope {
    ByTag(String, String),
    BySourceType(String),
    AllNonPrivate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum FederationPermission {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationRule {
    pub scope: FederationScope,
    pub permission: FederationPermission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationPolicy {
    pub id: PolicyId,
    pub name: String,
    pub active: bool,
    pub publish_rules: Vec<FederationRule>,
    pub subscribe_rules: Vec<FederationRule>,
}

pub struct KnowledgeSharing {
    policies: Vec<FederationPolicy>,
    relevance_threshold: f32,
}

impl KnowledgeSharing {
    pub fn new(policies: Vec<FederationPolicy>, relevance_threshold: f32) -> Self {
        Self { policies, relevance_threshold }
    }

    /// Check if a segment can be published to a specific target AILF.
    pub fn can_publish(&self, segment: &Segment, target: &InstanceId) -> bool {
        // If segment has explicit observable_by including the target, always allow
        if segment.observable_by.iter().any(|p| matches!(p, Principal::Ailf(id) if id == target)) {
            return true;
        }

        // If segment has observable_by entries but target isn't in them, deny
        if !segment.observable_by.is_empty() {
            return false;
        }

        // Check federation policies
        for policy in &self.policies {
            if !policy.active {
                continue;
            }
            for rule in &policy.publish_rules {
                if self.scope_matches(&rule.scope, segment) {
                    return matches!(rule.permission, FederationPermission::Allow);
                }
            }
        }

        false // default deny
    }

    /// Check if an announcement is semantically relevant to active goals.
    pub fn is_relevant(&self, announcement: &SegmentAnnouncement, goal_embeddings: &[Vec<f32>]) -> bool {
        if goal_embeddings.is_empty() {
            return false;
        }
        for goal_emb in goal_embeddings {
            let similarity = cosine_similarity(&announcement.embedding, goal_emb);
            if similarity >= self.relevance_threshold {
                return true;
            }
        }
        false
    }

    fn scope_matches(&self, scope: &FederationScope, segment: &Segment) -> bool {
        match scope {
            FederationScope::AllNonPrivate => {
                segment.observable_by.is_empty()
            }
            FederationScope::ByTag(_key, _value) => {
                // Tags are not currently in the segment model — match by content
                // For V0.1, this always returns false
                false
            }
            FederationScope::BySourceType(source_type) => {
                let actual = match &segment.source {
                    animus_core::Source::Conversation { .. } => "conversation",
                    animus_core::Source::Observation { .. } => "observation",
                    animus_core::Source::Consolidation { .. } => "consolidation",
                    animus_core::Source::Federation { .. } => "federation",
                    animus_core::Source::SelfDerived { .. } => "self-derived",
                    animus_core::Source::Manual { .. } => "manual",
                };
                actual == source_type
            }
        }
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
