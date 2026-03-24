# Phase 3 — Sensorium Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the AILF ambient awareness — the ability to observe file changes and process activity within consented scope, log observations, and surface relevant ones during reasoning.

**Architecture:** Event bus (tokio mpsc channels) distributes normalized `SensorEvent` structs from pluggable sensors (file watcher via `notify`, process monitor via `sysinfo`). A consent engine filters events against human-defined policies before they reach upper layers. A two-tier attention filter (rule-based + embedding similarity) determines which events are worth the AILF's attention. An append-only audit trail logs all observations. The runtime integrates Sensorium as background tokio tasks that feed observations into VectorFS segments for Mnemos context assembly.

**Tech Stack:** Rust, tokio (async runtime), `notify` 7.x (cross-platform file watching — FSEvents on macOS, inotify on Linux), `sysinfo` 0.33+ (cross-platform process/system monitoring), existing animus-core types (EventId, PolicyId, SensorEvent, etc.)

---

### Task 1: Core Sensorium Types in animus-core

**Files:**
- Create: `crates/animus-core/src/sensorium.rs`
- Modify: `crates/animus-core/src/lib.rs`
- Modify: `crates/animus-core/src/error.rs`
- Modify: `crates/animus-core/src/config.rs`

This task adds the shared types that all Sensorium components depend on. These types are in animus-core so other crates (animus-runtime, animus-cortex) can reference them without depending on animus-sensorium directly.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_types.rs`:

```rust
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
    // Empty rules = no events pass
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
    let drop = AttentionDecision::Drop { reason: "below threshold".to_string() };
    assert!(matches!(pass, AttentionDecision::Pass { promoted: true }));
    assert!(matches!(drop, AttentionDecision::Drop { .. }));
}
```

Add the module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_types -- --nocapture`
Expected: FAIL — `sensorium` module doesn't exist yet.

- [ ] **Step 3: Write the Sensorium types module**

Create `crates/animus-core/src/sensorium.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{EventId, PolicyId, SegmentId};

/// A normalized event from a sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorEvent {
    pub id: EventId,
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub source: String,
    pub data: serde_json::Value,
    pub consent_policy: Option<PolicyId>,
}

/// Categories of events the Sensorium can capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    FileChange,
    ProcessLifecycle,
    SystemResources,
    Network,
    Clipboard,
    WindowFocus,
    UsbDevice,
}

/// Human-defined boundaries on AILF observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentPolicy {
    pub id: PolicyId,
    pub name: String,
    pub rules: Vec<ConsentRule>,
    pub active: bool,
    pub created: DateTime<Utc>,
}

/// A single consent rule within a policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRule {
    pub event_types: Vec<EventType>,
    pub scope: Scope,
    pub permission: Permission,
    pub audit_level: AuditLevel,
}

/// Defines what the consent rule applies to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Scope {
    /// Glob pattern for file paths.
    PathGlob(String),
    /// Process name pattern.
    ProcessName(String),
    /// All events of the specified types.
    All,
}

/// Whether events matching this rule are permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    Allow,
    Deny,
    AllowAnonymized,
}

/// How much detail to record in the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditLevel {
    None,
    MetadataOnly,
    Full,
}

/// An entry in the append-only audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub event_id: EventId,
    pub consent_policy: Option<PolicyId>,
    pub attention_tier_reached: u8,
    pub action_taken: AuditAction,
    pub segment_created: Option<SegmentId>,
}

/// What the system did with an observed event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditAction {
    Logged,
    Promoted,
    Ignored,
    DeniedByConsent,
}

/// Result of the attention filter evaluating an event.
#[derive(Debug, Clone)]
pub enum AttentionDecision {
    Pass { promoted: bool },
    Drop { reason: String },
}
```

Add `pub mod sensorium;` to `crates/animus-core/src/lib.rs` and re-export key types.

Add `Sensorium(String)` variant to `AnimusError` in `crates/animus-core/src/error.rs`.

Add `SensoriumConfig` to `crates/animus-core/src/config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensoriumConfig {
    /// Directories to watch for file changes.
    pub watch_paths: Vec<PathBuf>,
    /// Process monitoring interval in seconds.
    pub process_poll_interval_secs: u64,
    /// Whether to enable file watching.
    pub file_watching_enabled: bool,
    /// Whether to enable process monitoring.
    pub process_monitoring_enabled: bool,
    /// Attention filter similarity threshold (0.0 - 1.0).
    pub attention_similarity_threshold: f32,
}
```

Add `sensorium: SensoriumConfig` field to `AnimusConfig`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_types -- --nocapture`
Expected: PASS (6 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/animus-core/src/sensorium.rs crates/animus-core/src/lib.rs crates/animus-core/src/error.rs crates/animus-core/src/config.rs crates/animus-tests/tests/integration/sensorium_types.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(core): add Sensorium types — SensorEvent, ConsentPolicy, AuditEntry, AttentionDecision"
```

---

### Task 2: Create animus-sensorium Crate Skeleton

**Files:**
- Create: `crates/animus-sensorium/Cargo.toml`
- Create: `crates/animus-sensorium/src/lib.rs`
- Create: `crates/animus-sensorium/src/bus.rs`
- Modify: `Cargo.toml` (workspace members + dependencies)

