//! HandoffBundle — VectorFS-native knowledge transfer for role transitions.
//!
//! When an instance yields a role, it packages relevant segments for the
//! incoming instance. Segments are already embedded — no re-embedding at
//! the receiving end.
//!
//! ## Design
//!
//! Segments are selected by tag/source at export time. The receiving instance
//! calls `ingest()` which writes them into its local VectorFS with provenance tags
//! marking the transfer origin. This preserves the knowledge graph without
//! requiring the receiver to re-embed or reprocess.

use animus_core::identity::InstanceId;
use animus_core::mesh::MeshRole;
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_vectorfs::VectorStore;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// HandoffSegment
// ---------------------------------------------------------------------------

/// A single segment prepared for transfer.
///
/// The embedding is carried inline so the receiving instance can ingest
/// directly without calling its embedding service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffSegment {
    pub content: String,
    pub embedding: Vec<f32>,
    pub confidence: f32,
    pub decay_class: DecayClass,
    pub tags: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// HandoffBundle
// ---------------------------------------------------------------------------

/// VectorFS-native knowledge transfer bundle for role transitions.
///
/// Created by the yielding instance, consumed by the successor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffBundle {
    /// Instance that created this bundle (the one yielding the role).
    pub source_instance: InstanceId,
    /// The role being transferred.
    pub yielded_role: MeshRole,
    /// Human-readable reason for the transfer.
    pub transfer_reason: String,
    /// Knowledge segments relevant to the yielded role.
    pub segments: Vec<HandoffSegment>,
    /// Short summaries of active goals associated with this role.
    pub goal_summaries: Vec<String>,
    /// Short summaries of recent reasoning threads related to this role.
    pub thread_summaries: Vec<String>,
    /// When this bundle was created.
    pub created: DateTime<Utc>,
}

impl HandoffBundle {
    /// Create a new empty bundle (caller fills fields).
    pub fn new(
        source_instance: InstanceId,
        yielded_role: MeshRole,
        transfer_reason: String,
    ) -> Self {
        Self {
            source_instance,
            yielded_role,
            transfer_reason,
            segments: Vec::new(),
            goal_summaries: Vec::new(),
            thread_summaries: Vec::new(),
            created: Utc::now(),
        }
    }

    /// Export segments from VectorFS tagged with the given role label.
    ///
    /// Selects segments whose `tags["mesh_role"]` matches the role label,
    /// or whose `tags["handoff_include"]` is `"true"`.
    /// Returns a bundle with those segments populated; goal/thread summaries
    /// must be filled by the caller (they require higher-level knowledge).
    pub fn export_from_store(
        source_instance: InstanceId,
        yielded_role: MeshRole,
        transfer_reason: String,
        store: &dyn VectorStore,
        max_segments: usize,
    ) -> Self {
        let role_label = yielded_role.label().to_lowercase();

        let all_ids = store.segment_ids(None);
        let mut segments = Vec::new();

        for id in all_ids.iter().take(max_segments * 4) {
            if segments.len() >= max_segments {
                break;
            }
            let Ok(Some(seg)) = store.get_raw(*id) else { continue };

            let include = seg.tags.get("mesh_role").map_or(false, |r| r.to_lowercase() == role_label)
                || seg.tags.get("handoff_include").map_or(false, |v| v == "true");

            if !include { continue; }

            let content_str = match &seg.content {
                Content::Text(t) => t.clone(),
                Content::Structured(v) => v.to_string(),
                _ => continue, // skip binary/reference — not transferable as text
            };

            segments.push(HandoffSegment {
                content: content_str,
                embedding: seg.embedding.clone(),
                confidence: seg.confidence,
                decay_class: seg.decay_class,
                tags: seg.tags.clone(),
            });
        }

        let mut bundle = Self::new(source_instance, yielded_role, transfer_reason);
        bundle.segments = segments;
        bundle
    }

