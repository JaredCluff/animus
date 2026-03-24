# Watcher Subsystem Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Watcher subsystem — pre-cognitive, compiled-Rust condition monitors that emit Signals only when conditions are met, with zero LLM cost on empty checks.

**Architecture:** A `Watcher` trait in `animus-cortex` exposes a synchronous `check()` that returns `Option<WatcherEvent>`. A `WatcherRegistry` (Arc-wrapped for shared ownership) loads configs from `watchers.json`, runs a single poll loop as a background tokio task, and promotes `WatcherEvent`s to `Signal`s on the existing mpsc channel. The `CommsWatcher` is the first concrete watcher. Both a `manage_watcher` LLM tool and `/watch list|enable|disable|set` slash commands call the same `WatcherRegistry::update_config()` path.

**Tech Stack:** Rust, tokio (async runtime), parking_lot (Mutex for shared state), serde_json (persistence), animus-core (Signal/ThreadId), tempfile (dev-tests)

---

## File Map

| Action | Path | Responsibility |
|--------|------|----------------|
| **Create** | `crates/animus-cortex/src/watcher.rs` | `Watcher` trait, `WatcherEvent`, `WatcherConfig`, `WatcherRegistry` |
| **Create** | `crates/animus-cortex/src/watchers/mod.rs` | Re-exports `CommsWatcher` |
| **Create** | `crates/animus-cortex/src/watchers/comms.rs` | `CommsWatcher` — scans comms dir for pending JSON |
| **Create** | `crates/animus-cortex/src/tools/manage_watcher.rs` | `ManageWatcherTool` — LLM tool for enable/disable/list/set_param |
| **Modify** | `crates/animus-cortex/src/tools/mod.rs` | Add `pub mod manage_watcher;` |
| **Modify** | `crates/animus-cortex/src/lib.rs` | Export `watcher`, `watchers`, `WatcherRegistry`, `ManageWatcherTool` |
| **Modify** | `crates/animus-runtime/src/main.rs` | Init registry, wire ToolContext/CommandContext, register tool, add slash commands, update system prompt |

---

## Task 1: Watcher trait and core types

**Files:**
- Create: `crates/animus-cortex/src/watcher.rs`

- [ ] **Step 1: Write the failing unit tests for WatcherConfig**

Add this to the bottom of `crates/animus-cortex/src/watcher.rs` (create file with just the test module first):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_config_default_is_disabled() {
        let cfg = WatcherConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.interval.is_none());
        assert!(cfg.last_checked.is_none());
        assert!(cfg.last_fired.is_none());
        // params defaults to null JSON value
        assert!(cfg.params.is_null());
    }

    #[test]
    fn watcher_config_roundtrips_serde() {
        let mut cfg = WatcherConfig::default();
        cfg.enabled = true;
        cfg.interval = Some(std::time::Duration::from_secs(60));
        cfg.params = serde_json::json!({ "dir": "/tmp/test" });

        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: WatcherConfig = serde_json::from_str(&json).unwrap();
        assert!(decoded.enabled);
        assert_eq!(decoded.interval, Some(std::time::Duration::from_secs(60)));
        assert_eq!(decoded.params["dir"], "/tmp/test");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (file doesn't exist yet)**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex watcher_config 2>&1 | head -20
```

Expected: compile error (file doesn't exist)

- [ ] **Step 3: Implement `WatcherConfig`, `WatcherEvent`, and the `Watcher` trait**

Create `crates/animus-cortex/src/watcher.rs`:

```rust
//! Watcher subsystem — pre-cognitive, compiled-Rust condition monitors.
//!
//! Watchers sit between Sensorium (raw observations) and Perception (LLM classification).
//! They fire a Signal only when a condition is met — zero LLM cost on empty checks.

use animus_core::{Signal, SignalPriority, SegmentId};
use animus_core::threading::ThreadId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

// ── Public types ─────────────────────────────────────────────────────────────

/// Lightweight output from a watcher check.
/// The registry promotes this to a full Signal (filling in thread IDs) before sending.
#[derive(Debug)]
pub struct WatcherEvent {
    pub priority: SignalPriority,
    pub summary: String,
    pub segment_refs: Vec<SegmentId>,
}

/// Per-watcher runtime configuration — persisted to `watchers.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    pub enabled: bool,
    /// Overrides the watcher's `default_interval()` when set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_duration_opt",
        deserialize_with = "deserialize_duration_opt"
    )]
    pub interval: Option<Duration>,
    /// Watcher-specific settings (e.g. `{"dir": "/home/animus/comms/from-claude"}`).
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired: Option<DateTime<Utc>>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: None,
            params: serde_json::Value::Null,
            last_checked: None,
            last_fired: None,
        }
    }
}

// Duration serde helpers (stores as integer seconds)
fn serialize_duration_opt<S>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match d {
        Some(dur) => s.serialize_some(&dur.as_secs()),
        None => s.serialize_none(),
    }
}

fn deserialize_duration_opt<'de, D>(d: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let secs: Option<u64> = Option::deserialize(d)?;
    Ok(secs.map(Duration::from_secs))
}

// ── Watcher trait ─────────────────────────────────────────────────────────────