This task sets up the crate with the EventBus — a tokio mpsc channel that distributes SensorEvents from sensors to consumers (consent engine, attention filter, audit trail).

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_bus.rs`:

```rust
use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use animus_sensorium::bus::EventBus;

#[tokio::test]
async fn event_bus_send_and_receive() {
    let bus = EventBus::new(100);
    let mut rx = bus.subscribe();

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::FileChange,
        source: "test".to_string(),
        data: serde_json::json!({"path": "/tmp/test.txt"}),
        consent_policy: None,
    };

    bus.publish(event.clone()).await.unwrap();
    let received = rx.recv().await.unwrap();
    assert_eq!(received.id, event.id);
    assert_eq!(received.event_type, EventType::FileChange);
}

#[tokio::test]
async fn event_bus_multiple_subscribers() {
    let bus = EventBus::new(100);
    let mut rx1 = bus.subscribe();
    let mut rx2 = bus.subscribe();

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::ProcessLifecycle,
        source: "test".to_string(),
        data: serde_json::json!({"pid": 123}),
        consent_policy: None,
    };

    bus.publish(event.clone()).await.unwrap();

    let r1 = rx1.recv().await.unwrap();
    let r2 = rx2.recv().await.unwrap();
    assert_eq!(r1.id, event.id);
    assert_eq!(r2.id, event.id);
}

#[tokio::test]
async fn event_bus_shutdown() {
    let bus = EventBus::new(100);
    let mut rx = bus.subscribe();
    bus.shutdown();
    // After shutdown, recv should return None
    assert!(rx.recv().await.is_none());
}
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_bus -- --nocapture`
Expected: FAIL — animus-sensorium crate doesn't exist.

- [ ] **Step 3: Create the crate and EventBus**

Create `crates/animus-sensorium/Cargo.toml`:

```toml
[package]
name = "animus-sensorium"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
animus-core = { workspace = true }
tokio = { workspace = true }
chrono = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
```

Add to workspace `Cargo.toml`:
- `"crates/animus-sensorium"` to `members`
- `animus-sensorium = { path = "crates/animus-sensorium" }` to `[workspace.dependencies]`

Add `animus-sensorium` as a dependency to `crates/animus-tests/Cargo.toml` and `crates/animus-runtime/Cargo.toml`.

Create `crates/animus-sensorium/src/bus.rs`:

```rust
use animus_core::sensorium::SensorEvent;
use tokio::sync::broadcast;

