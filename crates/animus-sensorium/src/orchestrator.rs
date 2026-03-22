use animus_core::sensorium::*;
use std::path::PathBuf;
use std::sync::Mutex;

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
    _attention_threshold: f32,
}

impl SensoriumOrchestrator {
    pub fn new(
        policies: Vec<ConsentPolicy>,
        attention_rules: Vec<AttentionRule>,
        audit_path: PathBuf,
        attention_threshold: f32,
    ) -> animus_core::Result<Self> {
        let audit = AuditTrail::open(&audit_path)?;
        Ok(Self {
            consent: ConsentEngine::new(policies),
            attention: AttentionFilter::new(attention_rules),
            audit: Mutex::new(audit),
            _attention_threshold: attention_threshold,
        })
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
            self.audit.lock().unwrap().append(&entry)?;
            return Ok(ProcessOutcome {
                permitted: false,
                passed_attention: false,
                audit_action: AuditAction::DeniedByConsent,
            });
        }

        // Step 2: Tier 1 attention filter
        let attention_decision = self.attention.tier1_evaluate(&event);
        let (passed, action) = match &attention_decision {
            AttentionDecision::Pass { promoted } => {
                let action = if *promoted {
                    AuditAction::Promoted
                } else {
                    AuditAction::Logged
                };
                (true, action)
            }
            AttentionDecision::Drop { .. } => (false, AuditAction::Ignored),
        };

        // Step 3: Audit
        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            event_id: event.id,
            consent_policy: consent_result.policy_id,
            attention_tier_reached: 1,
            action_taken: action,
            segment_created: None,
        };
        self.audit.lock().unwrap().append(&entry)?;

        Ok(ProcessOutcome {
            permitted: true,
            passed_attention: passed,
            audit_action: action,
        })
    }
}
