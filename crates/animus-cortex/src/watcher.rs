//! Watcher subsystem — polling-based sensors that emit `Signal`-ready events.
//!
//! A `Watcher` periodically checks some external condition (filesystem path,
//! network endpoint, system metric, …) and, when triggered, returns a
//! `WatcherEvent` that the registry converts into an `animus_core::Signal`.

use animus_core::{Signal, SignalPriority, SegmentId};
use animus_core::identity::ThreadId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// WatcherEvent
// ---------------------------------------------------------------------------

/// A trigger emitted by a `Watcher::check` call.
///
/// The registry converts this into a full `animus_core::Signal` by adding
/// routing and timestamp information.
#[derive(Debug)]
pub struct WatcherEvent {
    pub priority: SignalPriority,
    pub summary: String,
    pub segment_refs: Vec<SegmentId>,
}

// ---------------------------------------------------------------------------
// WatcherConfig
// ---------------------------------------------------------------------------

/// Per-watcher runtime configuration stored in Mnemos (or passed inline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    /// Whether this watcher is actively polling.
    pub enabled: bool,

    /// Override the watcher's `default_interval`.
    /// Serialized as whole seconds — sub-second precision is not preserved.
    /// For watcher intervals (30s, 60s, etc.) this is sufficient.
    #[serde(
        serialize_with = "serialize_duration",
        deserialize_with = "deserialize_duration"
    )]
    pub interval: Option<Duration>,

    /// Watcher-specific parameters (arbitrary JSON object or null).
    #[serde(default = "serde_json::Value::default")]
    pub params: serde_json::Value,

    /// When this watcher last ran a check.
    pub last_checked: Option<DateTime<Utc>>,

    /// When this watcher last fired an event.
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

// ---------------------------------------------------------------------------
// Duration serde helpers
// ---------------------------------------------------------------------------

fn serialize_duration<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(d) => serializer.serialize_some(&d.as_secs()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<u64> = Option::deserialize(deserializer)?;
    Ok(opt.map(Duration::from_secs))
}

// ---------------------------------------------------------------------------
// Watcher trait
// ---------------------------------------------------------------------------

/// A pluggable sensor that polls some condition and optionally fires an event.
///
/// Implementations must be `Send + Sync` so they can be held behind an `Arc`
/// inside the `WatcherRegistry`.
pub trait Watcher: Send + Sync {
    /// Stable identifier (e.g. `"fs_watcher"`, `"comms_watcher"`).
    fn id(&self) -> &str;

    /// Human-readable display name.
    fn name(&self) -> &str;

    /// Suggested polling cadence; may be overridden by `WatcherConfig::interval`.
    fn default_interval(&self) -> Duration;

    /// Inspect the watched resource and return an event if the trigger condition
    /// is met, or `None` if everything is quiet.
    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent>;
}

// ── Registry internal state ────────────────────────────────────────────────────

pub(crate) struct RegistryState {
    pub configs: HashMap<String, WatcherConfig>,
}

impl RegistryState {
    pub fn load_or_default(store_path: &std::path::Path) -> Self {
        match std::fs::read_to_string(store_path) {
            Err(_) => Self { configs: HashMap::new() },
            Ok(raw) => match serde_json::from_str::<HashMap<String, WatcherConfig>>(&raw) {
                Ok(configs) => {
                    info!("Loaded watcher configs from {}", store_path.display());
                    Self { configs }
                }
                Err(e) => {
                    warn!(
                        "Could not parse watchers.json ({}): {}. Proceeding with empty configs.",
                        store_path.display(), e
                    );
                    Self { configs: HashMap::new() }
                }
            },
        }
    }

    pub fn save(&self, store_path: &std::path::Path) -> std::io::Result<()> {
        let tmp_path = store_path.with_extension("tmp");
        let json = serde_json::to_string_pretty(&self.configs)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&tmp_path, json.as_bytes())?;
        std::fs::rename(&tmp_path, store_path)?;
        Ok(())
    }
}

// ── WatcherRegistry ────────────────────────────────────────────────────────────

const IDLE_SLEEP: Duration = Duration::from_secs(5);

