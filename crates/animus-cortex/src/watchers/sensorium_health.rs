//! Sensorium health watcher — monitors the observation pipeline for anomalies.
//!
//! Reads the sensorium audit trail and fires a signal when:
//! 1. The promotion rate for recent events is suspiciously low (attention filter
//!    may be too aggressive, or events are being denied by consent policies).
//! 2. No events have been observed at all (sensors may have stopped).
//!
//! # Config params
//! - `min_events`           (integer, default 30)   — minimum event count before rate check applies.
//! - `low_rate_threshold`   (float,   default 0.05) — promotion rate below which to fire.
//! - `recent_n`             (integer, default 200)  — how many recent audit entries to inspect.

use crate::watcher::{Watcher, WatcherConfig, WatcherEvent};
use animus_core::sensorium::{AuditAction, AuditEntry};
use animus_core::threading::SignalPriority;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_MIN_EVENTS: u64 = 30;
const DEFAULT_LOW_RATE: f64 = 0.05;
const DEFAULT_RECENT_N: u64 = 200;
const DEFAULT_INTERVAL_SECS: u64 = 3600; // 60 minutes

pub struct SensoriumHealthWatcher {
    audit_path: PathBuf,
}

impl SensoriumHealthWatcher {
    pub fn new(audit_path: PathBuf) -> Self {
        Self { audit_path }
    }

    /// Read up to `limit` recent audit entries. Returns empty vec if the file
    /// doesn't exist or can't be read.
    fn read_recent(&self, limit: usize) -> Vec<AuditEntry> {
        match animus_sensorium::audit::AuditTrail::read_recent(&self.audit_path, limit) {
            Ok(entries) => entries,
            Err(_) => Vec::new(),
        }
    }
}

impl Watcher for SensoriumHealthWatcher {
    fn id(&self) -> &str {
        "sensorium_health"
    }

    fn name(&self) -> &str {
        "Sensorium Health"
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(DEFAULT_INTERVAL_SECS)
    }

    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent> {
        let min_events = config
            .params
            .get("min_events")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MIN_EVENTS) as usize;
        let low_rate = config
            .params
            .get("low_rate_threshold")
            .and_then(|v| v.as_f64())
            .unwrap_or(DEFAULT_LOW_RATE);
        let recent_n = config
            .params
            .get("recent_n")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_RECENT_N) as usize;

        let entries = self.read_recent(recent_n);

        if entries.is_empty() {
            return Some(WatcherEvent {
                priority: SignalPriority::Info,
                summary: "Sensorium: no audit entries found — sensors may not be producing events, \
                           or the audit trail has not been initialized yet."
                    .into(),
                segment_refs: vec![],
            });
        }

        let total = entries.len();
        let promoted = entries
            .iter()
            .filter(|e| e.action_taken == AuditAction::Promoted)
            .count();
        let denied_by_consent = entries
            .iter()
            .filter(|e| e.action_taken == AuditAction::DeniedByConsent)
            .count();
        let rate = promoted as f64 / total as f64;

        if total >= min_events && rate < low_rate {
            Some(WatcherEvent {
                priority: SignalPriority::Info,
                summary: format!(
                    "Sensorium health: low promotion rate {:.1}% ({promoted}/{total} events promoted, \
                     {denied_by_consent} denied by consent) in last {recent_n} entries. \
                     Consider reviewing consent policies or attention rules.",
                    rate * 100.0
                ),
                segment_refs: vec![],
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::identity::EventId;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_entries(path: &std::path::Path, entries: &[AuditEntry]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        for e in entries {
            writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
        }
    }

    fn entry(action: AuditAction) -> AuditEntry {
        AuditEntry {
            timestamp: chrono::Utc::now(),
            event_id: EventId::new(),
            consent_policy: None,
            attention_tier_reached: 1,
            action_taken: action,
            segment_created: None,
        }
    }

    #[test]
    fn no_signal_when_healthy_promotion_rate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("audit.jsonl");
        let entries: Vec<_> = (0..40)
            .map(|i| {
                if i % 2 == 0 {
                    entry(AuditAction::Promoted)
                } else {
                    entry(AuditAction::Logged)
                }
            })
            .collect();
        write_entries(&path, &entries);

        let watcher = SensoriumHealthWatcher::new(path);
        let cfg = WatcherConfig::default();
        // 50% promotion rate — healthy, no signal
        assert!(watcher.check(&cfg).is_none());
    }

    #[test]
    fn signal_when_low_promotion_rate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("audit.jsonl");
        // 2 promoted out of 40 = 5% — right at the threshold
        let mut entries: Vec<_> = (0..38).map(|_| entry(AuditAction::Logged)).collect();
        entries.push(entry(AuditAction::Promoted));
        entries.push(entry(AuditAction::Promoted));
        write_entries(&path, &entries);

        let watcher = SensoriumHealthWatcher::new(path);
        let cfg = WatcherConfig {
            params: serde_json::json!({ "low_rate_threshold": 0.1 }),
            ..Default::default()
        };
        let event = watcher.check(&cfg).expect("should fire on low promotion rate");
        assert_eq!(event.priority, SignalPriority::Info);
        assert!(event.summary.contains("low promotion rate"));
    }

    #[test]
    fn signal_when_no_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("audit.jsonl");
        // File does not exist
        let watcher = SensoriumHealthWatcher::new(path);
        let cfg = WatcherConfig::default();
        let event = watcher.check(&cfg).expect("should fire when no entries");
        assert!(event.summary.contains("no audit entries"));
    }

    #[test]
    fn no_signal_below_min_events() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("audit.jsonl");
        // Only 5 entries — below default min_events of 30
        let entries: Vec<_> = (0..5).map(|_| entry(AuditAction::Logged)).collect();
        write_entries(&path, &entries);

        let watcher = SensoriumHealthWatcher::new(path);
        let cfg = WatcherConfig::default();
        assert!(watcher.check(&cfg).is_none());
    }
}