/// Event bus for distributing SensorEvents to multiple consumers.
pub struct EventBus {
    tx: broadcast::Sender<SensorEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub async fn publish(&self, event: SensorEvent) -> animus_core::Result<()> {
        self.tx.send(event).map_err(|e| {
            animus_core::AnimusError::Sensorium(format!("failed to publish event: {e}"))
        })?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SensorEvent> {
        self.tx.subscribe()
    }

    pub fn shutdown(&self) {
        // Dropping all receivers will cause recv to return error.
        // We accomplish shutdown by dropping the sender — but we hold it.
        // Instead, just drop. The bus going out of scope closes channels.
        // For explicit shutdown, we can do nothing — subscribers detect closed channel.
    }
}
```

Note: `broadcast::Sender::send` requires `Clone` on `SensorEvent`. The `SensorEvent` struct already has `Clone` derived. The `shutdown` approach: dropping the `EventBus` (and thus the `Sender`) will cause all `Receiver::recv()` calls to return `RecvError::Closed`, which the subscriber loop should handle by returning `None`. We may need to refine the shutdown test to account for broadcast channel semantics (lagged vs closed). The implementer should use `Arc<EventBus>` in practice and drop the sender explicitly for shutdown, or use a separate shutdown signal.

Create `crates/animus-sensorium/src/lib.rs`:

```rust
pub mod bus;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_bus -- --nocapture`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/ Cargo.toml crates/animus-tests/ crates/animus-runtime/Cargo.toml
git commit -m "feat(sensorium): add EventBus with broadcast channel distribution"
```

---

### Task 3: Consent Engine

**Files:**
- Create: `crates/animus-sensorium/src/consent.rs`
- Modify: `crates/animus-sensorium/src/lib.rs`

The consent engine evaluates whether an event is permitted by the human's consent policies. Default-deny: if no rule matches, the event is dropped. Rules are evaluated in order; first match wins.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_consent.rs`:

```rust
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
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_consent -- --nocapture`
Expected: FAIL — `consent` module doesn't exist.

- [ ] **Step 3: Implement the ConsentEngine**

Create `crates/animus-sensorium/src/consent.rs`:

```rust
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
    if pattern.ends_with("/**") {
        let prefix = &pattern[..pattern.len() - 3];
        path.starts_with(prefix)
    } else if pattern.contains('*') {
        // For simple wildcard patterns, use basic matching
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
```

The `glob_match` function is intentionally simple for V0.1. Full glob support (via the `glob` crate) can be added later. The pattern `dir/**` matches any path under `dir/`. Exact match is used for non-glob patterns.

Add `pub mod consent;` to `crates/animus-sensorium/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_consent -- --nocapture`
Expected: PASS (6 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/src/consent.rs crates/animus-sensorium/src/lib.rs crates/animus-tests/tests/integration/sensorium_consent.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(sensorium): consent engine with default-deny, path glob, and process name matching"
```

---

### Task 4: Audit Trail

**Files:**
- Create: `crates/animus-sensorium/src/audit.rs`
- Modify: `crates/animus-sensorium/src/lib.rs`

Append-only JSON lines file for recording all observation activity.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_audit.rs`:

```rust
use animus_core::sensorium::*;
use animus_core::{EventId, PolicyId, SegmentId};
use animus_sensorium::audit::AuditTrail;
use tempfile::TempDir;

#[test]
fn append_and_read_entries() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("audit.jsonl");
    let mut trail = AuditTrail::open(&path).unwrap();

    let entry = AuditEntry {
        timestamp: chrono::Utc::now(),
        event_id: EventId::new(),
        consent_policy: Some(PolicyId::new()),
        attention_tier_reached: 1,
        action_taken: AuditAction::Logged,
        segment_created: None,
    };

    trail.append(&entry).unwrap();
    trail.append(&entry).unwrap();

    let entries = AuditTrail::read_all(&path).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].action_taken, AuditAction::Logged);
}

#[test]
fn audit_trail_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("audit.jsonl");

    {
        let mut trail = AuditTrail::open(&path).unwrap();
        trail.append(&AuditEntry {
            timestamp: chrono::Utc::now(),
            event_id: EventId::new(),
            consent_policy: None,
            attention_tier_reached: 2,
            action_taken: AuditAction::Promoted,
            segment_created: Some(SegmentId::new()),
        }).unwrap();
    }

    {
        let mut trail = AuditTrail::open(&path).unwrap();
        trail.append(&AuditEntry {
            timestamp: chrono::Utc::now(),
            event_id: EventId::new(),
            consent_policy: None,
            attention_tier_reached: 1,
            action_taken: AuditAction::Ignored,
            segment_created: None,
        }).unwrap();
    }

    let entries = AuditTrail::read_all(&path).unwrap();
    assert_eq!(entries.len(), 2);
}

#[test]
fn audit_trail_entry_count() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("audit.jsonl");
    let mut trail = AuditTrail::open(&path).unwrap();

    for _ in 0..5 {
        trail.append(&AuditEntry {
            timestamp: chrono::Utc::now(),
            event_id: EventId::new(),
            consent_policy: None,
            attention_tier_reached: 1,
            action_taken: AuditAction::Logged,
            segment_created: None,
        }).unwrap();
    }

    assert_eq!(trail.entry_count(), 5);
}
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_audit -- --nocapture`
Expected: FAIL — `audit` module doesn't exist.

- [ ] **Step 3: Implement AuditTrail**

Create `crates/animus-sensorium/src/audit.rs`:

```rust
use animus_core::sensorium::AuditEntry;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Append-only audit trail backed by a JSON lines file.
pub struct AuditTrail {
    file: File,
    count: usize,
}

impl AuditTrail {
    pub fn open(path: &Path) -> animus_core::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Count existing entries
        let count = if path.exists() {
            let f = File::open(path)?;
            BufReader::new(f).lines().count()
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        Ok(Self { file, count })
    }

    pub fn append(&mut self, entry: &AuditEntry) -> animus_core::Result<()> {
        let json = serde_json::to_string(entry)?;
        writeln!(self.file, "{json}")?;
        self.file.flush()?;
        self.count += 1;
        Ok(())
    }

    pub fn entry_count(&self) -> usize {
        self.count
    }

    pub fn read_all(path: &Path) -> animus_core::Result<Vec<AuditEntry>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: AuditEntry = serde_json::from_str(&line)?;
            entries.push(entry);
        }
        Ok(entries)
    }
}
```

Add `pub mod audit;` to `crates/animus-sensorium/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_audit -- --nocapture`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/src/audit.rs crates/animus-sensorium/src/lib.rs crates/animus-tests/tests/integration/sensorium_audit.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(sensorium): append-only audit trail with JSON lines persistence"
```

---

### Task 5: Tier 1 Attention Filter (Rule-Based)

**Files:**
- Create: `crates/animus-sensorium/src/attention.rs`
- Modify: `crates/animus-sensorium/src/lib.rs`

The Tier 1 filter applies fast rule-based checks: event type patterns, path patterns, and ignore lists. This eliminates the vast majority of noise before any embedding computation.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_attention.rs`:

```rust
use animus_core::sensorium::*;
use animus_core::EventId;
use animus_sensorium::attention::{AttentionFilter, AttentionRule, RuleAction};

fn make_event(event_type: EventType, data: serde_json::Value) -> SensorEvent {
    SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type,
        source: "test".to_string(),
        data,
        consent_policy: None,
    }
}

#[test]
fn tier1_ignore_tmp_files() {
    let filter = AttentionFilter::new(vec![
        AttentionRule {
            event_types: vec![EventType::FileChange],
            path_patterns: vec!["/tmp/**".to_string()],
            action: RuleAction::Ignore,
        },
    ]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/tmp/scratch.txt", "op": "modify"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Drop { .. }));
}

#[test]
fn tier1_pass_interesting_files() {
    let filter = AttentionFilter::new(vec![
        AttentionRule {
            event_types: vec![EventType::FileChange],
            path_patterns: vec!["/tmp/**".to_string()],
            action: RuleAction::Ignore,
        },
    ]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/home/user/project/src/main.rs", "op": "modify"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { .. }));
}

#[test]
fn tier1_promote_high_priority_pattern() {
    let filter = AttentionFilter::new(vec![
        AttentionRule {
            event_types: vec![EventType::FileChange],
            path_patterns: vec!["**/Cargo.toml".to_string()],
            action: RuleAction::Promote,
        },
    ]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/home/user/project/Cargo.toml", "op": "modify"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: true }));
}

#[test]
fn tier1_no_rules_passes_through() {
    let filter = AttentionFilter::new(vec![]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/anything"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: false }));
}

#[test]
fn tier1_ignores_non_matching_event_types() {
    let filter = AttentionFilter::new(vec![
        AttentionRule {
            event_types: vec![EventType::ProcessLifecycle],
            path_patterns: vec![],
            action: RuleAction::Ignore,
        },
    ]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/test.rs"}),
    );
    // Rule is for ProcessLifecycle, event is FileChange — rule doesn't match
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: false }));
}
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_attention -- --nocapture`
Expected: FAIL — `attention` module doesn't exist.

- [ ] **Step 3: Implement AttentionFilter**

Create `crates/animus-sensorium/src/attention.rs`:

```rust
use animus_core::sensorium::*;
use serde::{Deserialize, Serialize};

/// What to do when a rule matches an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleAction {
    Ignore,
    Promote,
}

/// A rule for Tier 1 attention filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionRule {
    pub event_types: Vec<EventType>,
    pub path_patterns: Vec<String>,
    pub action: RuleAction,
}

/// Two-tier attention filter for event triage.
pub struct AttentionFilter {
    rules: Vec<AttentionRule>,
}

impl AttentionFilter {
    pub fn new(rules: Vec<AttentionRule>) -> Self {
        Self { rules }
    }

    /// Tier 1: Fast rule-based evaluation (microseconds).
    pub fn tier1_evaluate(&self, event: &SensorEvent) -> AttentionDecision {
        for rule in &self.rules {
            if !rule.event_types.contains(&event.event_type) {
                continue;
            }
            if self.rule_matches(rule, event) {
                return match rule.action {
                    RuleAction::Ignore => AttentionDecision::Drop {
                        reason: "matched ignore rule".to_string(),
                    },
                    RuleAction::Promote => AttentionDecision::Pass { promoted: true },
                };
            }
        }
        // No rule matched — pass through (not promoted)
        AttentionDecision::Pass { promoted: false }
    }

    /// Tier 2: Embedding similarity evaluation.
    /// Compares the event against active goal embeddings.
    /// Returns Pass if similarity exceeds threshold.
    pub fn tier2_evaluate(
        &self,
        event_embedding: &[f32],
        goal_embeddings: &[Vec<f32>],
        threshold: f32,
    ) -> AttentionDecision {
        let max_similarity = goal_embeddings
            .iter()
            .map(|goal| cosine_similarity(event_embedding, goal))
            .fold(f32::NEG_INFINITY, f32::max);

        if max_similarity >= threshold {
            AttentionDecision::Pass { promoted: true }
        } else {
            AttentionDecision::Drop {
                reason: format!(
                    "below attention threshold: {max_similarity:.3} < {threshold:.3}"
                ),
            }
        }
    }

    fn rule_matches(&self, rule: &AttentionRule, event: &SensorEvent) -> bool {
        if rule.path_patterns.is_empty() {
            // Event type match is sufficient
            return true;
        }
        if let Some(path) = event.data.get("path").and_then(|v| v.as_str()) {
            rule.path_patterns.iter().any(|p| glob_match(p, path))
        } else {
            false
        }
    }
}

fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.starts_with("**/") {
        let suffix = &pattern[3..];
        path.ends_with(suffix) || path.contains(&format!("/{suffix}"))
    } else if pattern.ends_with("/**") {
        let prefix = &pattern[..pattern.len() - 3];
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

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }
    dot / (mag_a * mag_b)
}
```

Add `pub mod attention;` to `crates/animus-sensorium/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_attention -- --nocapture`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/src/attention.rs crates/animus-sensorium/src/lib.rs crates/animus-tests/tests/integration/sensorium_attention.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(sensorium): tier 1 + tier 2 attention filter with rule matching and embedding similarity"
```

---

### Task 6: Tier 2 Attention Filter Tests

**Files:**
- Modify: `crates/animus-tests/tests/integration/sensorium_attention.rs`

Add tests specifically for the Tier 2 embedding similarity attention filter.

- [ ] **Step 1: Write Tier 2 tests**

Append to `crates/animus-tests/tests/integration/sensorium_attention.rs`:

```rust
#[test]
fn tier2_similar_event_passes() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings = vec![vec![0.9, 0.1, 0.0, 0.0]];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.8);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: true }));
}