/// Manages all registered watchers and their lifecycle.
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

    pub fn update_config(&self, id: &str, config: WatcherConfig) -> Result<(), String> {
        let mut state = self.state.lock();
        state.configs.insert(id.to_string(), config);
        state.save(&self.store_path)
            .map_err(|e| format!("Failed to persist watcher config: {e}"))
    }

    pub fn list(&self) -> Vec<(String, String, WatcherConfig)> {
        let state = self.state.lock();
        self.watchers.iter().map(|w| {
            let cfg = state.configs.get(w.id()).cloned().unwrap_or_default();
            (w.id().to_string(), w.name().to_string(), cfg)
        }).collect()
    }

    /// Returns the saved config for the given watcher id, or a default (disabled) config if none saved.
    pub fn get_config(&self, id: &str) -> WatcherConfig {
        let state = self.state.lock();
        state.configs.get(id).cloned().unwrap_or_default()
    }

    pub fn has_watcher(&self, id: &str) -> bool {
        self.watchers.iter().any(|w| w.id() == id)
    }

    pub fn start(&self) {
        let registry = self.clone();
        tokio::spawn(async move {
            registry.poll_loop().await;
        });
    }

    async fn poll_loop(&self) {
        loop {
            let now = chrono::Utc::now();
            let mut next_wake_in = IDLE_SLEEP;

            let watcher_count = self.watchers.len();
            for i in 0..watcher_count {
                let watcher = &self.watchers[i];
                let id = watcher.id();

                let (effective_interval, last_checked) = {
                    let state = self.state.lock();
                    let cfg = state.configs.get(id).cloned().unwrap_or_default();
                    if !cfg.enabled { continue; }
                    let interval = cfg.interval.unwrap_or_else(|| watcher.default_interval());
                    (interval, cfg.last_checked)
                };

                // When last_checked is None (first run or after restart, since last_checked is not
                // persisted to disk), due_at is None and the watcher fires immediately on the
                // first poll cycle. This is intentional: idempotent watchers like CommsWatcher
                // are safe to call on boot, and the behavior means "check as soon as you start."
                // Future watchers with non-idempotent check logic should account for this.
                let due_at = last_checked.map(|lc| {
                    lc + chrono::Duration::from_std(effective_interval).unwrap_or_default()
                });

                if let Some(due) = due_at {
                    if now < due {
                        let remaining = (due - now).to_std().unwrap_or(IDLE_SLEEP).min(effective_interval);
                        next_wake_in = next_wake_in.min(remaining);
                        continue;
                    }
                }

                let config_snapshot = {
                    let state = self.state.lock();
                    state.configs.get(id).cloned().unwrap_or_default()
                };

                let maybe_event = watcher.check(&config_snapshot);

                {
                    let mut state = self.state.lock();
                    let cfg = state.configs.entry(id.to_string()).or_default();
                    cfg.last_checked = Some(chrono::Utc::now());
                    if maybe_event.is_some() {
                        cfg.last_fired = Some(chrono::Utc::now());
                    }
                }

                if let Some(event) = maybe_event {
                    let signal = Signal {
                        source_thread: self.source_id,
                        target_thread: ThreadId::default(),
                        priority: event.priority,
                        summary: event.summary,
                        segment_refs: event.segment_refs,
                        created: chrono::Utc::now(),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn watcher_config_default_is_disabled() {
        let cfg = WatcherConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.interval.is_none());
        assert!(cfg.last_checked.is_none());
        assert!(cfg.last_fired.is_none());
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

    // Task 2 tests — need tempfile crate
    #[test]
    fn registry_load_missing_json_proceeds_with_empty_configs() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");
        let state = RegistryState::load_or_default(&store_path);
        assert!(state.configs.is_empty());
    }

    #[test]
    fn registry_load_invalid_json_degrades_gracefully() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");
        std::fs::write(&store_path, b"{ not valid json!!!").unwrap();
        let state = RegistryState::load_or_default(&store_path);
        assert!(state.configs.is_empty());
    }

    #[test]
    fn registry_update_config_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");
        let mut state = RegistryState { configs: HashMap::new() };
        let mut cfg = WatcherConfig::default();
        cfg.enabled = true;
        cfg.params = serde_json::json!({ "dir": "/tmp" });
        state.configs.insert("comms".to_string(), cfg);
        state.save(&store_path).unwrap();
        let reloaded = RegistryState::load_or_default(&store_path);
        let loaded_cfg = reloaded.configs.get("comms").unwrap();
        assert!(loaded_cfg.enabled);
        assert_eq!(loaded_cfg.params["dir"], "/tmp");
    }

    #[test]
    fn registry_unknown_watcher_ids_in_json_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("watchers.json");
        std::fs::write(&store_path, r#"{"obsolete_watcher":{"enabled":true,"params":null}}"#).unwrap();
        let state = RegistryState::load_or_default(&store_path);
        let _ = state; // must not panic
    }
}
