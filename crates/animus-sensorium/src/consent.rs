use animus_core::sensorium::*;

/// Result of consent evaluation.
pub struct ConsentResult {
    pub permission: Permission,
    pub policy_id: Option<animus_core::PolicyId>,
    pub audit_level: AuditLevel,
}

/// Evaluates events against human-defined consent policies.
/// Default-deny: events with no matching rule are denied.
/// Rules within active policies are evaluated in order; first match wins.
pub struct ConsentEngine {
    policies: Vec<ConsentPolicy>,
}

impl ConsentEngine {
    pub fn new(policies: Vec<ConsentPolicy>) -> Self {
        Self { policies }
    }

    pub fn evaluate(&self, event: &SensorEvent) -> ConsentResult {
        for policy in &self.policies {
            if !policy.active {
                continue;
            }
            for rule in &policy.rules {
                if self.rule_matches(rule, event) {
                    return ConsentResult {
                        permission: rule.permission,
                        policy_id: Some(policy.id),
                        audit_level: rule.audit_level,
                    };
                }
            }
        }
        // Default deny
        ConsentResult {
            permission: Permission::Deny,
            policy_id: None,
            audit_level: AuditLevel::MetadataOnly,
        }
    }

    fn rule_matches(&self, rule: &ConsentRule, event: &SensorEvent) -> bool {
        if !rule.event_types.contains(&event.event_type) {
            return false;
        }
        self.scope_matches(&rule.scope, event)
    }

    fn scope_matches(&self, scope: &Scope, event: &SensorEvent) -> bool {
        match scope {
            Scope::All => true,
            Scope::PathGlob(pattern) => {
                if let Some(path) = event.data.get("path").and_then(|v| v.as_str()) {
                    glob_match(pattern, path)
                } else {
                    false
                }
            }
            Scope::ProcessName(name) => {
                if let Some(proc_name) = event.data.get("name").and_then(|v| v.as_str()) {
                    proc_name == name
                } else {
                    false
                }
            }
        }
    }
}

/// Simple glob matching: supports `**` (any path) and `*` (single component).
fn glob_match(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        path.starts_with(prefix)
    } else if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            path.starts_with(parts[0]) && path.ends_with(parts[1])
        } else {
            path == pattern
        }
    } else {
        path == pattern
    }
}