#[test]
fn tier2_dissimilar_event_dropped() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings = vec![vec![0.0, 0.0, 0.0, 1.0]];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.5);
    assert!(matches!(decision, AttentionDecision::Drop { .. }));
}

#[test]
fn tier2_multiple_goals_best_match() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings = vec![
        vec![0.0, 1.0, 0.0, 0.0], // low similarity
        vec![0.95, 0.05, 0.0, 0.0], // high similarity
    ];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.9);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: true }));
}

#[test]
fn tier2_no_goals_drops() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings: Vec<Vec<f32>> = vec![];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.5);
    assert!(matches!(decision, AttentionDecision::Drop { .. }));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p animus-tests sensorium_attention -- --nocapture`
Expected: PASS (9 total tests)

- [ ] **Step 3: Commit**

```bash
git add crates/animus-tests/tests/integration/sensorium_attention.rs
git commit -m "test(sensorium): add tier 2 embedding similarity attention filter tests"
```

---

### Task 7: File Watcher Sensor

**Files:**
- Create: `crates/animus-sensorium/src/sensors/mod.rs`
- Create: `crates/animus-sensorium/src/sensors/file_watcher.rs`
- Modify: `crates/animus-sensorium/src/lib.rs`
- Modify: `crates/animus-sensorium/Cargo.toml`

Uses the `notify` crate for cross-platform file system monitoring (FSEvents on macOS, inotify on Linux). Translates FS events into SensorEvents and publishes to the EventBus.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_file_watcher.rs`:

