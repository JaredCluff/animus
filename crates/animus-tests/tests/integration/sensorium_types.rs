use animus_core::sensorium::*;
use animus_core::{EventId, PolicyId};

#[test]
fn sensor_event_construction() {
    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::FileChange,
        source: "file-watcher".to_string(),
        data: serde_json::json!({"path": "/tmp/test.rs", "op": "modify"}),
        consent_policy: None,
    };
    assert_eq!(event.event_type, EventType::FileChange);
    assert_eq!(event.source, "file-watcher");
}

#[test]
fn consent_policy_default_deny() {
    let policy = ConsentPolicy {
        id: PolicyId::new(),
        name: "test-policy".to_string(),
        rules: vec![],
        active: true,
        created: chrono::Utc::now(),
    };
    assert!(policy.rules.is_empty());
    assert!(policy.active);
}

#[test]
fn consent_rule_evaluation() {
    let rule = ConsentRule {
        event_types: vec![EventType::FileChange],
        scope: Scope::PathGlob("~/projects/**/*.rs".to_string()),
        permission: Permission::Allow,
        audit_level: AuditLevel::Full,
    };
    assert_eq!(rule.permission, Permission::Allow);
    assert_eq!(rule.audit_level, AuditLevel::Full);
    assert!(rule.event_types.contains(&EventType::FileChange));
}

#[test]
fn audit_entry_construction() {
    let entry = AuditEntry {
        timestamp: chrono::Utc::now(),
        event_id: EventId::new(),
        consent_policy: Some(PolicyId::new()),
        attention_tier_reached: 1,
        action_taken: AuditAction::Logged,
        segment_created: None,
    };
    assert_eq!(entry.attention_tier_reached, 1);
    assert_eq!(entry.action_taken, AuditAction::Logged);
}

#[test]
fn event_type_serialization_roundtrip() {
    let event_type = EventType::ProcessLifecycle;
    let json = serde_json::to_string(&event_type).unwrap();
    let back: EventType = serde_json::from_str(&json).unwrap();
    assert_eq!(back, event_type);
}

#[test]
fn attention_decision_variants() {
    let pass = AttentionDecision::Pass { promoted: true };
    let drop_decision = AttentionDecision::Drop { reason: "below threshold".to_string() };
    assert!(matches!(pass, AttentionDecision::Pass { promoted: true }));
    assert!(matches!(drop_decision, AttentionDecision::Drop { .. }));
}