    /// Ingest this bundle into the given VectorFS store.
    ///
    /// Segments are written with provenance tags marking:
    /// - `handoff_from`: the source instance ID
    /// - `handoff_role`: the yielded role label
    /// - `handoff_at`: ISO 8601 timestamp
    ///
    /// Returns the number of segments successfully ingested.
    pub fn ingest(self, store: &dyn VectorStore) -> usize {
        let source_str = self.source_instance.to_string();
        let role_str = self.yielded_role.label().to_string();
        let ts = self.created.format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let mut count = 0usize;

        for hs in self.segments {
            let mut tags = hs.tags;
            tags.insert("handoff_from".to_string(), source_str.clone());
            tags.insert("handoff_role".to_string(), role_str.clone());
            tags.insert("handoff_at".to_string(), ts.clone());

            let mut seg = Segment::new(
                Content::Text(hs.content),
                hs.embedding,
                Source::Manual { description: format!("handoff from {}/{}", source_str, role_str) },
            );
            seg.confidence = hs.confidence;
            seg.decay_class = hs.decay_class;
            seg.tags = tags;

            if store.store(seg).is_ok() {
                count += 1;
            }
        }

        tracing::info!(
            "HandoffBundle: ingested {} segments from {} (role: {})",
            count, source_str, role_str
        );

        count
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::identity::InstanceId;
    use animus_vectorfs::store::MmapVectorStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> Arc<MmapVectorStore> {
        Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap())
    }

    fn dummy_segment(role_label: &str, content: &str) -> Segment {
        let mut seg = Segment::new(
            Content::Text(content.to_string()),
            vec![0.1, 0.2, 0.3, 0.4],
            Source::Manual { description: "test".to_string() },
        );
        seg.tags.insert("mesh_role".to_string(), role_label.to_string());
        seg
    }

    #[test]
    fn empty_bundle_ingest_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let bundle = HandoffBundle::new(
            InstanceId::new(),
            MeshRole::Analyst,
            "test transfer".to_string(),
        );

        let ingested = bundle.ingest(store.as_ref());
        assert_eq!(ingested, 0);
    }

    #[test]
    fn ingest_adds_segments_with_provenance_tags() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let source_id = InstanceId::new();
        let mut bundle = HandoffBundle::new(
            source_id,
            MeshRole::Analyst,
            "tier drop".to_string(),
        );
        bundle.segments.push(HandoffSegment {
            content: "key analytical insight".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            confidence: 0.9,
            decay_class: DecayClass::Factual,
            tags: HashMap::new(),
        });

        let ingested = bundle.ingest(store.as_ref());
        assert_eq!(ingested, 1);

        // Verify the segment was stored and has provenance tags
        let count = store.count(None);
        assert_eq!(count, 1);

        let ids = store.segment_ids(None);
        let seg = store.get_raw(ids[0]).unwrap().unwrap();
        assert_eq!(seg.tags.get("handoff_role").map(|s| s.as_str()), Some("Analyst"));
        assert_eq!(seg.tags.get("handoff_from").map(|s| s.as_str()), Some(source_id.to_string().as_str()));
    }

    #[test]
    fn export_selects_role_tagged_segments() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        store.store(dummy_segment("analyst", "analyst knowledge")).unwrap();
        store.store(dummy_segment("coordinator", "coordinator knowledge")).unwrap();
        // A segment with handoff_include tag
        let mut include_seg = Segment::new(
            Content::Text("must transfer".to_string()),
            vec![0.1, 0.2, 0.3, 0.4],
            Source::Manual { description: "test".to_string() },
        );
        include_seg.tags.insert("handoff_include".to_string(), "true".to_string());
        store.store(include_seg).unwrap();

        let bundle = HandoffBundle::export_from_store(
            InstanceId::new(),
            MeshRole::Analyst,
            "test".to_string(),
            store.as_ref(),
            10,
        );

        // Should include the "analyst" tagged segment and the "handoff_include" one
        assert_eq!(bundle.segments.len(), 2);
    }
}
