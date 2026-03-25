use std::path::PathBuf;
use std::sync::Arc;

use animus_core::sensorium::*;
use animus_core::EmbeddingService;
use parking_lot::{Mutex, RwLock};

use crate::attention::{AttentionFilter, AttentionRule};
use crate::audit::AuditTrail;
use crate::consent::ConsentEngine;

/// Outcome of processing a single event through the pipeline.
pub struct ProcessOutcome {
    pub permitted: bool,
    pub passed_attention: bool,
    pub audit_action: AuditAction,
}

/// Wires consent, attention, and audit into a single event processing pipeline.
pub struct SensoriumOrchestrator {
    consent: ConsentEngine,
    attention: AttentionFilter,
    audit: Mutex<AuditTrail>,
    attention_threshold: f32,
    embedder: Arc<dyn EmbeddingService>,
    goal_embeddings: RwLock<Vec<Vec<f32>>>,
}

impl SensoriumOrchestrator {
    pub fn new(
        policies: Vec<ConsentPolicy>,
        attention_rules: Vec<AttentionRule>,
        audit_path: PathBuf,
        attention_threshold: f32,
        embedder: Arc<dyn EmbeddingService>,
    ) -> animus_core::Result<Self> {
        let audit = AuditTrail::open(&audit_path)?;
        Ok(Self {
            consent: ConsentEngine::new(policies),
            attention: AttentionFilter::new(attention_rules),
            audit: Mutex::new(audit),
            attention_threshold,
            embedder,
            goal_embeddings: RwLock::new(Vec::new()),
        })
    }

    /// Update the cached goal embeddings used by tier 2 attention.
    /// Call this when goals are added, removed, or changed.
    pub fn set_goal_embeddings(&self, embeddings: Vec<Vec<f32>>) {
        *self.goal_embeddings.write() = embeddings;
    }

    pub async fn process_event(&self, event: SensorEvent) -> animus_core::Result<ProcessOutcome> {
        // Step 1: Consent check
        let consent_result = self.consent.evaluate(&event);
        if consent_result.permission == Permission::Deny {
            let entry = AuditEntry {
                timestamp: chrono::Utc::now(),
                event_id: event.id,
                consent_policy: consent_result.policy_id,
                attention_tier_reached: 0,
                action_taken: AuditAction::DeniedByConsent,
                segment_created: None,
            };
            self.audit.lock().append(&entry)?;
            return Ok(ProcessOutcome {
                permitted: false,
                passed_attention: false,
                audit_action: AuditAction::DeniedByConsent,
            });
        }

        // If AllowAnonymized, strip identifying details from the event data
        // before it flows through attention/storage. The event_type and structure
        // are preserved so rule-based attention still works, but string values
        // that could contain PII (paths, IPs, names) are redacted.
        let event = if consent_result.permission == Permission::AllowAnonymized {
            let mut anon = event.clone();
            anon.data = anonymize_value(&anon.data);
            anon.source = "<redacted>".to_string();
            anon
        } else {
            event
        };

        // Step 2: Tier 1 attention filter (microsecond, rule-based)
        let tier1_decision = self.attention.tier1_evaluate(&event);
        let (mut passed, mut action, mut tier_reached) = match &tier1_decision {
            AttentionDecision::Pass { promoted } => {
                let action = if *promoted {
                    AuditAction::Promoted
                } else {
                    AuditAction::Logged
                };
                (true, action, 1u8)
            }
            AttentionDecision::Drop { .. } => (false, AuditAction::Ignored, 1u8),
        };

        // Step 3: Tier 2 attention filter (embedding similarity)
        // Only runs when tier 1 passed without promoting (needs deeper evaluation)
        // and when we have goal embeddings to compare against.
        // Clone goal embeddings before async boundary (parking_lot guards are !Send).
        let goals_snapshot: Vec<Vec<f32>> = self.goal_embeddings.read().clone();
        if passed && action == AuditAction::Logged && !goals_snapshot.is_empty() {
            let event_text = serde_json::to_string(&event.data).unwrap_or_default();
            match self.embedder.embed_text(&event_text).await {
                Ok(event_embedding) => {
                    let tier2_decision = self.attention.tier2_evaluate(
                        &event_embedding,
                        &goals_snapshot,
                        self.attention_threshold,
                    );
                    tier_reached = 2;
                    match tier2_decision {
                        AttentionDecision::Pass { .. } => {
                            action = AuditAction::Promoted;
                        }
                        AttentionDecision::Drop { reason } => {
                            tracing::debug!(
                                event_id = %event.id,
                                event_type = ?event.event_type,
                                reason = %reason,
                                "Tier 2 attention: event filtered (embedding similarity below threshold)"
                            );
                            passed = false;
                            action = AuditAction::Ignored;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Tier 2 attention: embedding failed: {e}");
                    // Fall through with tier 1 decision
                }
            }
        }

        // Step 4: Audit
        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            event_id: event.id,
            consent_policy: consent_result.policy_id,
            attention_tier_reached: tier_reached,
            action_taken: action,
            segment_created: None,
        };
        self.audit.lock().append(&entry)?;

        Ok(ProcessOutcome {
            permitted: true,
            passed_attention: passed,
            audit_action: action,
        })
    }
}

/// Recursively redact string values in a JSON value while preserving structure.
/// Object keys are kept so rule matching on structure still works.
fn anonymize_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let redacted: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), anonymize_value(v)))
                .collect();
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(anonymize_value).collect())
        }
        serde_json::Value::String(_) => serde_json::Value::String("<redacted>".to_string()),
        // Numbers, booleans, and nulls are kept — they're structural, not identifying
        other => other.clone(),
    }
}
