//! Segment pressure watcher — signals when memory consolidation is recommended.
//!
//! Monitors total VectorStore segment count at tier 0 (Warm) and fires a
//! `Normal`-priority signal when the count exceeds a configurable threshold.
//!
//! This gives the reasoning engine a predictable nudge to run Mnemos
//! consolidation rather than waiting for unbounded segment growth.
//!
//! # Config params
//! - `threshold` (integer, default 2000) — segment count above which the signal fires.

use crate::watcher::{Watcher, WatcherConfig, WatcherEvent};
use animus_core::threading::SignalPriority;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_THRESHOLD: u64 = 2000;
const DEFAULT_INTERVAL_SECS: u64 = 300; // 5 minutes

pub struct SegmentPressureWatcher {
    store: Arc<dyn VectorStore>,
}

impl SegmentPressureWatcher {
    pub fn new(store: Arc<dyn VectorStore>) -> Self {
        Self { store }
    }
}

impl Watcher for SegmentPressureWatcher {
    fn id(&self) -> &str {
        "segment_pressure"
    }

    fn name(&self) -> &str {
        "Segment Pressure"
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(DEFAULT_INTERVAL_SECS)
    }

    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent> {
        let threshold = config
            .params
            .get("threshold")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_THRESHOLD) as usize;

        let total = self.store.count(None);
        if total > threshold {
            Some(WatcherEvent {
                priority: SignalPriority::Normal,
                summary: format!(
                    "Memory pressure: {total} segments stored (threshold {threshold}). \
                     Consider running Mnemos consolidation to reduce warm-tier density."
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
    use animus_core::segment::{Content, Segment, Source};
    use animus_vectorfs::store::MmapVectorStore;
    use tempfile::TempDir;

    fn make_watcher(dir: &TempDir) -> (SegmentPressureWatcher, Arc<MmapVectorStore>) {
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let watcher = SegmentPressureWatcher::new(store.clone() as Arc<dyn VectorStore>);
        (watcher, store)
    }

    fn empty_config() -> WatcherConfig {
        WatcherConfig::default()
    }

    fn config_with_threshold(t: u64) -> WatcherConfig {
        WatcherConfig {
            params: serde_json::json!({ "threshold": t }),
            ..Default::default()
        }
    }

    fn add_segment(store: &Arc<MmapVectorStore>) {
        let seg = Segment::new(
            Content::Text("test segment".into()),
            vec![0.1, 0.2, 0.3, 0.4],
            Source::Manual { description: "test".into() },
        );
        store.store(seg).unwrap();
    }

    #[test]
    fn no_signal_below_threshold() {
        let dir = TempDir::new().unwrap();
        let (watcher, _store) = make_watcher(&dir);
        let cfg = config_with_threshold(10);
        assert!(watcher.check(&cfg).is_none());
    }

    #[test]
    fn signal_above_threshold() {
        let dir = TempDir::new().unwrap();
        let (watcher, store) = make_watcher(&dir);
        let cfg = config_with_threshold(2);

        for _ in 0..3 {
            add_segment(&store);
        }

        let event = watcher.check(&cfg).expect("should fire above threshold");
        assert_eq!(event.priority, SignalPriority::Normal);
        assert!(event.summary.contains("3 segments"));
    }

    #[test]
    fn default_threshold_applied_when_no_params() {
        let dir = TempDir::new().unwrap();
        let (watcher, _store) = make_watcher(&dir);
        // Empty store — should never fire with default threshold of 2000
        assert!(watcher.check(&empty_config()).is_none());
    }
}
