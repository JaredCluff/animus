use animus_core::error::{AnimusError, Result};
use animus_core::identity::SegmentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Tracks feedback signals for the quality gate.
/// V0.1: simple heuristic based on human corrections and acceptances.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct QualityTracker {
    /// Segment ID -> (acceptances, corrections)
    feedback: HashMap<SegmentId, (u32, u32)>,
}

impl QualityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that knowledge from this segment was accepted by the human.
    pub fn record_acceptance(&mut self, segment_id: SegmentId) {
        let entry = self.feedback.entry(segment_id).or_insert((0, 0));
        entry.0 += 1;
    }

    /// Record that knowledge from this segment was corrected by the human.
    pub fn record_correction(&mut self, segment_id: SegmentId) {
        let entry = self.feedback.entry(segment_id).or_insert((0, 0));
        entry.1 += 1;
    }

    /// Compute a confidence adjustment based on feedback.
    /// Returns a value to ADD to the segment's confidence.
    /// Positive = boost, negative = reduce.
    pub fn confidence_adjustment(&self, segment_id: SegmentId) -> f32 {
        match self.feedback.get(&segment_id) {
            Some((acceptances, corrections)) => {
                let total = *acceptances + *corrections;
                if total == 0 {
                    return 0.0;
                }
                let acceptance_rate = *acceptances as f32 / total as f32;
                // -0.2 to +0.2 range
                (acceptance_rate - 0.5) * 0.4
            }
            None => 0.0,
        }
    }

    /// Persist to disk.
    pub fn save(&self, path: &Path) -> Result<()> {
        let data = bincode::serialize(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &data)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load from disk.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let data = std::fs::read(path)?;
        let tracker: Self = bincode::deserialize(&data).map_err(|e| {
            AnimusError::Storage(format!("failed to load quality tracker: {e}"))
        })?;
        Ok(tracker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acceptance_boosts_confidence() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_acceptance(id);
        tracker.record_acceptance(id);
        tracker.record_acceptance(id);

        let adj = tracker.confidence_adjustment(id);
        assert!(adj > 0.0, "3 acceptances should boost confidence");
    }

    #[test]
    fn test_corrections_reduce_confidence() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_correction(id);
        tracker.record_correction(id);
        tracker.record_correction(id);

        let adj = tracker.confidence_adjustment(id);
        assert!(adj < 0.0, "3 corrections should reduce confidence");
    }

    #[test]
    fn test_mixed_feedback() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_acceptance(id);
        tracker.record_correction(id);

        let adj = tracker.confidence_adjustment(id);
        assert!(
            adj.abs() < 0.01,
            "equal accept/correct should be near zero, got {adj}"
        );
    }
}
