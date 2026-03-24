use animus_core::{
    segment::{Content, DecayClass, Source},
    Result, Segment, SegmentId, Tier,
};
use animus_core::config::QualityGateConfig;
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use crate::{VectorStore, SegmentUpdate};

/// Null-state patterns: transient failures that don't need repeated storage.
const NULL_STATE_PATTERNS: &[&str] = &[
    "not responding", "silence", "no output", "final silence",
    "keepalive failed", "no response", "no more output",
    "conversation closed", "thread closed", "loop terminated",
];

fn is_null_state(text: &str) -> bool {
    let lower = text.to_lowercase();
    NULL_STATE_PATTERNS.iter().any(|p| lower.contains(p))
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 { 0.0 } else { dot / (mag_a * mag_b) }
}

/// Wrapping VectorStore decorator that applies quality filtering before writes.
/// Pass-through for all other operations.
pub struct MemoryQualityGate {
    inner: Arc<dyn VectorStore>,
    config: QualityGateConfig,
}

impl MemoryQualityGate {
    pub fn new(inner: Arc<dyn VectorStore>, config: QualityGateConfig) -> Self {
        Self { inner, config }
    }
}

impl VectorStore for MemoryQualityGate {
    fn store(&self, segment: Segment) -> Result<SegmentId> {
        if !self.config.enabled {
            return self.inner.store(segment);
        }
        // Only filter channel-sourced segments (Conversation or Manual).
        let is_channel_source = matches!(
            &segment.source,
            Source::Conversation { .. } | Source::Manual { .. }
        );
        if !is_channel_source {
            return self.inner.store(segment);
        }

        let window = chrono::Duration::hours(self.config.dedup_window_hours as i64);
        let dedup_cutoff = Utc::now() - window;

        // Query for the 20 most similar existing segments (similarity-first, then recency post-filter).
        let candidates = self.inner.query(&segment.embedding, 20, None).unwrap_or_default();
        let recent_similar: Vec<&Segment> = candidates.iter()
            .filter(|s| s.created >= dedup_cutoff)
            .collect();

        // 1. Semantic deduplication: skip if near-duplicate exists within window.
        for s in &recent_similar {
            let sim = cosine_similarity(&segment.embedding, &s.embedding);
            if sim >= self.config.dedup_similarity_threshold {
                tracing::debug!(
                    "MemoryQualityGate: dedup skip (similarity={:.3}, threshold={:.3})",
                    sim, self.config.dedup_similarity_threshold
                );
                return Ok(segment.id);
            }
        }

        // 2. Null-state suppression (channel text segments only).
        if let Content::Text(ref text) = segment.content {
            if is_null_state(text) {
                let cooldown = chrono::Duration::minutes(self.config.null_state_cooldown_minutes as i64);
                let cooldown_cutoff = Utc::now() - cooldown;
                let has_recent_null = candidates.iter().any(|s| {
                    s.created >= cooldown_cutoff
                        && matches!(&s.content, Content::Text(t) if is_null_state(t))
                });
                if has_recent_null {
                    tracing::debug!("MemoryQualityGate: null-state cooldown skip");
                    return Ok(segment.id);
                }
                // Store it as Ephemeral (short-lived).
                let mut seg = segment;
                seg.decay_class = DecayClass::Ephemeral;
                return self.inner.store(seg);
            }
        }

        self.inner.store(segment)
    }

    fn query(&self, embedding: &[f32], top_k: usize, tier_filter: Option<Tier>) -> Result<Vec<Segment>> {
        self.inner.query(embedding, top_k, tier_filter)
    }

    fn get(&self, id: SegmentId) -> Result<Option<Segment>> { self.inner.get(id) }
    fn get_raw(&self, id: SegmentId) -> Result<Option<Segment>> { self.inner.get_raw(id) }
    fn update_meta(&self, id: SegmentId, update: SegmentUpdate) -> Result<()> { self.inner.update_meta(id, update) }
    fn set_tier(&self, id: SegmentId, tier: Tier) -> Result<()> { self.inner.set_tier(id, tier) }
    fn delete(&self, id: SegmentId) -> Result<()> { self.inner.delete(id) }
    fn merge(&self, source_ids: Vec<SegmentId>, merged: Segment) -> Result<SegmentId> { self.inner.merge(source_ids, merged) }
    fn count(&self, tier_filter: Option<Tier>) -> usize { self.inner.count(tier_filter) }
    fn segment_ids(&self, tier_filter: Option<Tier>) -> Vec<SegmentId> { self.inner.segment_ids(tier_filter) }
    fn snapshot(&self, snapshot_dir: &Path) -> Result<usize> { self.inner.snapshot(snapshot_dir) }
    fn restore_from_snapshot(&self, snapshot_dir: &Path) -> Result<usize> { self.inner.restore_from_snapshot(snapshot_dir) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::segment::{Content, Source};
    use crate::store::MmapVectorStore;

    fn make_gate() -> (MemoryQualityGate, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let raw = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
        let cfg = QualityGateConfig::default();
        (MemoryQualityGate::new(raw as Arc<dyn VectorStore>, cfg), tmp)
    }

    fn make_segment(content: &str, embedding: Vec<f32>) -> Segment {
        Segment::new(
            Content::Text(content.to_string()),
            embedding,
            Source::Manual { description: "test".to_string() },
        )
    }

    #[test]
    fn dedup_blocks_near_identical() {
        let (gate, _tmp) = make_gate();
        let emb = vec![1.0_f32, 0.0, 0.0, 0.0];
        let s1 = make_segment("hello world", emb.clone());
        let id1 = gate.store(s1).unwrap();
        // Second segment with nearly identical embedding
        let s2 = make_segment("hello world slightly different", vec![0.999, 0.001, 0.0, 0.0]);
        let id2 = gate.store(s2).unwrap();
        // id2 should equal s2.id (skipped) but count should still be 1
        assert_eq!(gate.count(None), 1);
        let _ = (id1, id2);
    }

    #[test]
    fn unique_content_passes_through() {
        let (gate, _tmp) = make_gate();
        let s1 = make_segment("topic A", vec![1.0, 0.0, 0.0, 0.0]);
        let s2 = make_segment("topic B", vec![0.0, 1.0, 0.0, 0.0]);
        gate.store(s1).unwrap();
        gate.store(s2).unwrap();
        assert_eq!(gate.count(None), 2);
    }

    #[test]
    fn null_state_stored_as_ephemeral() {
        let (gate, _tmp) = make_gate();
        let s = make_segment("silence — not responding", vec![1.0, 0.0, 0.0, 0.0]);
        let id = gate.store(s).unwrap();
        let stored = gate.get(id).unwrap().unwrap();
        assert_eq!(stored.decay_class, DecayClass::Ephemeral);
    }

    #[test]
    fn null_state_deduped_within_cooldown() {
        let (gate, _tmp) = make_gate();
        let s1 = make_segment("silence — not responding", vec![1.0, 0.0, 0.0, 0.0]);
        gate.store(s1).unwrap();
        // Second null-state soon after — should be skipped
        let s2 = make_segment("silence — keepalive failed", vec![0.99, 0.0, 0.0, 0.01]);
        gate.store(s2).unwrap();
        assert_eq!(gate.count(None), 1);
    }
}