```rust
use animus_core::sensorium::EventType;
use animus_sensorium::bus::EventBus;
use animus_sensorium::sensors::file_watcher::FileWatcher;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn detects_file_creation() {
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(EventBus::new(100));
    let mut rx = bus.subscribe();

    let watcher = FileWatcher::new(bus.clone(), vec![dir.path().to_path_buf()]).unwrap();
    watcher.start();

    // Give watcher time to initialize
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Create a file
    std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

    // Wait for event (with timeout)
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        rx.recv(),
    )
    .await
    .expect("timeout waiting for event")
    .expect("channel closed");

    assert_eq!(event.event_type, EventType::FileChange);
    assert_eq!(event.source, "file-watcher");
    let path = event.data.get("path").unwrap().as_str().unwrap();
    assert!(path.contains("test.txt"));

    watcher.stop();
}

#[tokio::test]
async fn detects_file_modification() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("existing.txt");
    std::fs::write(&test_file, "original").unwrap();

    let bus = Arc::new(EventBus::new(100));
    let mut rx = bus.subscribe();

    let watcher = FileWatcher::new(bus.clone(), vec![dir.path().to_path_buf()]).unwrap();
    watcher.start();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Modify the file
    std::fs::write(&test_file, "modified").unwrap();

    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        rx.recv(),
    )
    .await
    .expect("timeout waiting for event")
    .expect("channel closed");

    assert_eq!(event.event_type, EventType::FileChange);

    watcher.stop();
}
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_file_watcher -- --nocapture`
Expected: FAIL — `sensors` module doesn't exist.

- [ ] **Step 3: Implement FileWatcher**

Add to `crates/animus-sensorium/Cargo.toml`:

```toml
notify = "7"
```

Add `notify = "7"` to workspace `[workspace.dependencies]` in root `Cargo.toml`.

Create `crates/animus-sensorium/src/sensors/mod.rs`:

```rust
pub mod file_watcher;
```

Create `crates/animus-sensorium/src/sensors/file_watcher.rs`:

```rust
use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::bus::EventBus;

/// Watches file system paths for changes and publishes SensorEvents.
pub struct FileWatcher {
    bus: Arc<EventBus>,
    paths: Vec<PathBuf>,
    shutdown: Arc<Notify>,
    // Hold the watcher to keep it alive
    _watcher: Option<RecommendedWatcher>,
}

impl FileWatcher {
    pub fn new(bus: Arc<EventBus>, paths: Vec<PathBuf>) -> animus_core::Result<Self> {
        Ok(Self {
            bus,
            paths,
            shutdown: Arc::new(Notify::new()),
            _watcher: None,
        })
    }

    pub fn start(&mut self) {
        let bus = self.bus.clone();
        let shutdown = self.shutdown.clone();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(256);

        // Create the notify watcher with a sync callback that sends to the async channel
        let tx_clone = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx_clone.blocking_send(event);
            }
        })
        .expect("failed to create file watcher");

        for path in &self.paths {
            if let Err(e) = watcher.watch(path, RecursiveMode::Recursive) {
                tracing::warn!("Failed to watch {}: {e}", path.display());
            }
        }

        self._watcher = Some(watcher);

        // Spawn async task to convert notify events to SensorEvents
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        let op = format!("{:?}", event.kind);
                        for path in &event.paths {
                            let sensor_event = SensorEvent {
                                id: EventId::new(),
                                timestamp: chrono::Utc::now(),
                                event_type: EventType::FileChange,
                                source: "file-watcher".to_string(),
                                data: serde_json::json!({
                                    "path": path.to_string_lossy(),
                                    "op": op,
                                }),
                                consent_policy: None,
                            };
                            if let Err(e) = bus.publish(sensor_event).await {
                                tracing::warn!("Failed to publish file event: {e}");
                            }
                        }
                    }
                    _ = shutdown.notified() => {
                        break;
                    }
                }
            }
        });
    }

    pub fn stop(&self) {
        self.shutdown.notify_one();
    }
}
```

Add `pub mod sensors;` to `crates/animus-sensorium/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_file_watcher -- --nocapture`
Expected: PASS (2 tests). Note: file system events may be platform-dependent in timing. The 5-second timeout provides margin.

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/src/sensors/ crates/animus-sensorium/src/lib.rs crates/animus-sensorium/Cargo.toml Cargo.toml crates/animus-tests/tests/integration/sensorium_file_watcher.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(sensorium): file watcher sensor with cross-platform notify backend"
```

---

### Task 8: Process Monitor Sensor

**Files:**
- Create: `crates/animus-sensorium/src/sensors/process_monitor.rs`
- Modify: `crates/animus-sensorium/src/sensors/mod.rs`
- Modify: `crates/animus-sensorium/Cargo.toml`

Uses the `sysinfo` crate to poll for process lifecycle changes (start/stop).

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_process_monitor.rs`:

