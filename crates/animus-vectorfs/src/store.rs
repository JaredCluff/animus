use animus_core::error::{AnimusError, Result};
use animus_core::identity::{PolicyId, SegmentId};
use animus_core::segment::{Content, DecayClass, Principal, Segment, Source, Tier};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Report from a full scan of segment files on disk.
#[derive(Debug, Default, Clone)]
pub struct StoreHealthReport {
    /// Number of `.bin` files examined.
    pub files_scanned: usize,
    /// Segments that deserialized and have correct dimensionality.
    pub healthy: usize,
    /// Files that failed to deserialize (binary corruption).
    pub corrupted: usize,
    pub corrupted_ids: Vec<String>,
    /// Segments whose embedding length doesn't match the store's dimensionality.
    pub dim_mismatch: usize,
    pub dim_mismatch_ids: Vec<String>,
    /// Files exceeding MAX_SEGMENT_BYTES.
    pub oversized: usize,
    pub oversized_ids: Vec<String>,
    /// Count of files that couldn't be read due to OS I/O errors.
    pub io_errors: usize,
    /// Set if the segments directory itself couldn't be opened.
    pub io_error: Option<String>,
}

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

// ---------------------------------------------------------------------------
// Legacy segment formats for migration
//
// When the Segment struct gains new fields, old .bin files serialized without
// those fields fail to deserialize with bincode (binary layout is positional).
// The structs below mirror historical layouts so we can attempt recovery before
// quarantining an unreadable segment.
//
// Version history:
//   V0 (ac964d5 → 59b3ed5): no tags, no alpha/beta, no decay_class
//   V1 (59b3ed6 → 3b2e826): tags added, still no alpha/beta/decay_class
//   Current (3b2e827+):      alpha, beta, decay_class added (with serde defaults)
// ---------------------------------------------------------------------------

/// Segment layout before lifecycle capabilities (59b3ed6): no tags, alpha, beta, or decay_class.
#[derive(serde::Deserialize)]
struct SegmentV0 {
    id: SegmentId,
    embedding: Vec<f32>,
    content: Content,
    source: Source,
    confidence: f32,
    lineage: Vec<SegmentId>,
    tier: Tier,
    relevance_score: f32,
    access_count: u64,
    last_accessed: chrono::DateTime<chrono::Utc>,
    created: chrono::DateTime<chrono::Utc>,
    associations: Vec<(SegmentId, f32)>,
    consent_policy: Option<PolicyId>,
    observable_by: Vec<Principal>,
}

/// Segment layout after lifecycle capabilities (59b3ed6) but before quality gate (3b2e827):
/// has tags, but no alpha/beta/decay_class.
#[derive(serde::Deserialize)]
struct SegmentV1 {
    id: SegmentId,
    embedding: Vec<f32>,
    content: Content,
    source: Source,
    confidence: f32,
    lineage: Vec<SegmentId>,
    tier: Tier,
    relevance_score: f32,
    access_count: u64,
    last_accessed: chrono::DateTime<chrono::Utc>,
    created: chrono::DateTime<chrono::Utc>,
    associations: Vec<(SegmentId, f32)>,
    consent_policy: Option<PolicyId>,
    observable_by: Vec<Principal>,
    tags: std::collections::HashMap<String, String>,
}

fn migrate_v0(v: SegmentV0) -> Segment {
    Segment {
        id: v.id,
        embedding: v.embedding,
        content: v.content,
        source: v.source,
        confidence: v.confidence,
        lineage: v.lineage,
        tier: v.tier,
        relevance_score: v.relevance_score,
        access_count: v.access_count,
        last_accessed: v.last_accessed,
        created: v.created,
        associations: v.associations,
        consent_policy: v.consent_policy,
        observable_by: v.observable_by,
        tags: std::collections::HashMap::new(),
        alpha: 1.0,
        beta: 1.0,
        decay_class: DecayClass::default(),
    }
}