/// A compiled-in background condition monitor.
///
/// `check()` is synchronous, pure Rust, no LLM, no async.
/// It should complete in milliseconds — filesystem scans, threshold reads, etc.
pub trait Watcher: Send + Sync {
    /// Stable identifier used as the persistence key. Never change after deployment.
    fn id(&self) -> &str;

    /// Human-readable name displayed in `/watch list`.
    fn name(&self) -> &str;

    /// Default poll interval if not overridden in config.
    fn default_interval(&self) -> Duration;

    /// The condition check. Returns `Some(event)` if the condition is met, `None` otherwise.
    /// Must be fast (milliseconds). No blocking I/O longer than ~100ms.
    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent>;
}
```

Then add the test module at the bottom of the same file (the tests from Step 1).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex watcher_config 2>&1
```

Expected: 2 tests pass

---

## Task 2: WatcherRegistry — state, persistence, update_config

**Files:**
- Modify: `crates/animus-cortex/src/watcher.rs` (continue adding to same file)

- [ ] **Step 1: Write failing tests for registry load/persist**

Add to the `tests` module in `watcher.rs`:

```rust
    #[test]
    fn registry_load_missing_json_proceeds_with_empty_configs() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");
        // File does not exist — must not panic, must return Ok with empty configs
        let state = RegistryState::load_or_default(&store_path);
        assert!(state.configs.is_empty());
    }

    #[test]
    fn registry_load_invalid_json_degrades_gracefully() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");
        std::fs::write(&store_path, b"{ not valid json!!!").unwrap();
        // Must not panic — returns empty configs with a warning logged
        let state = RegistryState::load_or_default(&store_path);
        assert!(state.configs.is_empty());
    }

    #[test]
    fn registry_update_config_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");

        // Manually create a RegistryState and save it
        let mut state = RegistryState { configs: HashMap::new() };
        let mut cfg = WatcherConfig::default();
        cfg.enabled = true;
        cfg.params = serde_json::json!({ "dir": "/tmp" });
        state.configs.insert("comms".to_string(), cfg);
        state.save(&store_path).unwrap();

        // Reload — config must survive the round-trip
        let reloaded = RegistryState::load_or_default(&store_path);
        let loaded_cfg = reloaded.configs.get("comms").unwrap();
        assert!(loaded_cfg.enabled);
        assert_eq!(loaded_cfg.params["dir"], "/tmp");
    }

    #[test]
    fn registry_unknown_watcher_ids_in_json_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");
        // JSON with an ID not in the registered watchers — must be silently ignored
        std::fs::write(&store_path, r#"{"obsolete_watcher":{"enabled":true,"params":null}}"#).unwrap();
        let state = RegistryState::load_or_default(&store_path);
        // Load succeeds — unknown ID is in configs but won't match any registered watcher
        // This is OK — they're just inert entries
        let _ = state; // no panic
    }
```

Also add `use tempfile;` import at top of test module.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex registry 2>&1 | head -20
```

Expected: compile errors (RegistryState not defined yet)

- [ ] **Step 3: Implement RegistryState, WatcherRegistry, and update_config**

Add this block to `watcher.rs` (after the `Watcher` trait, before the `#[cfg(test)]` module):

