// crates/animus-cortex/src/watchers/providers.rs
//! Polls providers.json for new entries and fires a WatcherEvent on change.
//! The runtime's main loop handles the event by hot-adding new engines.
//!
//! Uses mtime comparison to avoid re-parsing unchanged files. Interior
//! mutability (parking_lot::Mutex) is required because the Watcher trait
//! signature uses `&self`.

use crate::watcher::{Watcher, WatcherConfig, WatcherEvent};
use animus_core::load_providers_json;
use animus_core::threading::SignalPriority;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

struct Inner {
    last_mtime: Option<SystemTime>,
    last_known_ids: HashSet<String>,
}

pub struct ProvidersJsonWatcher {
    providers_path: PathBuf,
    inner: parking_lot::Mutex<Inner>,
}

impl ProvidersJsonWatcher {
    pub fn new(providers_path: PathBuf) -> Self {
        Self {
            providers_path,
            inner: parking_lot::Mutex::new(Inner {
                last_mtime: None,
                last_known_ids: HashSet::new(),
            }),
        }
    }
}

impl Watcher for ProvidersJsonWatcher {
    fn id(&self) -> &str {
        "providers_json"
    }

    fn name(&self) -> &str {
        "Providers JSON Watcher"
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn check(&self, _config: &WatcherConfig) -> Option<WatcherEvent> {
        let mtime = std::fs::metadata(&self.providers_path)
            .and_then(|m| m.modified())
            .ok()?;

        let mut inner = self.inner.lock();

        if inner.last_mtime == Some(mtime) {
            return None; // no change
        }
        inner.last_mtime = Some(mtime);

        let entries = load_providers_json(&self.providers_path);
        let current_ids: HashSet<String> =
            entries.iter().map(|e| e.provider_id.clone()).collect();

        let new_ids: Vec<String> = current_ids
            .difference(&inner.last_known_ids)
            .cloned()
            .collect();

        inner.last_known_ids = current_ids;

        if new_ids.is_empty() {
            return None;
        }

        Some(WatcherEvent {
            priority: SignalPriority::Urgent,
            summary: format!(
                "providers.json: new provider(s) detected: {}",
                new_ids.join(", ")
            ),
            segment_refs: vec![],
        })
    }
}