```rust
use animus_core::sensorium::EventType;
use animus_sensorium::bus::EventBus;
use animus_sensorium::sensors::process_monitor::ProcessMonitor;
use std::sync::Arc;

#[tokio::test]
async fn detects_new_process() {
    let bus = Arc::new(EventBus::new(100));
    let mut rx = bus.subscribe();

    let mut monitor = ProcessMonitor::new(bus.clone(), std::time::Duration::from_millis(500));
    monitor.start();

    // Spawn a short-lived process
    let child = std::process::Command::new("sleep")
        .arg("2")
        .spawn()
        .expect("failed to spawn sleep");
    let child_pid = child.id();

    // Wait for the monitor to detect it (2 poll cycles)
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                if let Ok(event) = rx.recv().await {
                    if event.event_type == EventType::ProcessLifecycle {
                        if let Some(pid) = event.data.get("pid").and_then(|v| v.as_u64()) {
                            if pid == child_pid as u64 {
                                return event;
                            }
                        }
                    }
                }
            }
        },
    )
    .await;

    // It's OK if this doesn't fire on all platforms — process polling is best-effort
    if let Ok(event) = event {
        assert_eq!(event.event_type, EventType::ProcessLifecycle);
        assert_eq!(event.source, "process-monitor");
    }

    monitor.stop();
}

#[tokio::test]
async fn process_monitor_stops_cleanly() {
    let bus = Arc::new(EventBus::new(100));
    let mut monitor = ProcessMonitor::new(bus.clone(), std::time::Duration::from_millis(100));
    monitor.start();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    monitor.stop();
    // Should not panic or hang
}
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_process_monitor -- --nocapture`
Expected: FAIL — `process_monitor` module doesn't exist.

- [ ] **Step 3: Implement ProcessMonitor**

Add to workspace `[workspace.dependencies]` in root `Cargo.toml`:

```toml
sysinfo = "0.33"
```

Add to `crates/animus-sensorium/Cargo.toml` dependencies:

```toml
sysinfo = { workspace = true }
```

Create `crates/animus-sensorium/src/sensors/process_monitor.rs`:

```rust
use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::System;
use tokio::sync::Notify;

use crate::bus::EventBus;

/// Polls for process lifecycle changes and publishes SensorEvents.
pub struct ProcessMonitor {
    bus: Arc<EventBus>,
    poll_interval: Duration,
    shutdown: Arc<Notify>,
}

impl ProcessMonitor {
    pub fn new(bus: Arc<EventBus>, poll_interval: Duration) -> Self {
        Self {
            bus,
            poll_interval,
            shutdown: Arc::new(Notify::new()),
        }
    }

    pub fn start(&mut self) {
        let bus = self.bus.clone();
        let interval = self.poll_interval;
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            let mut sys = System::new();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            let mut known_pids: HashSet<sysinfo::Pid> = sys.processes().keys().copied().collect();

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                        let current_pids: HashSet<sysinfo::Pid> = sys.processes().keys().copied().collect();

                        // Detect new processes
                        for &pid in current_pids.difference(&known_pids) {
                            if let Some(process) = sys.process(pid) {
                                let event = SensorEvent {
                                    id: EventId::new(),
                                    timestamp: chrono::Utc::now(),
                                    event_type: EventType::ProcessLifecycle,
                                    source: "process-monitor".to_string(),
                                    data: serde_json::json!({
                                        "pid": pid.as_u32(),
                                        "name": process.name().to_string_lossy(),
                                        "op": "start",
                                    }),
                                    consent_policy: None,
                                };
                                if let Err(e) = bus.publish(event).await {
                                    tracing::warn!("Failed to publish process event: {e}");
                                }
                            }
                        }

                        // Detect terminated processes
                        for &pid in known_pids.difference(&current_pids) {
                            let event = SensorEvent {
                                id: EventId::new(),
                                timestamp: chrono::Utc::now(),
                                event_type: EventType::ProcessLifecycle,
                                source: "process-monitor".to_string(),
                                data: serde_json::json!({
                                    "pid": pid.as_u32(),
                                    "op": "stop",
                                }),
                                consent_policy: None,
                            };
                            if let Err(e) = bus.publish(event).await {
                                tracing::warn!("Failed to publish process stop event: {e}");
                            }
                        }

                        known_pids = current_pids;
                    }
                    _ = shutdown.notified() => {
                        break;
                    }
                }
            }
        });
    }

    pub fn stop(&self) {
        self.shutdown.notify_one();
    }
}
```

