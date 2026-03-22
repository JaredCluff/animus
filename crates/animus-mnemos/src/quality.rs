use animus_core::error::{AnimusError, Result};
use animus_core::identity::SegmentId;
use animus_core::segment::Segment;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Per-segment Bayesian feedback state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BayesianFeedback {
    /// Positive evidence (acceptances). Corresponds to Beta distribution alpha.
    pub alpha: f32,
    /// Negative evidence (corrections). Corresponds to Beta distribution beta.
    pub beta: f32,
}

impl BayesianFeedback {
    /// Uniform prior: Beta(1, 1).
    pub fn uniform_prior() -> Self {
        Self {
            alpha: 1.0,
            beta: 1.0,
        }
    }

    /// Mean of the Beta distribution: alpha / (alpha + beta).
    pub fn mean(&self) -> f32 {
        if self.alpha + self.beta == 0.0 {
            return 0.5;
        }
        self.alpha / (self.alpha + self.beta)
    }

    /// Variance of the Beta distribution.
    /// Lower variance = more certain about the estimate.
    pub fn variance(&self) -> f32 {
        let sum = self.alpha + self.beta;
        if sum == 0.0 {
            return 0.25; // maximum uncertainty
        }
        (self.alpha * self.beta) / (sum * sum * (sum + 1.0))
    }

    /// Total observations (alpha + beta - 2, since prior is Beta(1,1)).
    pub fn observation_count(&self) -> f32 {
        (self.alpha + self.beta - 2.0).max(0.0)
    }
}

/// Legacy V1 format for migration.
#[derive(Deserialize)]
struct LegacyTracker {
    feedback: HashMap<SegmentId, (u32, u32)>,
}

/// Tracks Bayesian feedback signals for the quality gate.
/// V0.2: Beta distribution tracking with principled uncertainty quantification.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct QualityTracker {
    feedback: HashMap<SegmentId, BayesianFeedback>,
}

impl QualityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that knowledge from this segment was accepted.
    /// Increments the Beta distribution alpha parameter.
    pub fn record_acceptance(&mut self, segment_id: SegmentId) {
        let entry = self
            .feedback
            .entry(segment_id)
            .or_insert_with(BayesianFeedback::uniform_prior);
        entry.alpha += 1.0;
    }

    /// Record that knowledge from this segment was corrected.
    /// Increments the Beta distribution beta parameter.
    pub fn record_correction(&mut self, segment_id: SegmentId) {
        let entry = self
            .feedback
            .entry(segment_id)
            .or_insert_with(BayesianFeedback::uniform_prior);
        entry.beta += 1.0;
    }

    /// Get the Bayesian feedback state for a segment, if any.
    pub fn get_feedback(&self, segment_id: SegmentId) -> Option<&BayesianFeedback> {
        self.feedback.get(&segment_id)
    }

    /// Compute the Bayesian confidence for a segment.
    /// Returns the mean of the Beta distribution, or 0.5 if no feedback.
    pub fn bayesian_confidence(&self, segment_id: SegmentId) -> f32 {
        match self.feedback.get(&segment_id) {
            Some(bf) => bf.mean(),
            None => 0.5,
        }
    }

    /// Compute a confidence adjustment based on feedback.
    /// Returns a value to ADD to the segment's base confidence.
    /// Backwards-compatible API — internally uses Bayesian estimation.
    pub fn confidence_adjustment(&self, segment_id: SegmentId) -> f32 {
        match self.feedback.get(&segment_id) {
            Some(bf) => {
                // Map Bayesian mean [0, 1] to adjustment range [-0.3, +0.3]
                // More observations = larger possible adjustment
                let mean = bf.mean();
                let weight = (bf.observation_count() / 10.0).min(1.0);
                (mean - 0.5) * 0.6 * weight
            }
            None => 0.0,
        }
    }

    /// Sync tracker state to a segment's alpha/beta fields.
    /// Call this to push accumulated feedback into the segment struct.
    pub fn sync_to_segment(&self, segment: &mut Segment) {
        if let Some(bf) = self.feedback.get(&segment.id) {
            segment.alpha = bf.alpha;
            segment.beta = bf.beta;
            segment.confidence = bf.mean();
        }
    }

    /// Pull a segment's alpha/beta into the tracker.
    /// Call this when loading segments that may have been updated externally.
    pub fn sync_from_segment(&mut self, segment: &Segment) {
        if segment.alpha != 1.0 || segment.beta != 1.0 {
            self.feedback.insert(
                segment.id,
                BayesianFeedback {
                    alpha: segment.alpha,
                    beta: segment.beta,
                },
            );
        }
    }

    /// Remove feedback data for a segment (e.g., after deletion).
    pub fn remove(&mut self, segment_id: SegmentId) {
        self.feedback.remove(&segment_id);
    }

    /// Number of segments with feedback data.
    pub fn tracked_count(&self) -> usize {
        self.feedback.len()
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

    /// Load from disk, with automatic migration from V1 format.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let data = std::fs::read(path)?;

        // Try V2 (current) format first
        if let Ok(tracker) = bincode::deserialize::<Self>(&data) {
            return Ok(tracker);
        }

        // Try legacy V1 format and migrate
        if let Ok(legacy) = bincode::deserialize::<LegacyTracker>(&data) {
            tracing::info!(
                "Migrating QualityTracker from V1 to V2 ({} entries)",
                legacy.feedback.len()
            );
            let mut tracker = Self::new();
            for (id, (acceptances, corrections)) in legacy.feedback {
                tracker.feedback.insert(
                    id,
                    BayesianFeedback {
                        alpha: 1.0 + acceptances as f32,
                        beta: 1.0 + corrections as f32,
                    },
                );
            }
            // Re-save in new format
            if let Err(e) = tracker.save(path) {
                tracing::warn!("Failed to save migrated quality tracker: {e}");
            }
            return Ok(tracker);
        }

        Err(AnimusError::Storage(
            "failed to load quality tracker: unrecognized format".to_string(),
        ))
    }
}

