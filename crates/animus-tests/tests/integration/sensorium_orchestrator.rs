use animus_core::sensorium::*;
use animus_core::{EventId, PolicyId};
use animus_sensorium::attention::{AttentionRule, RuleAction};
use animus_sensorium::orchestrator::SensoriumOrchestrator;
use tempfile::TempDir;

fn make_policies() -> Vec<ConsentPolicy> {
    vec![ConsentPolicy {
        id: PolicyId::new(),
        name: "allow-projects".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::FileChange],
            scope: Scope::PathGlob("/home/user/projects/**".to_string()),
            permission: Permission::Allow,
            audit_level: AuditLevel::Full,
        }],
        active: true,
        created: chrono::Utc::now(),
    }]
}

fn make_rules() -> Vec<AttentionRule> {
    vec![AttentionRule {
        event_types: vec![EventType::FileChange],
        path_patterns: vec!["/home/user/projects/.git/**".to_string()],
        action: RuleAction::Ignore,
    }]
}

#[tokio::test]
async fn pipeline_processes_permitted_event() {
    let dir = TempDir::new().unwrap();
    let audit_path = dir.path().join("audit.jsonl");

    let orch = SensoriumOrchestrator::new(
        make_policies(),
        make_rules(),
        audit_path.clone(),
        0.5,
    )
    .unwrap();

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::FileChange,
        source: "file-watcher".to_string(),
        data: serde_json::json!({"path": "/home/user/projects/animus/src/main.rs", "op": "modify"}),
        consent_policy: None,
    };

    let result = orch.process_event(event).await;
    assert!(result.is_ok());
    let outcome = result.unwrap();
    assert!(outcome.permitted);
    assert!(outcome.passed_attention);
}

#[tokio::test]
async fn pipeline_blocks_denied_event() {
    let dir = TempDir::new().unwrap();
    let audit_path = dir.path().join("audit.jsonl");

    let orch = SensoriumOrchestrator::new(
        make_policies(),
        make_rules(),
        audit_path.clone(),
        0.5,
    )
    .unwrap();

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::FileChange,
        source: "file-watcher".to_string(),
        data: serde_json::json!({"path": "/etc/passwd", "op": "read"}),
        consent_policy: None,
    };

    let result = orch.process_event(event).await;
    assert!(result.is_ok());
    let outcome = result.unwrap();
    assert!(!outcome.permitted);
}

#[tokio::test]
async fn pipeline_filters_git_noise() {
    let dir = TempDir::new().unwrap();
    let audit_path = dir.path().join("audit.jsonl");

    let orch = SensoriumOrchestrator::new(
        make_policies(),
        make_rules(),
        audit_path.clone(),
        0.5,
    )
    .unwrap();

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::FileChange,
        source: "file-watcher".to_string(),
        data: serde_json::json!({"path": "/home/user/projects/.git/objects/abc", "op": "create"}),
        consent_policy: None,
    };

    let result = orch.process_event(event).await;
    assert!(result.is_ok());
    let outcome = result.unwrap();
    assert!(outcome.permitted);
    assert!(!outcome.passed_attention);
}

#[tokio::test]
async fn pipeline_writes_audit_entries() {
    let dir = TempDir::new().unwrap();
    let audit_path = dir.path().join("audit.jsonl");

    let orch = SensoriumOrchestrator::new(
        make_policies(),
        make_rules(),
        audit_path.clone(),
        0.5,
    )
    .unwrap();

    for i in 0..3 {
        let event = SensorEvent {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            event_type: EventType::FileChange,
            source: "file-watcher".to_string(),
            data: serde_json::json!({"path": format!("/home/user/projects/file{i}.rs"), "op": "create"}),
            consent_policy: None,
        };
        orch.process_event(event).await.unwrap();
    }

    let entries = animus_sensorium::audit::AuditTrail::read_all(&audit_path).unwrap();
    assert_eq!(entries.len(), 3);
}
