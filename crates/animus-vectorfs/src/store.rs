use animus_core::error::{AnimusError, Result};
use animus_core::identity::SegmentId;
use animus_core::segment::{Content, DecayClass, Segment, Source, Tier};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::index::HnswIndex;
use crate::{SegmentUpdate, VectorStore};

/// Maximum serialized segment size: 64 MiB.
const MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;

/// Metadata persisted alongside the VectorFS store.
#[derive(Debug, Serialize, Deserialize)]
struct StoreMeta {
    dimensionality: usize,
}

/// A text segment preserved for re-embedding when dimensionality changes.
/// Written to `reembed-queue.jsonl` so the runtime can restore memories
/// after switching embedding providers.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReembedEntry {
    pub text: String,
    pub source: Source,
    pub decay_class: DecayClass,
    pub tags: std::collections::HashMap<String, String>,
}

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
    ///
    /// If the store already contains segments with a different dimensionality,
    /// incompatible segments are removed and the store is re-initialized with
    /// the new dimensionality.
    pub fn open(dir: &Path, dimensionality: usize) -> Result<Self> {
        let segments_dir = dir.join("segments");
        fs::create_dir_all(&segments_dir)?;

        let meta_path = dir.join("meta.json");
        let stored_dim = Self::read_meta(&meta_path)?;

        // Handle dimensionality mismatch: preserve text content for re-embedding,
        // then clear incompatible binary embeddings.
        if let Some(stored) = stored_dim {
            if stored != dimensionality {
                match Self::save_reembed_queue(dir, &segments_dir) {
                    Ok(n) if n > 0 => tracing::warn!(
                        "VectorFS dimensionality changed ({stored} -> {dimensionality}); \
                         saved {n} text segments to reembed-queue.jsonl for re-embedding on startup"
                    ),
                    Ok(_) => tracing::warn!(
                        "VectorFS dimensionality changed ({stored} -> {dimensionality}); \
                         no text segments to preserve"
                    ),
                    Err(e) => tracing::error!(
                        "VectorFS dimensionality changed but failed to save reembed queue: {e}; \
                         memories may be lost"
                    ),
                }
                Self::clear_segments(&segments_dir)?;
            }
        }

        // Persist current dimensionality
        Self::write_meta(&meta_path, dimensionality)?;

        let index = HnswIndex::new(dimensionality, 10_000);
        let mut segments = HashMap::new();

        // Load existing segments from disk
        for entry in fs::read_dir(&segments_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "bin") {
                let metadata = fs::metadata(&path)?;
                if metadata.len() > MAX_SEGMENT_BYTES {
                    tracing::warn!(
                        "segment file too large ({} bytes), skipping: {}",
                        metadata.len(),
                        path.display()
                    );
                    continue;
                }
                let data = fs::read(&path)?;
                match bincode::deserialize::<Segment>(&data) {
                    Ok(segment) => {
                        if segment.embedding.len() != dimensionality {
                            tracing::warn!(
                                "segment {} has {} dims (expected {}), removing",
                                segment.id, segment.embedding.len(), dimensionality
                            );
                            let _ = fs::remove_file(&path);
                            continue;
                        }
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
            "VectorFS opened at {} with {} segments (dim={})",
            dir.display(),
            segments.len(),
            dimensionality,
        );

        Ok(Self {
            base_dir: dir.to_path_buf(),
            segments: RwLock::new(segments),
            index,
            dimensionality,
        })
    }

    /// Read stored metadata, returning the persisted dimensionality if available.
    fn read_meta(meta_path: &Path) -> Result<Option<usize>> {
        if !meta_path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(meta_path)?;
        let meta: StoreMeta = serde_json::from_str(&data).map_err(|e| {
            AnimusError::Storage(format!("failed to parse VectorFS meta.json: {e}"))
        })?;
        Ok(Some(meta.dimensionality))
    }

    /// Write metadata to disk.
    fn write_meta(meta_path: &Path, dimensionality: usize) -> Result<()> {
        let meta = StoreMeta { dimensionality };
        let data = serde_json::to_string_pretty(&meta).map_err(|e| {
            AnimusError::Storage(format!("failed to serialize VectorFS meta.json: {e}"))
        })?;
        fs::write(meta_path, data)?;
        Ok(())
    }

    /// Scan segment files and write all text-content segments to `reembed-queue.jsonl`
    /// so the runtime can re-embed them after a dimensionality change.
    /// Non-text segments (observations, structured data) are silently dropped.
    fn save_reembed_queue(base_dir: &Path, segments_dir: &Path) -> Result<usize> {
        let queue_path = base_dir.join("reembed-queue.jsonl");
        let mut file = fs::File::create(&queue_path)?;
        let mut count = 0usize;
        for entry in fs::read_dir(segments_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "bin") {
                continue;
            }
            let data = match fs::read(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let seg: Segment = match bincode::deserialize(&data) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if let Content::Text(ref text) = seg.content {
                let entry = ReembedEntry {
                    text: text.clone(),
                    source: seg.source.clone(),
                    decay_class: seg.decay_class,
                    tags: seg.tags.clone(),
                };
                if let Ok(line) = serde_json::to_string(&entry) {
                    let _ = writeln!(file, "{}", line);
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Remove all segment files from the segments directory.
    fn clear_segments(segments_dir: &Path) -> Result<()> {
        for entry in fs::read_dir(segments_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "bin") {
                fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Path to the re-embed queue file (if it exists).
    pub fn reembed_queue_path(&self) -> std::path::PathBuf {
        self.base_dir.join("reembed-queue.jsonl")
    }

    /// Load all entries from the re-embed queue. Returns empty vec if no queue exists.
    pub fn load_reembed_queue(&self) -> Vec<ReembedEntry> {
        let path = self.reembed_queue_path();
        if !path.exists() {
            return Vec::new();
        }
        let Ok(contents) = fs::read_to_string(&path) else { return Vec::new(); };
        contents.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Delete the re-embed queue file after processing.
    pub fn clear_reembed_queue(&self) -> Result<()> {
        let path = self.reembed_queue_path();
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Returns the stored dimensionality.
    pub fn dimensionality(&self) -> usize {
        self.dimensionality
    }

    /// Flush all segments to disk using atomic writes.
    pub fn flush(&self) -> Result<()> {
        let segments = self.segments.read();
        for segment in segments.values() {
            self.persist_segment(segment)?;
        }
        Ok(())
    }

    /// Write a single segment to disk atomically (write-to-temp-then-rename).
    fn persist_segment(&self, segment: &Segment) -> Result<()> {
        let segments_dir = self.base_dir.join("segments");
        let final_path = segments_dir.join(format!("{}.bin", segment.id.0));
        let tmp_path = segments_dir.join(format!("{}.bin.tmp", segment.id.0));
        let data = bincode::serialize(segment)?;
        fs::write(&tmp_path, &data)?;
        fs::rename(&tmp_path, &final_path)?;
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

        // Reject NaN/Inf embeddings
        if segment.embedding.iter().any(|v| !v.is_finite()) {
            return Err(AnimusError::Storage(
                "embedding contains NaN or Inf values".to_string(),
            ));
        }

        let id = segment.id;
        // Persist to disk first — if this fails, no in-memory state is modified.
        // If HNSW insert fails after persist, the orphan file is benign (reloaded on restart).
        self.persist_segment(&segment)?;
        // Hold the write lock for the entire check+HNSW-insert+map-insert to prevent
        // a TOCTOU race: without this, two concurrent store() calls for the same ID
        // could both pass the contains_key check and both insert into HNSW, corrupting
        // top_k counts.
        {
            let mut segments = self.segments.write();
            if !segments.contains_key(&id) {
                self.index.insert(id, &segment.embedding)?;
            }
            segments.insert(id, segment);
        }
        Ok(id)
    }

    fn query(
        &self,
        embedding: &[f32],
        top_k: usize,
        tier_filter: Option<Tier>,
    ) -> Result<Vec<Segment>> {
        // Reject non-finite query embeddings — NaN/Inf in the query vector would
        // produce undefined distance values in the HNSW search, corrupting results.
        if embedding.iter().any(|v| !v.is_finite()) {
            return Err(AnimusError::Storage(
                "query embedding contains NaN or Inf values".to_string(),
            ));
        }

        // Search more than top_k in case some get filtered by tier
        let search_k = if tier_filter.is_some() {
            top_k.saturating_mul(3)
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

        // Record access on returned segments and persist
        drop(segments);
        let mut segments = self.segments.write();
        for result in &results {
            if let Some(seg) = segments.get_mut(&result.id) {
                seg.record_access();
            }
        }
        // Persist access metadata updates
        let to_persist: Vec<Segment> = results
            .iter()
            .filter_map(|r| segments.get(&r.id).cloned())
            .collect();
        drop(segments);
        for seg in &to_persist {
            if let Err(e) = self.persist_segment(seg) {
                tracing::warn!("failed to persist access update for {}: {e}", seg.id);
            }
        }

        Ok(results)
    }

    fn get(&self, id: SegmentId) -> Result<Option<Segment>> {
        let mut segments = self.segments.write();
        if let Some(seg) = segments.get_mut(&id) {
            seg.record_access();
            let cloned = seg.clone();
            drop(segments);
            if let Err(e) = self.persist_segment(&cloned) {
                tracing::warn!("failed to persist access update for {id}: {e}");
            }
            Ok(Some(cloned))
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
        if let Some(assoc) = update.associations {
            seg.associations = assoc;
        }
        if let Some(tags) = update.tags {
            seg.tags = tags;
        }
        let alpha_beta_changed = update.alpha.is_some() || update.beta.is_some();
        if let Some(alpha) = update.alpha {
            seg.alpha = alpha;
        }
        if let Some(beta) = update.beta {
            seg.beta = beta;
        }
        if alpha_beta_changed {
            // Recompute confidence from updated alpha/beta to keep them in sync.
            seg.confidence = seg.bayesian_confidence();
        } else if let Some(conf) = update.confidence {
            seg.confidence = conf;
        }
        if let Some(decay_class) = update.decay_class {
            seg.decay_class = decay_class;
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
            if let Err(e) = self.delete(id) {
                tracing::warn!("merge: failed to delete source segment {id}: {e}");
            }
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

    fn snapshot(&self, snapshot_dir: &Path) -> Result<usize> {
        let snap_segments = snapshot_dir.join("segments");
        fs::create_dir_all(&snap_segments)?;

        let meta_src = self.base_dir.join("meta.json");
        if meta_src.exists() {
            fs::copy(&meta_src, snapshot_dir.join("meta.json"))?;
        } else {
            Self::write_meta(&snapshot_dir.join("meta.json"), self.dimensionality)?;
        }

        let segments = self.segments.read();
        let mut count = 0;
        for segment in segments.values() {
            let data = bincode::serialize(segment)?;
            let path = snap_segments.join(format!("{}.bin", segment.id.0));
            fs::write(&path, &data)?;
            count += 1;
        }

        // Write COMPLETE marker last — absence means the snapshot was interrupted.
        fs::write(snapshot_dir.join("COMPLETE"), b"")?;
        tracing::info!("Snapshot created at {} ({count} segments)", snapshot_dir.display());
        Ok(count)
    }

    fn restore_from_snapshot(&self, snapshot_dir: &Path) -> Result<usize> {
        let snap_segments = snapshot_dir.join("segments");
        if !snap_segments.exists() {
            return Err(AnimusError::Storage(format!(
                "snapshot directory has no segments: {}",
                snapshot_dir.display()
            )));
        }
        if !snapshot_dir.join("COMPLETE").exists() {
            return Err(AnimusError::Storage(format!(
                "snapshot at {} is incomplete (missing COMPLETE marker)",
                snapshot_dir.display()
            )));
        }

        let mut count = 0;
        for entry in fs::read_dir(&snap_segments)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "bin") {
                let data = fs::read(&path)?;
                if data.len() as u64 > MAX_SEGMENT_BYTES {
                    tracing::warn!("Skipping oversized snapshot segment: {}", path.display());
                    continue;
                }
                match bincode::deserialize::<Segment>(&data) {
                    Ok(segment) => {
                        if segment.embedding.len() != self.dimensionality {
                            tracing::warn!(
                                "Skipping snapshot segment {} (dim {} != {})",
                                segment.id.0, segment.embedding.len(), self.dimensionality
                            );
                            continue;
                        }
                        self.persist_segment(&segment)?;
                        {
                            let mut segs = self.segments.write();
                            if !segs.contains_key(&segment.id) {
                                self.index.insert(segment.id, &segment.embedding)?;
                                segs.insert(segment.id, segment);
                                count += 1;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Skipping corrupt snapshot segment {}: {e}", path.display());
                    }
                }
            }
        }

        tracing::info!("Restored {count} segments from snapshot at {}", snapshot_dir.display());
        Ok(count)
    }
}