/// Compute the effective confidence for a segment, combining Bayesian
/// confidence with temporal decay.
pub fn effective_confidence(segment: &Segment) -> f32 {
    let bayesian = segment.bayesian_confidence();
    let decay = segment.temporal_decay_factor();
    // Decay reduces confidence over time, but never below a floor
    // to prevent old-but-valid knowledge from being completely discarded
    let floor = 0.1;
    (bayesian * decay).max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::segment::DecayClass;
    use animus_core::{Content, Source};

    fn test_segment() -> Segment {
        Segment::new(
            Content::Text("test".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::Manual {
                description: "test".to_string(),
            },
        )
    }

    #[test]
    fn test_acceptance_boosts_confidence() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_acceptance(id);
        tracker.record_acceptance(id);
        tracker.record_acceptance(id);

        let conf = tracker.bayesian_confidence(id);
        assert!(conf > 0.5, "3 acceptances should boost above 0.5, got {conf}");
        // Beta(4, 1) -> mean = 4/5 = 0.8
        assert!((conf - 0.8).abs() < 0.01, "expected ~0.8, got {conf}");
    }

    #[test]
    fn test_corrections_reduce_confidence() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_correction(id);
        tracker.record_correction(id);
        tracker.record_correction(id);

        let conf = tracker.bayesian_confidence(id);
        assert!(conf < 0.5, "3 corrections should reduce below 0.5, got {conf}");
        // Beta(1, 4) -> mean = 1/5 = 0.2
        assert!((conf - 0.2).abs() < 0.01, "expected ~0.2, got {conf}");
    }

    #[test]
    fn test_mixed_feedback() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_acceptance(id);
        tracker.record_correction(id);

        let conf = tracker.bayesian_confidence(id);
        // Beta(2, 2) -> mean = 0.5
        assert!(
            (conf - 0.5).abs() < 0.01,
            "equal accept/correct should be ~0.5, got {conf}"
        );
    }

    #[test]
    fn test_bayesian_feedback_variance() {
        let mut tracker = QualityTracker::new();
        let id1 = SegmentId::new();
        let id2 = SegmentId::new();

        // Few observations = high variance
        tracker.record_acceptance(id1);
        let var1 = tracker.get_feedback(id1).unwrap().variance();

        // Many observations = low variance
        for _ in 0..20 {
            tracker.record_acceptance(id2);
        }
        let var2 = tracker.get_feedback(id2).unwrap().variance();

        assert!(
            var2 < var1,
            "more observations should give lower variance: {var2} >= {var1}"
        );
    }

    #[test]
    fn test_confidence_adjustment_scales_with_observations() {
        let mut tracker = QualityTracker::new();
        let id1 = SegmentId::new();
        let id2 = SegmentId::new();

        // 1 acceptance: small adjustment
        tracker.record_acceptance(id1);
        let adj1 = tracker.confidence_adjustment(id1);

        // 10 acceptances: larger adjustment
        for _ in 0..10 {
            tracker.record_acceptance(id2);
        }
        let adj2 = tracker.confidence_adjustment(id2);

        assert!(
            adj2 > adj1,
            "more observations should give larger adjustment: {adj2} <= {adj1}"
        );
        assert!(adj1 > 0.0, "acceptance should give positive adjustment");
        assert!(adj2 > 0.0, "acceptances should give positive adjustment");
    }

    #[test]
    fn test_sync_to_segment() {
        let mut tracker = QualityTracker::new();
        let mut seg = test_segment();

        tracker.record_acceptance(seg.id);
        tracker.record_acceptance(seg.id);

        tracker.sync_to_segment(&mut seg);

        assert!((seg.alpha - 3.0).abs() < 0.01); // 1.0 prior + 2 acceptances
        assert!((seg.beta - 1.0).abs() < 0.01); // 1.0 prior, no corrections
        assert!((seg.confidence - 0.75).abs() < 0.01); // 3/4
    }

    #[test]
    fn test_segment_bayesian_confidence() {
        let mut seg = test_segment();
        assert!((seg.bayesian_confidence() - 0.5).abs() < 0.01); // uniform prior

        seg.record_positive_feedback();
        seg.record_positive_feedback();
        // alpha=3, beta=1 -> 3/4 = 0.75
        assert!((seg.bayesian_confidence() - 0.75).abs() < 0.01);
        assert!((seg.confidence - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_segment_negative_feedback() {
        let mut seg = test_segment();
        seg.record_negative_feedback();
        seg.record_negative_feedback();
        seg.record_negative_feedback();
        // alpha=1, beta=4 -> 1/5 = 0.2
        assert!((seg.bayesian_confidence() - 0.2).abs() < 0.01);
        assert!((seg.confidence - 0.2).abs() < 0.01);
    }

    #[test]
    fn test_temporal_decay_brand_new() {
        let seg = test_segment();
        let decay = seg.temporal_decay_factor();
        // Brand new segment should have decay very close to 1.0
        assert!(
            decay > 0.99,
            "brand new segment should have decay ~1.0, got {decay}"
        );
    }

    #[test]
    fn test_temporal_decay_class_half_lives() {
        // Factual should decay slower than Opinion
        let factual_hl = DecayClass::Factual.half_life_secs();
        let opinion_hl = DecayClass::Opinion.half_life_secs();
        assert!(factual_hl > opinion_hl);

        // General == Procedural
        let general_hl = DecayClass::General.half_life_secs();
        let procedural_hl = DecayClass::Procedural.half_life_secs();
        assert!((general_hl - procedural_hl).abs() < 1.0);
    }

    #[test]
    fn test_health_score_range() {
        let seg = test_segment();
        let score = seg.health_score();
        assert!(
            (0.0..=1.0).contains(&score),
            "health score should be in [0, 1], got {score}"
        );
    }

    #[test]
    fn test_health_score_improves_with_acceptance() {
        let mut seg = test_segment();
        let baseline = seg.health_score();

        for _ in 0..5 {
            seg.record_positive_feedback();
        }
        let improved = seg.health_score();

        assert!(
            improved > baseline,
            "acceptances should improve health: {improved} <= {baseline}"
        );
    }

    #[test]
    fn test_effective_confidence_floor() {
        let mut seg = test_segment();
        // Simulate a very old segment by setting created far in the past
        seg.created = chrono::Utc::now() - chrono::Duration::days(365);
        seg.decay_class = DecayClass::Opinion; // fast decay

        let eff = effective_confidence(&seg);
        assert!(
            eff >= 0.1,
            "effective confidence should never go below floor (0.1), got {eff}"
        );
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("quality.bin");

        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();
        tracker.record_acceptance(id);
        tracker.record_acceptance(id);
        tracker.record_correction(id);
        tracker.save(&path).unwrap();

        let loaded = QualityTracker::load(&path).unwrap();
        let conf = loaded.bayesian_confidence(id);
        // Beta(3, 2) -> 3/5 = 0.6
        assert!((conf - 0.6).abs() < 0.01);
    }
}
