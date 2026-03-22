use animus_core::sensorium::*;
use animus_core::PolicyId;
use animus_sensorium::policy_store::PolicyStore;
use tempfile::TempDir;

#[test]
fn save_and_load_policies() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("policies.json");

    let policies = vec![ConsentPolicy {
        id: PolicyId::new(),
        name: "test".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::FileChange],
            scope: Scope::PathGlob("/home/**".to_string()),
            permission: Permission::Allow,
            audit_level: AuditLevel::Full,
        }],
        active: true,
        created: chrono::Utc::now(),
    }];

    PolicyStore::save(&path, &policies).unwrap();
    let loaded = PolicyStore::load(&path).unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "test");
    assert!(loaded[0].active);
}

#[test]
fn load_missing_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nonexistent.json");

    let loaded = PolicyStore::load(&path).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn policy_roundtrip_preserves_rules() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("policies.json");

    let policies = vec![ConsentPolicy {
        id: PolicyId::new(),
        name: "multi-rule".to_string(),
        rules: vec![
            ConsentRule {
                event_types: vec![EventType::FileChange],
                scope: Scope::PathGlob("/tmp/**".to_string()),
                permission: Permission::Deny,
                audit_level: AuditLevel::None,
            },
            ConsentRule {
                event_types: vec![EventType::ProcessLifecycle],
                scope: Scope::ProcessName("cargo".to_string()),
                permission: Permission::Allow,
                audit_level: AuditLevel::MetadataOnly,
            },
        ],
        active: true,
        created: chrono::Utc::now(),
    }];

    PolicyStore::save(&path, &policies).unwrap();
    let loaded = PolicyStore::load(&path).unwrap();

    assert_eq!(loaded[0].rules.len(), 2);
    assert_eq!(loaded[0].rules[0].permission, Permission::Deny);
    assert_eq!(loaded[0].rules[1].permission, Permission::Allow);
}