```rust
// ── Registry internal state (shared via Arc) ──────────────────────────────────

/// Mutable state shared between the poll loop and command/tool handlers.
/// Wrapped in `Arc<Mutex>` inside `WatcherRegistry`.
pub(crate) struct RegistryState {
    pub configs: HashMap<String, WatcherConfig>,
}

impl RegistryState {
    /// Load from `store_path`. If missing or invalid, log a warning and return empty state.
    /// Never returns an error — failure degrades to all-disabled defaults.
    pub fn load_or_default(store_path: &std::path::Path) -> Self {
        match std::fs::read_to_string(store_path) {
            Err(_) => {
                // File missing — normal on first run
                Self { configs: HashMap::new() }
            }
            Ok(raw) => match serde_json::from_str::<HashMap<String, WatcherConfig>>(&raw) {
                Ok(configs) => {
                    info!("Loaded watcher configs from {}", store_path.display());
                    Self { configs }
                }
                Err(e) => {
                    warn!(
                        "Could not parse watchers.json ({}): {}. Proceeding with empty configs.",
                        store_path.display(),
                        e
                    );
                    Self { configs: HashMap::new() }
                }
            },
        }
    }

    /// Atomically write configs to disk: write to `.tmp`, then rename.
    pub fn save(&self, store_path: &std::path::Path) -> std::io::Result<()> {
        let tmp_path = store_path.with_extension("tmp");
        let json = serde_json::to_string_pretty(&self.configs)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&tmp_path, json.as_bytes())?;
        std::fs::rename(&tmp_path, store_path)?;
        Ok(())
    }
}

// ── WatcherRegistry ───────────────────────────────────────────────────────────

/// Poll loop sleep when no watchers are enabled.
const IDLE_SLEEP: Duration = Duration::from_secs(5);

/// Manages all registered watchers and their lifecycle.
///
/// Cheaply cloneable — clone to share between the poll task and command/tool handlers.
#[derive(Clone)]
pub struct WatcherRegistry {
    watchers: Arc<Vec<Box<dyn Watcher>>>,
    state: Arc<parking_lot::Mutex<RegistryState>>,
    signal_tx: mpsc::Sender<Signal>,
    source_id: ThreadId,
    store_path: Arc<PathBuf>,
}

impl WatcherRegistry {
    /// Construct a new registry with the given watchers, load saved configs from
    /// `store_path`, and wire to the existing signal channel.
    pub fn new(
        watchers: Vec<Box<dyn Watcher>>,
        signal_tx: mpsc::Sender<Signal>,
        store_path: PathBuf,
    ) -> Self {
        let state = RegistryState::load_or_default(&store_path);
        Self {
            watchers: Arc::new(watchers),
            state: Arc::new(parking_lot::Mutex::new(state)),
            signal_tx,
            source_id: ThreadId::new(),
            store_path: Arc::new(store_path),
        }
    }

    /// Update (or insert) the config for a watcher. Persists immediately to disk.
    ///
    /// Returns an error string if the save fails (non-fatal — caller should log/display).
    pub fn update_config(&self, id: &str, config: WatcherConfig) -> Result<(), String> {
        let mut state = self.state.lock();
        state.configs.insert(id.to_string(), config);
        state
            .save(&self.store_path)
            .map_err(|e| format!("Failed to persist watcher config: {e}"))
    }

    /// Return a snapshot of (id, name, config) for all registered watchers.
    pub fn list(&self) -> Vec<(String, String, WatcherConfig)> {
        let state = self.state.lock();
        self.watchers
            .iter()
            .map(|w| {
                let cfg = state
                    .configs
                    .get(w.id())
                    .cloned()
                    .unwrap_or_default();
                (w.id().to_string(), w.name().to_string(), cfg)
            })
            .collect()
    }

    /// Get the current config for a watcher by id.
    pub fn get_config(&self, id: &str) -> Option<WatcherConfig> {
        let state = self.state.lock();
        // Return saved config or default (disabled) if none saved
        Some(state.configs.get(id).cloned().unwrap_or_default())
    }

    /// Returns true if a watcher with the given id is registered.
    pub fn has_watcher(&self, id: &str) -> bool {
        self.watchers.iter().any(|w| w.id() == id)
    }

    /// Spawn the poll loop as a background tokio task.
    /// Call once at startup.
    pub fn start(&self) {
        let registry = self.clone();
        tokio::spawn(async move {
            registry.poll_loop().await;
        });
    }

    async fn poll_loop(&self) {
        loop {
            let now = Utc::now();
            let mut next_wake_in = IDLE_SLEEP;

            // Snapshot the watcher list — watchers are immutable after construction
            let watcher_count = self.watchers.len();

            for i in 0..watcher_count {
                let watcher = &self.watchers[i];
                let id = watcher.id();

                // Take a lock snapshot per watcher, release before check() call.
                // The `continue` inside the lock is the only enabled guard needed.
                let (effective_interval, last_checked) = {
                    let state = self.state.lock();
                    let cfg = state.configs.get(id).cloned().unwrap_or_default();
                    if !cfg.enabled {
                        continue;
                    }
                    let interval = cfg.interval.unwrap_or_else(|| watcher.default_interval());
                    (interval, cfg.last_checked)
                };

                // Check if due
                let due_at = last_checked.map(|lc| {
                    lc + chrono::Duration::from_std(effective_interval).unwrap_or_default()
                });

                if let Some(due) = due_at {
                    if now < due {
                        let remaining = (due - now)
                            .to_std()
                            .unwrap_or(IDLE_SLEEP)
                            .min(effective_interval);
                        next_wake_in = next_wake_in.min(remaining);
                        continue;
                    }
                }

                // Run the check (synchronous — must be fast)
                let config_snapshot = {
                    let state = self.state.lock();
                    state.configs.get(id).cloned().unwrap_or_default()
                };

                let maybe_event = watcher.check(&config_snapshot);

                // Update last_checked (and last_fired if fired)
                {
                    let mut state = self.state.lock();
                    let cfg = state.configs.entry(id.to_string()).or_default();
                    cfg.last_checked = Some(Utc::now());
                    if maybe_event.is_some() {
                        cfg.last_fired = Some(Utc::now());
                    }
                }

                if let Some(event) = maybe_event {
                    let signal = Signal {
                        source_thread: self.source_id,
                        target_thread: ThreadId::default(),
                        priority: event.priority,
                        summary: event.summary,
                        segment_refs: event.segment_refs,
                        created: Utc::now(),
                    };
                    if let Err(e) = self.signal_tx.send(signal).await {
                        warn!("WatcherRegistry: failed to send signal: {e}");
                    }
                }

                next_wake_in = next_wake_in.min(effective_interval);
            }

            tokio::time::sleep(next_wake_in).await;
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex registry 2>&1
```

Expected: 4 registry tests pass

