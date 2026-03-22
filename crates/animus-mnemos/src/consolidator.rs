use animus_core::error::Result;
use animus_core::identity::SegmentId;
use animus_core::segment::{Content, Segment, Source, Tier};
use animus_vectorfs::VectorStore;
use std::collections::HashSet;
use std::sync::Arc;

use crate::evictor::cosine_similarity;

/// Background consolidation process for memory health.
pub struct Consolidator<S: VectorStore> {
    store: Arc<S>,
    /// Minimum cosine similarity to consider two segments related.
    similarity_threshold: f32,
}

impl<S: VectorStore> Consolidator<S> {
    pub fn new(store: Arc<S>, similarity_threshold: f32) -> Self {
        Self {
            store,
            similarity_threshold,
        }
    }

    /// Run one consolidation cycle.
    /// Finds clusters of similar warm segments and merges them.
    pub fn run_cycle(&self) -> Result<ConsolidationReport> {
        let mut report = ConsolidationReport::default();

        let warm_ids = self.store.segment_ids(Some(Tier::Warm));
        if warm_ids.len() < 2 {
            return Ok(report);
        }

        // Collect warm segments
        let mut warm_segments: Vec<Segment> = Vec::new();
        for id in &warm_ids {
            if let Some(seg) = self.store.get_raw(*id)? {
                warm_segments.push(seg);
            }
        }

        // Find clusters of similar segments
        let mut merged_ids: HashSet<SegmentId> = HashSet::new();

        for i in 0..warm_segments.len() {
            if merged_ids.contains(&warm_segments[i].id) {
                continue;
            }

            let mut cluster = vec![i];

            for j in (i + 1)..warm_segments.len() {
                if merged_ids.contains(&warm_segments[j].id) {
                    continue;
                }

                let sim = cosine_similarity(
                    &warm_segments[i].embedding,
                    &warm_segments[j].embedding,
                );

                if sim >= self.similarity_threshold {
                    cluster.push(j);
                }
            }

            // Only merge if we found near-duplicates with text content
            if cluster.len() >= 2 {
                let cluster_segments: Vec<&Segment> =
                    cluster.iter().map(|&idx| &warm_segments[idx]).collect();

                // Skip clusters with no text content to prevent data loss
                let has_text = cluster_segments
                    .iter()
                    .any(|s| matches!(&s.content, Content::Text(_)));
                if !has_text {
                    continue;
                }

                let merged = self.merge_cluster(&cluster_segments);
                let source_ids: Vec<SegmentId> =
                    cluster_segments.iter().map(|s| s.id).collect();

                for &id in &source_ids {
                    merged_ids.insert(id);
                }

                match self.store.merge(source_ids, merged) {
                    Ok(new_id) => {
                        tracing::debug!(
                            "consolidated {} segments into {}",
                            cluster.len(),
                            new_id
                        );
                        report.segments_merged += cluster.len();
                        report.segments_created += 1;
                    }
                    Err(e) => {
                        tracing::warn!("consolidation merge failed: {e}");
                    }
                }
            }
        }

        report.segments_scanned = warm_segments.len();
        Ok(report)
    }

    /// Merge a cluster of similar segments into one consolidated segment.
    fn merge_cluster(&self, segments: &[&Segment]) -> Segment {
        // Average the embeddings
        let dim = segments[0].embedding.len();
        let mut avg_embedding = vec![0.0f32; dim];
        for seg in segments {
            for (i, v) in seg.embedding.iter().enumerate() {
                avg_embedding[i] += v;
            }
        }
        let n = segments.len() as f32;
        for v in &mut avg_embedding {
            *v /= n;
        }

        // Re-normalize to unit vector for correct cosine distance
        let norm: f32 = avg_embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut avg_embedding {
                *v /= norm;
            }
        }

        // Concatenate text content
        let merged_text: String = segments
            .iter()
            .filter_map(|s| match &s.content {
                Content::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n---\n");

        // Use the highest confidence
        let max_confidence = segments
            .iter()
            .map(|s| s.confidence)
            .fold(0.0f32, f32::max);

        let lineage: Vec<SegmentId> = segments.iter().map(|s| s.id).collect();

        let mut merged = Segment::new(
            Content::Text(merged_text),
            avg_embedding,
            Source::Consolidation {
                merged_from: lineage.clone(),
            },
        );
        merged.confidence = max_confidence;
        merged.lineage = lineage;
        merged.tier = Tier::Warm;

        merged
    }
}

/// Report from a consolidation cycle.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    pub segments_scanned: usize,
    pub segments_merged: usize,
    pub segments_created: usize,
}