fn migrate_v1(v: SegmentV1) -> Segment {
    Segment {
        id: v.id,
        embedding: v.embedding,
        content: v.content,
        source: v.source,
        confidence: v.confidence,
        lineage: v.lineage,
        tier: v.tier,
        relevance_score: v.relevance_score,
        access_count: v.access_count,
        last_accessed: v.last_accessed,
        created: v.created,
        associations: v.associations,
        consent_policy: v.consent_policy,
        observable_by: v.observable_by,
        tags: v.tags,
        alpha: 1.0,
        beta: 1.0,
        decay_class: DecayClass::default(),
    }
}

/// Attempt to deserialize a segment from raw bytes, trying legacy formats
/// (V1 then V0) if the current format fails.
///
/// Returns `Some(segment)` if any version succeeded, `None` if all fail.
fn try_deserialize_any_version(data: &[u8]) -> Option<(Segment, &'static str)> {
    if let Ok(s) = bincode::deserialize::<SegmentV1>(data) {
        return Some((migrate_v1(s), "V1"));
    }
    if let Ok(s) = bincode::deserialize::<SegmentV0>(data) {
        return Some((migrate_v0(s), "V0"));
    }
    None
}

/// Move an unrecoverable segment file to the quarantine directory.
/// NEVER deletes from disk — per CLAUDE.md VectorFS Memory Rules.
fn quarantine_segment(path: &Path, quarantine_dir: &Path) {
    if let Err(e) = fs::create_dir_all(quarantine_dir) {
        tracing::error!(
            "cannot create quarantine dir {}: {e}; segment {} left in place",
            quarantine_dir.display(),
            path.display()
        );
        return;
    }

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown.bin".to_string());
    let ts = chrono::Utc::now().timestamp();
    let dest = quarantine_dir.join(format!("{ts}-{file_name}"));

    // Try atomic rename first; fall back to copy+remove for cross-device moves.
    if fs::rename(path, &dest).is_ok() {
        tracing::warn!(
            "quarantined unrecoverable segment: {} → {}",
            path.display(),
            dest.display()
        );
        return;
    }
    match fs::copy(path, &dest) {
        Ok(_) => {
            if let Err(e) = fs::remove_file(path) {
                tracing::warn!(
                    "quarantined segment to {} but could not remove original: {e}",
                    dest.display()
                );
            } else {
                tracing::warn!(
                    "quarantined unrecoverable segment: {} → {}",
                    path.display(),
                    dest.display()
                );
            }
        }
        Err(e) => {
            tracing::error!(
                "failed to quarantine segment {} (copy error: {e}); left in place",
                path.display()
            );
        }
    }
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

        let quarantine_dir = dir.join("quarantine");

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
                let segment = match bincode::deserialize::<Segment>(&data) {
                    Ok(s) => s,
                    Err(_) => {
                        // Current format failed — try legacy versions before giving up.
                        match try_deserialize_any_version(&data) {
                            Some((migrated, version)) => {
                                tracing::info!(
                                    "migrated segment {} from legacy format {version} to current",
                                    migrated.id
                                );
                                // Re-persist in current format so future loads succeed.
                                let final_path = segments_dir.join(format!("{}.bin", migrated.id.0));
                                let tmp_path = segments_dir.join(format!("{}.bin.tmp", migrated.id.0));
                                if let Ok(encoded) = bincode::serialize(&migrated) {
                                    let _ = fs::write(&tmp_path, &encoded)
                                        .and_then(|_| fs::rename(&tmp_path, &final_path));
                                }
                                migrated
                            }
                            None => {
                                // All versions failed — quarantine, never delete.
                                quarantine_segment(&path, &quarantine_dir);
                                continue;
                            }
                        }
                    }
                };
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

    /// Replace the embedding for an existing segment with a new vector.
    ///
    /// Updates the in-memory segment map, re-indexes the segment in HNSW
    /// (remove old mapping + insert with new vector), and persists the updated
    /// segment to disk atomically. Used by the startup synthetic-embedding
    /// migration to replace hash-based synthetic vectors with real embeddings.
    ///
    /// The old HNSW entry becomes a ghost node (no SegmentId mapping), which
    /// is harmless — search results filter unmapped nodes via filter_map.
    pub fn update_embedding(&self, id: SegmentId, embedding: Vec<f32>) -> Result<()> {
        if embedding.len() != self.dimensionality {
            return Err(AnimusError::DimensionMismatch {
                expected: self.dimensionality,
                actual: embedding.len(),
            });
        }
        if embedding.iter().any(|v| !v.is_finite()) {
            return Err(AnimusError::Storage(
                "replacement embedding contains NaN or Inf".to_string(),
            ));
        }

        let segment_clone = {
            let mut segments = self.segments.write();
            let seg = segments
                .get_mut(&id)
                .ok_or(AnimusError::SegmentNotFound(id.0))?;
            seg.embedding = embedding.clone();
            seg.clone()
        };

        // Re-index: remove old vector mapping, insert new one.
        // hnsw_rs doesn't support true deletion; remove() clears the ID map entry
        // so the old vector becomes an unreachable ghost. The new insert gives
        // the segment a fresh internal ID pointing to the real embedding.
        let _ = self.index.remove(id); // ok if not in index
        if let Err(e) = self.index.insert(id, &embedding) {
            tracing::warn!("update_embedding: HNSW re-insert for {id} failed: {e}");
        }

        self.persist_segment(&segment_clone)
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

    /// Scan all segment files on disk and return a health report.
    ///
    /// Reads every `.bin` file in the segments directory and attempts to
    /// deserialize it. Reports counts of healthy, corrupted, oversized, and
    /// dimension-mismatched segments without modifying anything.
    pub fn scan_health(&self) -> StoreHealthReport {
        let segments_dir = self.base_dir.join("segments");
        let mut report = StoreHealthReport::default();

        let entries = match fs::read_dir(&segments_dir) {
            Ok(e) => e,
            Err(e) => {
                report.io_error = Some(format!("cannot read segments dir: {e}"));
                return report;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "bin") {
                continue;
            }
            report.files_scanned += 1;

            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => { report.io_errors += 1; continue; }
            };

            if metadata.len() > MAX_SEGMENT_BYTES {
                report.oversized += 1;
                report.oversized_ids.push(path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string());
                continue;
            }

            let data = match fs::read(&path) {
                Ok(d) => d,
                Err(_) => { report.io_errors += 1; continue; }
            };

            match bincode::deserialize::<Segment>(&data) {
                Ok(seg) => {
                    if seg.embedding.len() != self.dimensionality {
                        report.dim_mismatch += 1;
                        report.dim_mismatch_ids.push(seg.id.to_string());
                    } else {
                        report.healthy += 1;
                    }
                }
                Err(_) => {
                    report.corrupted += 1;
                    report.corrupted_ids.push(path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string());
                }
            }
        }

        report
    }

    /// Remove all corrupted and dimension-mismatched segment files from disk,
    /// evicting them from the in-memory map and HNSW index as well.
    ///
    /// Returns the number of files removed.
    pub fn remove_corrupted(&self) -> usize {
        let report = self.scan_health();
        let mut removed = 0usize;
        let ids_to_remove: Vec<String> = report.corrupted_ids.iter()
            .chain(report.dim_mismatch_ids.iter())
            .cloned()
            .collect();

        for id_str in &ids_to_remove {
            let path = self.base_dir.join("segments").join(format!("{id_str}.bin"));
            if fs::remove_file(&path).is_ok() {
                removed += 1;
                // Also evict from in-memory state if we can parse the UUID
                if let Ok(uuid) = uuid::Uuid::parse_str(id_str) {
                    let id = SegmentId(uuid);
                    self.segments.write().remove(&id);
                    // HNSW doesn't support deletion — segment simply won't be found
                    // on next query. Harmless orphan in the index; cleaned on restart.
                }
            }
        }
        removed
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
                let segment = match bincode::deserialize::<Segment>(&data) {
                    Ok(s) => s,
                    Err(_) => match try_deserialize_any_version(&data) {
                        Some((migrated, version)) => {
                            tracing::info!(
                                "migrated snapshot segment {} from legacy format {version}",
                                migrated.id
                            );
                            migrated
                        }
                        None => {
                            let quarantine_dir = self.base_dir.join("quarantine");
                            quarantine_segment(&path, &quarantine_dir);
                            continue;
                        }
                    },
                };
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
        }

        tracing::info!("Restored {count} segments from snapshot at {}", snapshot_dir.display());
        Ok(count)
    }
}