- [ ] **Step 5: Verify compilation**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo check -p animus-cortex 2>&1
```

Expected: No errors (warnings OK)

---

## Task 3: CommsWatcher

**Files:**
- Create: `crates/animus-cortex/src/watchers/mod.rs`
- Create: `crates/animus-cortex/src/watchers/comms.rs`

- [ ] **Step 1: Write failing tests for CommsWatcher**

Create `crates/animus-cortex/src/watchers/comms.rs` with just the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::WatcherConfig;
    use crate::Watcher;

    #[test]
    fn check_returns_none_when_no_dir_param() {
        let cfg = WatcherConfig::default(); // params is null
        assert!(CommsWatcher.check(&cfg).is_none());
    }

    #[test]
    fn check_returns_none_for_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });
        assert!(CommsWatcher.check(&cfg).is_none());
    }

    #[test]
    fn check_detects_pending_message_and_marks_read() {
        let tmp = tempfile::tempdir().unwrap();
        let msg_path = tmp.path().join("msg-001.json");
        std::fs::write(
            &msg_path,
            r#"{"id":"msg-001","from":"claude","subject":"Hello","content":"Hi there","status":"pending"}"#,
        )
        .unwrap();

        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });

        let event = CommsWatcher.check(&cfg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert!(event.summary.contains("Hello"));

        // File must be atomically marked "read"
        let raw = std::fs::read_to_string(&msg_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(json["status"], "read");
    }

    #[test]
    fn check_ignores_already_read_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let msg_path = tmp.path().join("msg-002.json");
        std::fs::write(
            &msg_path,
            r#"{"id":"msg-002","from":"claude","subject":"Old","content":"Already read","status":"read"}"#,
        )
        .unwrap();

        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });

        assert!(CommsWatcher.check(&cfg).is_none());
    }

    #[test]
    fn check_batches_multiple_pending_messages() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            std::fs::write(
                tmp.path().join(format!("msg-{i:03}.json")),
                format!(
                    r#"{{"id":"msg-{i:03}","from":"claude","subject":"Msg {i}","content":"Content {i}","status":"pending"}}"#
                ),
            )
            .unwrap();
        }

        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });

        let event = CommsWatcher.check(&cfg);
        assert!(event.is_some());
        let summary = event.unwrap().summary;
        // All 3 subjects appear in the batched summary
        assert!(summary.contains("Msg 1"));
        assert!(summary.contains("Msg 2"));
        assert!(summary.contains("Msg 3"));

        // All files marked read
        for i in 1..=3 {
            let raw = std::fs::read_to_string(tmp.path().join(format!("msg-{i:03}.json"))).unwrap();
            let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(json["status"], "read");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex comms 2>&1 | head -20
```

Expected: compile error (CommsWatcher not defined)

- [ ] **Step 3: Implement CommsWatcher**

Fill in `crates/animus-cortex/src/watchers/comms.rs`:

```rust
//! CommsWatcher — monitors the Claude Code → Animus comms directory.
//!
//! Scans for `*.json` files with `"status": "pending"`. For each found:
//! - Reads the content
//! - Atomically marks it `"status": "read"` (write to `.tmp`, rename)
//! - Batches subject + content into a single WatcherEvent summary

use crate::watcher::{Watcher, WatcherConfig, WatcherEvent};
use animus_core::SignalPriority;
use std::time::Duration;

pub struct CommsWatcher;

impl Watcher for CommsWatcher {
    fn id(&self) -> &str {
        "comms"
    }

    fn name(&self) -> &str {
        "Claude Code Comms"
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent> {
        let dir = config.params["dir"].as_str()?;
        let entries = std::fs::read_dir(dir).ok()?;

        let mut batch: Vec<(String, String)> = Vec::new(); // (subject, content)
        let mut has_alert = false;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let raw = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut msg: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if msg["status"].as_str() != Some("pending") {
                continue;
            }

            // Extract fields before mutating
            let subject = msg["subject"]
                .as_str()
                .unwrap_or("(no subject)")
                .to_string();
            let content = msg["content"]
                .as_str()
                .unwrap_or("")
                .to_string();

            if msg["type"].as_str() == Some("alert") {
                has_alert = true;
            }

            // Atomically mark as read: write to .tmp, rename
            msg["status"] = serde_json::Value::String("read".to_string());
            let updated = match serde_json::to_string_pretty(&msg) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let tmp_path = path.with_extension("tmp");
            if std::fs::write(&tmp_path, updated.as_bytes()).is_ok() {
                let _ = std::fs::rename(&tmp_path, &path);
            }

            batch.push((subject, content));
        }

        if batch.is_empty() {
            return None;
        }

        // Build a batched summary injected into the LLM prompt
        let mut summary = format!(
            "[CommsWatcher] {} message(s) from Claude Code:\n",
            batch.len()
        );
        for (i, (subject, content)) in batch.iter().enumerate() {
            summary.push_str(&format!("\n{}. **{}**\n{}\n", i + 1, subject, content));
        }

        Some(WatcherEvent {
            priority: if has_alert {
                SignalPriority::Urgent
            } else {
                SignalPriority::Normal
            },
            summary,
            segment_refs: vec![],
        })
    }
}

// Tests live below — copy the full test module from Step 1 verbatim here.
// The test module starts with `use super::*; use crate::watcher::WatcherConfig; ...`
#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::WatcherConfig;
    use crate::Watcher;
    // paste all five tests from Task 3, Step 1 here
}
```

