use std::collections::HashMap;
use std::path::Path;

use animus_core::segment::DecayClass;
use animus_core::{Result, Segment, SegmentId, Tier};

/// Metadata update for a segment (partial update without replacing content).
#[derive(Debug, Default)]
pub struct SegmentUpdate {
    pub relevance_score: Option<f32>,
    pub confidence: Option<f32>,
    pub associations: Option<Vec<(SegmentId, f32)>>,
    pub tags: Option<HashMap<String, String>>,
    /// Update Bayesian alpha parameter.
    pub alpha: Option<f32>,
    /// Update Bayesian beta parameter.
    pub beta: Option<f32>,
    /// Update decay class.
    pub decay_class: Option<DecayClass>,
}

/// The core storage abstraction for VectorFS.
pub trait VectorStore: Send + Sync {
    /// Store a new segment. Returns the segment's ID.
    fn store(&self, segment: Segment) -> Result<SegmentId>;

    /// Retrieve segments by semantic similarity to the given embedding.
    fn query(
        &self,
        embedding: &[f32],
        top_k: usize,
        tier_filter: Option<Tier>,
    ) -> Result<Vec<Segment>>;

    /// Retrieve a segment by exact ID. Records an access.
    fn get(&self, id: SegmentId) -> Result<Option<Segment>>;

    /// Retrieve a segment by exact ID without recording an access.
    /// Used by internal systems (TierManager, Consolidator) that need to read
    /// segments without affecting their access statistics.
    fn get_raw(&self, id: SegmentId) -> Result<Option<Segment>>;

    /// Update segment metadata without replacing content.
    fn update_meta(&self, id: SegmentId, update: SegmentUpdate) -> Result<()>;

    /// Change a segment's storage tier.
    fn set_tier(&self, id: SegmentId, tier: Tier) -> Result<()>;

    /// Permanently delete a segment.
    fn delete(&self, id: SegmentId) -> Result<()>;

    /// Merge multiple segments into one consolidated segment.
    /// Source segments are deleted; the merged segment is stored.
    fn merge(&self, source_ids: Vec<SegmentId>, merged: Segment) -> Result<SegmentId>;

    /// Count segments, optionally filtered by tier.
    fn count(&self, tier_filter: Option<Tier>) -> usize;

    /// Get all segment IDs, optionally filtered by tier.
    fn segment_ids(&self, tier_filter: Option<Tier>) -> Vec<SegmentId>;

    /// Create a point-in-time snapshot of all segments to the given directory.
    /// Returns the number of segments captured.
    fn snapshot(&self, _snapshot_dir: &Path) -> Result<usize> {
        Err(animus_core::AnimusError::Storage(
            "snapshot not supported by this store".to_string(),
        ))
    }

    /// Restore segments from a previously captured snapshot.
    /// Returns the number of segments loaded.
    fn restore_from_snapshot(&self, _snapshot_dir: &Path) -> Result<usize> {
        Err(animus_core::AnimusError::Storage(
            "restore_from_snapshot not supported by this store".to_string(),
        ))
    }
}

pub mod index;
pub mod quality_gate;
pub mod store;
pub mod tier_manager;

pub use quality_gate::MemoryQualityGate;
pub use store::ReembedEntry;