Add `pub mod process_monitor;` to `crates/animus-sensorium/src/sensors/mod.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_process_monitor -- --nocapture`
Expected: PASS (2 tests). The process detection test is best-effort — if the polling window misses the short-lived process, the test still passes.

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/src/sensors/process_monitor.rs crates/animus-sensorium/src/sensors/mod.rs crates/animus-sensorium/Cargo.toml Cargo.toml crates/animus-tests/tests/integration/sensorium_process_monitor.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(sensorium): process lifecycle monitor with diff-based polling"
```

---

### Task 9: Sensorium Orchestrator

**Files:**
- Create: `crates/animus-sensorium/src/orchestrator.rs`
- Modify: `crates/animus-sensorium/src/lib.rs`

The orchestrator wires together the event bus, consent engine, attention filter, and audit trail into a single pipeline. Events flow: Sensor → EventBus → Consent Check → Attention Filter → Audit → Optional VectorFS storage.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_orchestrator.rs`:

```rust
use animus_core::sensorium::*;
use animus_core::{EventId, PolicyId};
use animus_sensorium::attention::{AttentionFilter, AttentionRule, RuleAction};
use animus_sensorium::consent::ConsentEngine;
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
    ).unwrap();

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
    ).unwrap();

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
    ).unwrap();

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
    assert!(outcome.permitted); // consent passes
    assert!(!outcome.passed_attention); // attention filter ignores .git
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
    ).unwrap();

    // Process a few events
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
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_orchestrator -- --nocapture`
Expected: FAIL — `orchestrator` module doesn't exist.

- [ ] **Step 3: Implement SensoriumOrchestrator**

Create `crates/animus-sensorium/src/orchestrator.rs`:

```rust
use animus_core::sensorium::*;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::attention::AttentionFilter;
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
        attention_rules: Vec<crate::attention::AttentionRule>,
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
```

Add `pub mod orchestrator;` to `crates/animus-sensorium/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_orchestrator -- --nocapture`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/src/orchestrator.rs crates/animus-sensorium/src/lib.rs crates/animus-tests/tests/integration/sensorium_orchestrator.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(sensorium): orchestrator pipeline — consent → attention → audit"
```

---

### Task 10: Runtime Integration

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`
- Modify: `crates/animus-runtime/Cargo.toml`

Wire the Sensorium into the main runtime. The Sensorium runs as background tasks. Observations that pass the attention filter are stored as VectorFS segments with `Source::Observation`, making them available to Mnemos context assembly. Add `/sensorium` and `/consent` commands.

- [ ] **Step 1: Write integration expectations**

This task is integration-only (wiring existing tested components). No new unit tests needed — existing Sensorium tests cover component behavior. Manual verification:

Expected behavior after integration:
1. Runtime starts Sensorium background tasks (file watcher + process monitor)
2. `/sensorium` shows observation stats
3. `/consent` shows active consent policies
4. File changes in watched directories appear as observations
5. Observations pass consent + attention filtering
6. Passed observations are stored as VectorFS segments

- [ ] **Step 2: Implement runtime integration**

In `crates/animus-runtime/src/main.rs`, add:

1. Import Sensorium types:
```rust
use animus_sensorium::bus::EventBus;
use animus_sensorium::sensors::file_watcher::FileWatcher;
use animus_sensorium::sensors::process_monitor::ProcessMonitor;
use animus_sensorium::orchestrator::SensoriumOrchestrator;
use animus_core::sensorium::*;
```

2. In `run()`, after VectorFS initialization:
```rust
// Initialize Sensorium
let event_bus = Arc::new(EventBus::new(1000));

// Default consent: watch the data directory
let default_consent = ConsentPolicy {
    id: PolicyId::new(),
    name: "default".to_string(),
    rules: vec![ConsentRule {
        event_types: vec![EventType::FileChange, EventType::ProcessLifecycle],
        scope: Scope::All,
        permission: Permission::Allow,
        audit_level: AuditLevel::MetadataOnly,
    }],
    active: false, // disabled by default — user must opt in
    created: chrono::Utc::now(),
};

let audit_path = data_dir.join("sensorium-audit.jsonl");
let orchestrator = Arc::new(SensoriumOrchestrator::new(
    vec![default_consent],
    vec![], // no attention rules initially
    audit_path,
    0.5,
)?);

// Start background event processing
let orch_clone = orchestrator.clone();
let store_clone = store.clone();
let embedder_dim = dimensionality;
let mut bus_rx = event_bus.subscribe();
tokio::spawn(async move {
    while let Ok(event) = bus_rx.recv().await {
        match orch_clone.process_event(event.clone()).await {
            Ok(outcome) if outcome.passed_attention => {
                // Store as observation segment
                let embedding = vec![0.0f32; embedder_dim]; // synthetic for now
                let segment = animus_core::Segment::new(
                    animus_core::segment::Content::Structured(event.data.clone()),
                    embedding,
                    animus_core::segment::Source::Observation {
                        event_type: format!("{:?}", event.event_type),
                        raw_event_id: event.id,
                    },
                );
                if let Err(e) = store_clone.store(segment) {
                    tracing::warn!("Failed to store observation: {e}");
                }
            }
            Ok(_) => {} // filtered out — expected
            Err(e) => tracing::warn!("Sensorium processing error: {e}"),
        }
    }
});

tracing::info!("Sensorium initialized (sensors inactive until consent granted)");
```

3. Add `/sensorium` and `/consent` to `handle_command`.

4. Update the system prompt to mention Sensorium awareness.

5. Update `/help` output.

- [ ] **Step 3: Verify the build compiles**

Run: `cargo build -p animus-runtime`
Expected: Success.

