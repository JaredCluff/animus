use animus_core::error::{AnimusError, Result};
use animus_core::identity::SegmentId;
use animus_core::segment::{Segment, Tier};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::HnswIndex;
use crate::{SegmentUpdate, VectorStore};

/// File-backed VectorStore using bincode-serialized segment files and HNSW index.
pub struct MmapVectorStore {
    /// Base directory for storage.
    base_dir: PathBuf,
    /// In-memory segment map.
    segments: RwLock<HashMap<SegmentId, Segment>>,
    /// HNSW vector index for similarity search.
    index: HnswIndex,
    /// Vector dimensionality.
    dimensionality: usize,
}

impl MmapVectorStore {
    /// Open or create a VectorStore at the given directory.
    pub fn open(dir: &Path, dimensionality: usize) -> Result<Self> {
        let segments_dir = dir.join("segments");
        fs::create_dir_all(&segments_dir)?;

        let index = HnswIndex::new(dimensionality, 10_000);
        let mut segments = HashMap::new();

        // Load existing segments from disk
        for entry in fs::read_dir(&segments_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "bin") {
                let data = fs::read(&path)?;
                match bincode::deserialize::<Segment>(&data) {
                    Ok(segment) => {
                        if let Err(e) = index.insert(segment.id, &segment.embedding) {
                            tracing::warn!("failed to index segment {}: {e}", segment.id);
                            continue;
                        }
                        segments.insert(segment.id, segment);
                    }
                    Err(e) => {
                        tracing::warn!("failed to load segment from {}: {e}", path.display());
                    }
                }
            }
        }

        tracing::info!(
            "VectorFS opened at {} with {} segments",
            dir.display(),
            segments.len()
        );

        Ok(Self {
            base_dir: dir.to_path_buf(),
            segments: RwLock::new(segments),
            index,
            dimensionality,
        })
    }

    /// Flush all segments to disk.
    pub fn flush(&self) -> Result<()> {
        let segments = self.segments.read();
        let segments_dir = self.base_dir.join("segments");
        for (id, segment) in segments.iter() {
            let path = segments_dir.join(format!("{}.bin", id.0));
            let data = bincode::serialize(segment)?;
            fs::write(&path, &data)?;
        }
        Ok(())
    }

    /// Write a single segment to disk.
    fn persist_segment(&self, segment: &Segment) -> Result<()> {
        let segments_dir = self.base_dir.join("segments");
        let path = segments_dir.join(format!("{}.bin", segment.id.0));
        let data = bincode::serialize(segment)?;
        fs::write(&path, &data)?;
        Ok(())
    }

    /// Remove a segment file from disk.
    fn remove_segment_file(&self, id: SegmentId) -> Result<()> {
        let path = self
            .base_dir
            .join("segments")
            .join(format!("{}.bin", id.0));
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

impl VectorStore for MmapVectorStore {
    fn store(&self, segment: Segment) -> Result<SegmentId> {
        if segment.embedding.len() != self.dimensionality {
            return Err(AnimusError::DimensionMismatch {
                expected: self.dimensionality,
                actual: segment.embedding.len(),
            });
        }

        let id = segment.id;
        self.index.insert(id, &segment.embedding)?;
        self.persist_segment(&segment)?;
        self.segments.write().insert(id, segment);
        Ok(id)
    }

    fn query(
        &self,
        embedding: &[f32],
        top_k: usize,
        tier_filter: Option<Tier>,
    ) -> Result<Vec<Segment>> {
        // Search more than top_k in case some get filtered by tier
        let search_k = if tier_filter.is_some() {
            top_k * 3
        } else {
            top_k
        };

        let candidates = self.index.search(embedding, search_k)?;
        let segments = self.segments.read();

        let results: Vec<Segment> = candidates
            .into_iter()
            .filter_map(|(id, _distance)| {
                let seg = segments.get(&id)?;
                if let Some(tier) = tier_filter {
                    if seg.tier != tier {
                        return None;
                    }
                }
                Some(seg.clone())
            })
            .take(top_k)
            .collect();

        // Record access on returned segments
        drop(segments);
        let mut segments = self.segments.write();
        for result in &results {
            if let Some(seg) = segments.get_mut(&result.id) {
                seg.record_access();
            }
        }

        Ok(results)
    }

    fn get(&self, id: SegmentId) -> Result<Option<Segment>> {
        let mut segments = self.segments.write();
        if let Some(seg) = segments.get_mut(&id) {
            seg.record_access();
            Ok(Some(seg.clone()))
        } else {
            Ok(None)
        }
    }

    fn get_raw(&self, id: SegmentId) -> Result<Option<Segment>> {
        let segments = self.segments.read();
        Ok(segments.get(&id).cloned())
    }

    fn update_meta(&self, id: SegmentId, update: SegmentUpdate) -> Result<()> {
        let mut segments = self.segments.write();
        let seg = segments
            .get_mut(&id)
            .ok_or(AnimusError::SegmentNotFound(id.0))?;

        if let Some(score) = update.relevance_score {
            seg.relevance_score = score;
        }
        if let Some(conf) = update.confidence {
            seg.confidence = conf;
        }
        if let Some(assoc) = update.associations {
            seg.associations = assoc;
        }

        let segment_clone = seg.clone();
        drop(segments);
        self.persist_segment(&segment_clone)?;
        Ok(())
    }

    fn set_tier(&self, id: SegmentId, tier: Tier) -> Result<()> {
        let mut segments = self.segments.write();
        let seg = segments
            .get_mut(&id)
            .ok_or(AnimusError::SegmentNotFound(id.0))?;
        seg.tier = tier;

        let segment_clone = seg.clone();
        drop(segments);
        self.persist_segment(&segment_clone)?;
        Ok(())
    }

    fn delete(&self, id: SegmentId) -> Result<()> {
        self.segments.write().remove(&id);
        // Ignore index removal errors (segment might not be in index)
        let _ = self.index.remove(id);
        self.remove_segment_file(id)?;
        Ok(())
    }

    fn merge(&self, source_ids: Vec<SegmentId>, merged: Segment) -> Result<SegmentId> {
        let merged_id = self.store(merged)?;

        for id in source_ids {
            let _ = self.delete(id);
        }

        Ok(merged_id)
    }

    fn count(&self, tier_filter: Option<Tier>) -> usize {
        let segments = self.segments.read();
        match tier_filter {
            Some(tier) => segments.values().filter(|s| s.tier == tier).count(),
            None => segments.len(),
        }
    }

    fn segment_ids(&self, tier_filter: Option<Tier>) -> Vec<SegmentId> {
        let segments = self.segments.read();
        segments
            .iter()
            .filter(|(_, s)| tier_filter.is_none_or(|t| s.tier == t))
            .map(|(id, _)| *id)
            .collect()
    }
}