Create `crates/animus-cortex/src/watchers/mod.rs`:

```rust
pub mod comms;
pub use comms::CommsWatcher;
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex comms 2>&1
```

Expected: 5 comms tests pass

---

## Task 4: manage_watcher tool

**Files:**
- Create: `crates/animus-cortex/src/tools/manage_watcher.rs`

The tool needs access to `WatcherRegistry` via `ToolContext`. We'll add `watcher_registry: Option<WatcherRegistry>` to `ToolContext` in this task.

- [ ] **Step 1: Write failing test for ManageWatcherTool**

Create `crates/animus-cortex/src/tools/manage_watcher.rs` with just the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolContext};
    use crate::watcher::{WatcherRegistry, WatcherConfig};
    use crate::watchers::CommsWatcher;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn make_test_registry(tmp_dir: &std::path::Path) -> WatcherRegistry {
        let (tx, _rx) = mpsc::channel(8);
        WatcherRegistry::new(
            vec![Box::new(CommsWatcher)],
            tx,
            tmp_dir.join("watchers.json"),
        )
    }

    fn make_ctx(registry: WatcherRegistry) -> ToolContext {
        ToolContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            store: Arc::new(animus_vectorfs::mmap::MmapVectorStore::open_temp().unwrap()),
            embedder: Arc::new(animus_embed::NoopEmbedder),
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: Some(registry),
        }
    }

    #[tokio::test]
    async fn list_action_returns_watcher_table() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        let ctx = make_ctx(registry);
        let result = ManageWatcherTool
            .execute(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("comms"));
        assert!(result.content.contains("Claude Code Comms"));
    }

    #[tokio::test]
    async fn enable_action_enables_watcher() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        let ctx = make_ctx(registry.clone());
        let result = ManageWatcherTool
            .execute(
                serde_json::json!({"action": "enable", "watcher_id": "comms"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let cfg = registry.get_config("comms").unwrap();
        assert!(cfg.enabled);
    }

    #[tokio::test]
    async fn disable_action_disables_watcher() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        // First enable it
        let mut cfg = WatcherConfig::default();
        cfg.enabled = true;
        registry.update_config("comms", cfg).unwrap();

        let ctx = make_ctx(registry.clone());
        let result = ManageWatcherTool
            .execute(
                serde_json::json!({"action": "disable", "watcher_id": "comms"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let cfg = registry.get_config("comms").unwrap();
        assert!(!cfg.enabled);
    }

    #[tokio::test]
    async fn unknown_watcher_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        let ctx = make_ctx(registry);
        let result = ManageWatcherTool
            .execute(
                serde_json::json!({"action": "enable", "watcher_id": "nonexistent"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("nonexistent"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex manage_watcher 2>&1 | head -20
```

Expected: compile errors

- [ ] **Step 3: Add `watcher_registry` field to `ToolContext`**

Edit `crates/animus-cortex/src/tools/mod.rs` — add to `ToolContext`:

```rust
use crate::watcher::WatcherRegistry;

pub struct ToolContext {
    // ... existing fields ...
    /// Watcher registry for the manage_watcher tool. None if watchers are not configured.
    pub watcher_registry: Option<WatcherRegistry>,
}
```

- [ ] **Step 4: Implement ManageWatcherTool**

Fill in `crates/animus-cortex/src/tools/manage_watcher.rs`:

```rust
//! manage_watcher tool — lets the LLM enable, disable, configure, and list background watchers.

use crate::telos::Autonomy;
use crate::watcher::WatcherConfig;
use super::{Tool, ToolContext, ToolResult};
use std::time::Duration;

pub struct ManageWatcherTool;

#[async_trait::async_trait]
impl Tool for ManageWatcherTool {
    fn name(&self) -> &str {
        "manage_watcher"
    }

    fn description(&self) -> &str {
        "Enable, disable, or configure a background watcher. Watchers monitor conditions \
         without LLM involvement and signal you when something requires attention. \
         Use action=list to see all watchers and their current state."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["enable", "disable", "list", "set_param"],
                    "description": "Operation to perform"
                },
                "watcher_id": {
                    "type": "string",
                    "description": "Required for enable, disable, set_param. E.g. \"comms\""
                },
                "interval_secs": {
                    "type": "integer",
                    "description": "Optional poll interval override in seconds (for enable)"
                },
                "params": {
                    "type": "object",
                    "description": "Key-value pairs to merge into watcher params (for set_param)"
                }
            },
            "required": ["action"]
        })
    }

    fn required_autonomy(&self) -> Autonomy {
        Autonomy::Suggest
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        let registry = match &ctx.watcher_registry {
            Some(r) => r,
            None => {
                return Ok(ToolResult {
                    content: "Watcher registry not available".to_string(),
                    is_error: true,
                })
            }
        };

        let action = params["action"].as_str().unwrap_or("");

        match action {
            "list" => {
                let entries = registry.list();
                if entries.is_empty() {
                    return Ok(ToolResult {
                        content: "No watchers registered.".to_string(),
                        is_error: false,
                    });
                }
                let mut out = String::from("Registered watchers:\n");
                for (id, name, cfg) in &entries {
                    let state = if cfg.enabled { "enabled" } else { "disabled" };
                    let interval = cfg
                        .interval
                        .map(|d| format!("{}s", d.as_secs()))
                        .unwrap_or_else(|| "default".to_string());
                    let last_fired = cfg
                        .last_fired
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| "never".to_string());
                    out.push_str(&format!(
                        "  {id} — {name} [{state}] interval={interval} last_fired={last_fired}\n"
                    ));
                }
                Ok(ToolResult { content: out, is_error: false })
            }

            "enable" => {
                let id = params["watcher_id"].as_str().ok_or("missing watcher_id")?;
                if !registry.has_watcher(id) {
                    return Ok(ToolResult {
                        content: format!("Unknown watcher: {id}"),
                        is_error: true,
                    });
                }
                let mut cfg = registry.get_config(id).unwrap_or_default();
                cfg.enabled = true;
                if let Some(secs) = params["interval_secs"].as_u64() {
                    cfg.interval = Some(Duration::from_secs(secs));
                }
                match registry.update_config(id, cfg) {
                    Ok(()) => Ok(ToolResult {
                        content: format!("Watcher '{id}' enabled."),
                        is_error: false,
                    }),
                    Err(e) => Ok(ToolResult { content: e, is_error: true }),
                }
            }

            "disable" => {
                let id = params["watcher_id"].as_str().ok_or("missing watcher_id")?;
                if !registry.has_watcher(id) {
                    return Ok(ToolResult {
                        content: format!("Unknown watcher: {id}"),
                        is_error: true,
                    });
                }
                let mut cfg = registry.get_config(id).unwrap_or_default();
                cfg.enabled = false;
                match registry.update_config(id, cfg) {
                    Ok(()) => Ok(ToolResult {
                        content: format!("Watcher '{id}' disabled."),
                        is_error: false,
                    }),
                    Err(e) => Ok(ToolResult { content: e, is_error: true }),
                }
            }

            "set_param" => {
                let id = params["watcher_id"].as_str().ok_or("missing watcher_id")?;
                if !registry.has_watcher(id) {
                    return Ok(ToolResult {
                        content: format!("Unknown watcher: {id}"),
                        is_error: true,
                    });
                }
                let new_params = params["params"]
                    .as_object()
                    .ok_or("params must be an object")?;
                let mut cfg = registry.get_config(id).unwrap_or_default();
                // Merge: existing params is an object or null
                let mut existing = match cfg.params.take() {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                for (k, v) in new_params {
                    existing.insert(k.clone(), v.clone());
                }
                cfg.params = serde_json::Value::Object(existing);
                match registry.update_config(id, cfg) {
                    Ok(()) => Ok(ToolResult {
                        content: format!("Watcher '{id}' params updated."),
                        is_error: false,
                    }),
                    Err(e) => Ok(ToolResult { content: e, is_error: true }),
                }
            }

            other => Ok(ToolResult {
                content: format!("Unknown action: {other}. Valid: list, enable, disable, set_param"),
                is_error: true,
            }),
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex manage_watcher 2>&1
```

Expected: 4 manage_watcher tests pass

---

## Task 5: lib.rs exports and tools/mod.rs registration

**Files:**
- Modify: `crates/animus-cortex/src/lib.rs`
- Modify: `crates/animus-cortex/src/tools/mod.rs`

- [ ] **Step 1: Add modules and re-exports to lib.rs**

Edit `crates/animus-cortex/src/lib.rs` — add after existing module declarations:

```rust
pub mod watcher;
pub mod watchers;
```

Add to the `pub use` section:

```rust
pub use watcher::{Watcher, WatcherConfig, WatcherEvent, WatcherRegistry};
pub use watchers::CommsWatcher;
```

- [ ] **Step 2: Add manage_watcher to tools/mod.rs**

Edit `crates/animus-cortex/src/tools/mod.rs` — add to the existing module list:

```rust
pub mod manage_watcher;
```

- [ ] **Step 3: Verify animus-cortex compiles cleanly**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo check -p animus-cortex 2>&1
```

Expected: No errors

- [ ] **Step 4: Run the full animus-cortex test suite**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex 2>&1
```

Expected: All tests pass (including pre-existing perception/reflection tests)

---

## Task 6: main.rs wiring

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

This task is the largest single file edit. Make changes in this order to avoid breaking the intermediate builds.

**Context notes:**
- Signal channel is at line ~225: `let (signal_tx, mut signal_rx) = ...`
- ToolContext is constructed at line ~252
- Tool registry is built at lines ~228-243
- Perception loop starts at lines ~387-412
- Reflection loop starts at lines ~427-453
- CommandContext struct is at lines ~1070-1085
- CommandContext instantiation is at lines ~563-578
- handle_command starts at line 1087
- Existing `/watch <path>` command is at line 1532
- System prompt tool list is at lines 42-52

> **Naming note:** There is an existing `/watch <path>` command that starts the Sensorium FileWatcher. The new Watcher subsystem uses `/watch list|enable|disable|set`. These are disambiguated by checking whether the first word of `arg` is a subcommand keyword. A path that starts with `list`, `enable`, `disable`, or `set` (e.g., `/watch list-files`) would incorrectly match — this is an acceptable limitation documented in the command output.

- [ ] **Step 1: Initialize WatcherRegistry after signal channel creation**

Find the signal channel creation (around line 225). Add immediately after:

```rust
// ── Watcher Registry ──────────────────────────────────────────────────────────
let watcher_registry = animus_cortex::WatcherRegistry::new(
    vec![
        Box::new(animus_cortex::CommsWatcher),
    ],
    signal_tx.clone(),
    data_dir.join("watchers.json"),
);
watcher_registry.start();
tracing::info!("Watcher registry started ({} watchers registered)", 1);
```

- [ ] **Step 2: Add `watcher_registry` to ToolContext**

Find the `ToolContext { ... }` construction block (~line 252). Add the new field:

```rust
let tool_ctx = ToolContext {
    data_dir: data_dir.clone(),
    store: store.clone() as std::sync::Arc<dyn animus_vectorfs::VectorStore>,
    embedder: embedder.clone(),
    signal_tx: Some(signal_tx.clone()),
    autonomy_tx: Some(autonomy_tx),
    active_telegram_chat_id: active_telegram_chat_id.clone(),
    watcher_registry: Some(watcher_registry.clone()),  // ← add this
};
```

- [ ] **Step 3: Register ManageWatcherTool**

In the tool registry block (~lines 228-243), add:

```rust
reg.register(Box::new(animus_cortex::tools::manage_watcher::ManageWatcherTool));
```

- [ ] **Step 4: Update system prompt to list manage_watcher**

Find the system prompt tool list (around lines 42-52). Add after `telegram_send`:

```
- `manage_watcher(action, watcher_id?, interval_secs?, params?)` — Enable, disable, or configure background watchers. Use action=list to see all.
```

Also update the `/commands` line (line ~65):

```
/goals /remember /forget /status /threads /thread /sleep /wake /watch /quit
```

- [ ] **Step 5: Add `watcher_registry` to CommandContext struct**

Find `struct CommandContext<'a>` (~line 1070). Add a new field:

```rust
watcher_registry: &'a animus_cortex::WatcherRegistry,
```

- [ ] **Step 6: Pass `watcher_registry` when constructing CommandContext**

Find the CommandContext construction (~lines 563-578). Add:

```rust
let mut ctx = CommandContext {
    // ... existing fields ...
    watcher_registry: &watcher_registry,
};
```

- [ ] **Step 7: Add `/watch list|enable|disable|set` slash commands**

Find the existing `/watch` command handler (~line 1532):

```rust
"/watch" if !arg.is_empty() => {
```

**Before** that branch, add the new watcher subcommands:

```rust
"/watch" if matches!(arg.split_whitespace().next(), Some("list" | "enable" | "disable" | "set")) => {
    let parts: Vec<&str> = arg.splitn(3, ' ').collect();
    let sub = parts[0];
    match sub {
        "list" => {
            let entries = ctx.watcher_registry.list();
            if entries.is_empty() {
                ctx.interface.display_status("No watchers registered.");
            } else {
                ctx.interface.display_status("Registered watchers:");
                for (id, name, cfg) in &entries {
                    let state = if cfg.enabled { "enabled" } else { "disabled" };
                    let interval = cfg
                        .interval
                        .map(|d| format!("{}s", d.as_secs()))
                        .unwrap_or_else(|| "default".to_string());
                    let last_fired = cfg
                        .last_fired
                        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                        .unwrap_or_else(|| "never".to_string());
                    ctx.interface.display(&format!(
                        "  {id} — {name} [{state}] interval={interval} last_fired={last_fired}"
                    ));
                }
            }
        }

        "enable" => {
            let watcher_id = match parts.get(1) {
                Some(id) => *id,
                None => {
                    ctx.interface.display_status("Usage: /watch enable <id> [interval=<N>s]");
                    return Ok(CommandResult::Continue);
                }
            };
            if !ctx.watcher_registry.has_watcher(watcher_id) {
                ctx.interface
                    .display_status(&format!("Unknown watcher: {watcher_id}"));
                return Ok(CommandResult::Continue);
            }
            let mut cfg = ctx.watcher_registry.get_config(watcher_id).unwrap_or_default();
            cfg.enabled = true;
            // Parse optional interval=<N>s
            if let Some(opts) = parts.get(2) {
                for kv in opts.split_whitespace() {
                    if let Some(val) = kv.strip_prefix("interval=") {
                        let secs_str = val.trim_end_matches('s');
                        if let Ok(secs) = secs_str.parse::<u64>() {
                            cfg.interval = Some(std::time::Duration::from_secs(secs));
                        }
                    }
                }
            }
            match ctx.watcher_registry.update_config(watcher_id, cfg) {
                Ok(()) => ctx.interface.display_status(&format!("Watcher '{watcher_id}' enabled.")),
                Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
            }
        }

        "disable" => {
            let watcher_id = match parts.get(1) {
                Some(id) => *id,
                None => {
                    ctx.interface.display_status("Usage: /watch disable <id>");
                    return Ok(CommandResult::Continue);
                }
            };
            if !ctx.watcher_registry.has_watcher(watcher_id) {
                ctx.interface
                    .display_status(&format!("Unknown watcher: {watcher_id}"));
                return Ok(CommandResult::Continue);
            }
            let mut cfg = ctx.watcher_registry.get_config(watcher_id).unwrap_or_default();
            cfg.enabled = false;
            match ctx.watcher_registry.update_config(watcher_id, cfg) {
                Ok(()) => ctx.interface.display_status(&format!("Watcher '{watcher_id}' disabled.")),
                Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
            }
        }

        "set" => {
            // /watch set <id> <key>=<value>
            let watcher_id = match parts.get(1) {
                Some(id) => *id,
                None => {
                    ctx.interface.display_status("Usage: /watch set <id> <key>=<value>");
                    return Ok(CommandResult::Continue);
                }
            };
            if !ctx.watcher_registry.has_watcher(watcher_id) {
                ctx.interface
                    .display_status(&format!("Unknown watcher: {watcher_id}"));
                return Ok(CommandResult::Continue);
            }
            let kv_str = match parts.get(2) {
                Some(s) => *s,
                None => {
                    ctx.interface.display_status("Usage: /watch set <id> <key>=<value>");
                    return Ok(CommandResult::Continue);
                }
            };
            let (key, value) = match kv_str.split_once('=') {
                Some(pair) => pair,
                None => {
                    ctx.interface.display_status("Usage: /watch set <id> <key>=<value>");
                    return Ok(CommandResult::Continue);
                }
            };
            let mut cfg = ctx.watcher_registry.get_config(watcher_id).unwrap_or_default();
            let mut existing = match cfg.params.take() {
                serde_json::Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            existing.insert(key.to_string(), serde_json::Value::String(value.to_string()));
            cfg.params = serde_json::Value::Object(existing);
            match ctx.watcher_registry.update_config(watcher_id, cfg) {
                Ok(()) => ctx.interface.display_status(&format!(
                    "Watcher '{watcher_id}' param '{key}' set to '{value}'."
                )),
                Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
            }
        }

        _ => unreachable!(),
    }
}
```

- [ ] **Step 8: Verify main.rs compiles**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo check --bin animus 2>&1
```

Expected: No errors. Fix any borrow/lifetime issues in CommandContext before proceeding.

- [ ] **Step 9: Full release build**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo build --release --bin animus 2>&1
```

Expected: Successful build with no errors.

---

## Task 7: Smoke test

- [ ] **Step 1: Run full test suite**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test 2>&1
```

Expected: All tests pass

- [ ] **Step 2: Verify success criteria from spec**

Check each criterion manually:

| # | Criterion | How to verify |
|---|-----------|---------------|
| 1 | Watcher finds nothing → zero LLM tokens | `CommsWatcher.check()` with empty dir returns `None` ✓ (unit test) |
| 2 | Watcher fires → Signal indistinguishable from Perception/Reflection | WatcherEvent promotes to Signal with same structure ✓ (code review) |
| 3 | `/watch enable comms` survives restart | `watchers.json` written atomically; `load_or_default` reloads it ✓ (unit test) |
| 4 | Animus can toggle via `manage_watcher` | ManageWatcherTool enable/disable tested ✓ (unit tests) |
| 5 | CommsWatcher detects pending, marks read | Unit tests in `comms.rs` ✓ |
| 6 | Corrupt `watchers.json` degrades gracefully | Unit test in `watcher.rs` ✓ |

- [ ] **Step 3: Commit all new files and changes**

> ⚠️ Check work-hours-guard before committing: run `TZ=America/Denver date '+%A %H:%M'` and verify it is outside Mon-Fri 8am-5pm MT.

```bash
cd /Users/jared.cluff/gitrepos/animus
git add \
  crates/animus-cortex/src/watcher.rs \
  crates/animus-cortex/src/watchers/mod.rs \
  crates/animus-cortex/src/watchers/comms.rs \
  crates/animus-cortex/src/tools/manage_watcher.rs \
  crates/animus-cortex/src/tools/mod.rs \
  crates/animus-cortex/src/lib.rs \
  crates/animus-runtime/src/main.rs \
  docs/superpowers/plans/2026-03-23-watcher-subsystem.md \
  docs/superpowers/specs/2026-03-23-watcher-subsystem-design.md
```

```bash
git commit -m "feat(cortex): add Watcher subsystem with CommsWatcher and manage_watcher tool"
```

---

## Appendix: Key Interfaces

**Watcher trait (add new watchers here):**

```rust
pub trait Watcher: Send + Sync {
    fn id(&self) -> &str;               // stable persistence key
    fn name(&self) -> &str;             // human-readable
    fn default_interval(&self) -> Duration;
    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent>;
}
```

**To add a new watcher:**
1. Create `crates/animus-cortex/src/watchers/<name>.rs`
2. Implement `Watcher` trait
3. Export from `watchers/mod.rs`
4. Register in `main.rs` WatcherRegistry constructor

**CommsWatcher default config:**

```json
{
  "enabled": true,
  "params": { "dir": "/home/animus/comms/from-claude" }
}
```

Enable via: `/watch enable comms` then `/watch set comms dir=/home/animus/comms/from-claude`