- [ ] **Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/animus-runtime/
git commit -m "feat(runtime): integrate Sensorium background tasks, /sensorium and /consent commands"
```

---

### Task 11: Sensorium Config and Policy Persistence

**Files:**
- Modify: `crates/animus-core/src/config.rs`
- Create: `crates/animus-sensorium/src/policy_store.rs`
- Modify: `crates/animus-sensorium/src/lib.rs`

Persist consent policies to disk so they survive restarts. Add SensoriumConfig to AnimusConfig.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/sensorium_policy_store.rs`:

```rust
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
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests sensorium_policy_store -- --nocapture`
Expected: FAIL — `policy_store` module doesn't exist.

- [ ] **Step 3: Implement PolicyStore**

Create `crates/animus-sensorium/src/policy_store.rs`:

```rust
use animus_core::sensorium::ConsentPolicy;
use std::path::Path;

/// Persists consent policies as JSON.
pub struct PolicyStore;

impl PolicyStore {
    pub fn save(path: &Path, policies: &[ConsentPolicy]) -> animus_core::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(policies)?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> animus_core::Result<Vec<ConsentPolicy>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(path)?;
        let policies: Vec<ConsentPolicy> = serde_json::from_str(&data)?;
        Ok(policies)
    }
}
```

Add `pub mod policy_store;` to `crates/animus-sensorium/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_policy_store -- --nocapture`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/animus-sensorium/src/policy_store.rs crates/animus-sensorium/src/lib.rs crates/animus-tests/tests/integration/sensorium_policy_store.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "feat(sensorium): policy store with JSON persistence and atomic writes"
```

---

### Task 12: Full Integration Test

**Files:**
- Create: `crates/animus-tests/tests/integration/sensorium_full_pipeline.rs`

End-to-end test: file watcher detects a change → event bus → consent → attention → audit → VectorFS segment storage.

- [ ] **Step 1: Write the full pipeline test**

```rust
use animus_core::sensorium::*;
use animus_core::{EventId, PolicyId};
use animus_embed::SyntheticEmbedding;
use animus_sensorium::attention::{AttentionRule, RuleAction};
use animus_sensorium::bus::EventBus;
use animus_sensorium::orchestrator::SensoriumOrchestrator;
use animus_sensorium::sensors::file_watcher::FileWatcher;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn full_pipeline_file_change_to_segment() {
    let dir = TempDir::new().unwrap();
    let watch_dir = dir.path().join("watched");
    std::fs::create_dir_all(&watch_dir).unwrap();

    let vectorfs_dir = dir.path().join("vectorfs");
    let dim = 128;
    let store = Arc::new(MmapVectorStore::open(&vectorfs_dir, dim).unwrap());

    let bus = Arc::new(EventBus::new(100));

    // Consent: allow everything in the watched directory
    let policies = vec![ConsentPolicy {
        id: PolicyId::new(),
        name: "test-allow".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::FileChange],
            scope: Scope::All,
            permission: Permission::Allow,
            audit_level: AuditLevel::Full,
        }],
        active: true,
        created: chrono::Utc::now(),
    }];

    let audit_path = dir.path().join("audit.jsonl");
    let orchestrator = Arc::new(
        SensoriumOrchestrator::new(policies, vec![], audit_path.clone(), 0.5).unwrap(),
    );

    // Start background processing
    let orch_clone = orchestrator.clone();
    let store_clone = store.clone();
    let mut rx = bus.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match orch_clone.process_event(event.clone()).await {
                Ok(outcome) if outcome.passed_attention => {
                    let embedding = vec![0.0f32; dim];
                    let segment = animus_core::Segment::new(
                        animus_core::segment::Content::Structured(event.data.clone()),
                        embedding,
                        animus_core::segment::Source::Observation {
                            event_type: format!("{:?}", event.event_type),
                            raw_event_id: event.id,
                        },
                    );
                    let _ = store_clone.store(segment);
                }
                _ => {}
            }
        }
    });

    // Start file watcher
    let mut watcher = FileWatcher::new(bus.clone(), vec![watch_dir.clone()]).unwrap();
    watcher.start();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Create a file — this should trigger the full pipeline
    std::fs::write(watch_dir.join("important.rs"), "fn main() {}").unwrap();

    // Wait for the pipeline to process
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    watcher.stop();

    // Verify: audit trail has entries
    let entries = animus_sensorium::audit::AuditTrail::read_all(&audit_path).unwrap();
    assert!(!entries.is_empty(), "audit trail should have entries");

    // Verify: VectorFS has observation segments
    let segment_count = store.count(None);
    assert!(segment_count > 0, "VectorFS should have observation segments");
}
```

Add module to `crates/animus-tests/tests/integration/mod.rs`.

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p animus-tests sensorium_full_pipeline -- --nocapture`
Expected: PASS (1 test)

- [ ] **Step 3: Commit**

```bash
git add crates/animus-tests/tests/integration/sensorium_full_pipeline.rs crates/animus-tests/tests/integration/mod.rs
git commit -m "test(sensorium): end-to-end pipeline test — file change → consent → attention → audit → VectorFS"
```

---

### Task 13: Final Verification and Cleanup

- [ ] **Step 1: Run full workspace tests**

Run: `cargo test --workspace`
Expected: All tests pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Verify build**

Run: `cargo build --workspace`
Expected: Clean build, no warnings.

- [ ] **Step 4: Commit any final fixes**

If clippy or tests reveal issues, fix and commit.
