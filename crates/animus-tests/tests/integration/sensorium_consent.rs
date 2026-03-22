use animus_core::sensorium::*;
use animus_core::{EventId, PolicyId};
use animus_sensorium::consent::ConsentEngine;

fn make_file_event(path: &str) -> SensorEvent {
    SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::FileChange,
        source: "file-watcher".to_string(),
        data: serde_json::json!({"path": path, "op": "modify"}),
        consent_policy: None,
    }
}

#[test]
fn default_deny_no_rules() {
    let engine = ConsentEngine::new(vec![]);
    let event = make_file_event("/home/user/secret.txt");
    let result = engine.evaluate(&event);
    assert_eq!(result.permission, Permission::Deny);
}

#[test]
fn allow_matching_path() {
    let policy = ConsentPolicy {
        id: PolicyId::new(),
        name: "dev-files".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::FileChange],
            scope: Scope::PathGlob("/home/user/projects/**".to_string()),
            permission: Permission::Allow,
            audit_level: AuditLevel::Full,
        }],
        active: true,
        created: chrono::Utc::now(),
        created_by: None,
    };
    let engine = ConsentEngine::new(vec![policy]);
    let event = make_file_event("/home/user/projects/animus/src/main.rs");
    let result = engine.evaluate(&event);
    assert_eq!(result.permission, Permission::Allow);
}

#[test]
fn deny_non_matching_path() {
    let policy = ConsentPolicy {
        id: PolicyId::new(),
        name: "dev-files".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::FileChange],
            scope: Scope::PathGlob("/home/user/projects/**".to_string()),
            permission: Permission::Allow,
            audit_level: AuditLevel::Full,
        }],
        active: true,
        created: chrono::Utc::now(),
        created_by: None,
    };
    let engine = ConsentEngine::new(vec![policy]);
    let event = make_file_event("/etc/passwd");
    let result = engine.evaluate(&event);
    assert_eq!(result.permission, Permission::Deny);
}

#[test]
fn explicit_deny_overrides_allow() {
    let policy = ConsentPolicy {
        id: PolicyId::new(),
        name: "mixed".to_string(),
        rules: vec![
            ConsentRule {
                event_types: vec![EventType::FileChange],
                scope: Scope::PathGlob("/home/user/projects/.env".to_string()),
                permission: Permission::Deny,
                audit_level: AuditLevel::MetadataOnly,
            },
            ConsentRule {
                event_types: vec![EventType::FileChange],
                scope: Scope::PathGlob("/home/user/projects/**".to_string()),
                permission: Permission::Allow,
                audit_level: AuditLevel::Full,
            },
        ],
        active: true,
        created: chrono::Utc::now(),
        created_by: None,
    };
    let engine = ConsentEngine::new(vec![policy]);
    let event = make_file_event("/home/user/projects/.env");
    let result = engine.evaluate(&event);
    assert_eq!(result.permission, Permission::Deny);
}

#[test]
fn inactive_policy_is_skipped() {
    let policy = ConsentPolicy {
        id: PolicyId::new(),
        name: "disabled".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::FileChange],
            scope: Scope::All,
            permission: Permission::Allow,
            audit_level: AuditLevel::Full,
        }],
        active: false,
        created: chrono::Utc::now(),
        created_by: None,
    };
    let engine = ConsentEngine::new(vec![policy]);
    let event = make_file_event("/tmp/anything.txt");
    let result = engine.evaluate(&event);
    assert_eq!(result.permission, Permission::Deny);
}

#[test]
fn process_event_scope_matching() {
    let policy = ConsentPolicy {
        id: PolicyId::new(),
        name: "process-watch".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::ProcessLifecycle],
            scope: Scope::ProcessName("cargo".to_string()),
            permission: Permission::Allow,
            audit_level: AuditLevel::MetadataOnly,
        }],
        active: true,
        created: chrono::Utc::now(),
        created_by: None,
    };
    let engine = ConsentEngine::new(vec![policy]);

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::ProcessLifecycle,
        source: "process-monitor".to_string(),
        data: serde_json::json!({"name": "cargo", "pid": 1234, "op": "start"}),
        consent_policy: None,
    };
    let result = engine.evaluate(&event);
    assert_eq!(result.permission, Permission::Allow);
}
