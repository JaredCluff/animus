use animus_core::segment::Tier;
use animus_core::tier::TierConfig;
use chrono::Utc;
use std::sync::Arc;

use crate::VectorStore;

/// Background tier manager that promotes/demotes segments based on scoring.
pub struct TierManager<S: VectorStore> {
    store: Arc<S>,
    config: TierConfig,
}

impl<S: VectorStore> TierManager<S> {
    pub fn new(store: Arc<S>, config: TierConfig) -> Self {
        Self { store, config }
    }

    /// Run one cycle of tier evaluation across all segments.
    pub fn run_cycle(&self) {
        let all_ids = self.store.segment_ids(None);

        for id in all_ids {
            let segment = match self.store.get_raw(id) {
                Ok(Some(s)) => s,
                _ => continue,
            };

            // Don't touch Hot segments — Mnemos manages those
            if segment.tier == Tier::Hot {
                continue;
            }

            let score = self.compute_score(&segment);

            match segment.tier {
                Tier::Warm => {
                    if score < self.config.cold_threshold {
                        let age_secs = (Utc::now() - segment.last_accessed)
                            .num_seconds()
                            .max(0) as u64;
                        if age_secs >= self.config.cold_delay_secs {
                            tracing::debug!("demoting segment {} to Cold (score={score:.3})", id);
                            if let Err(e) = self.store.set_tier(id, Tier::Cold) {
                                tracing::warn!("failed to demote segment {id}: {e}");
                            }
                        }
                    }
                }
                Tier::Cold => {
                    if score >= self.config.warm_threshold {
                        tracing::debug!("promoting segment {} to Warm (score={score:.3})", id);
                        if let Err(e) = self.store.set_tier(id, Tier::Warm) {
                            tracing::warn!("failed to promote segment {id}: {e}");
                        }
                    }
                }
                Tier::Hot => {} // filtered above
            }
        }
    }

    /// Compute the tier score for a segment.
    /// Uses Bayesian confidence and exponential temporal decay.
    fn compute_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        let recency = self.recency_score(segment);
        let frequency = self.frequency_score(segment);
        let confidence = segment.bayesian_confidence();
        let decay = segment.temporal_decay_factor();

        // Confidence is modulated by temporal decay — old knowledge
        // with high confidence still decays, but more slowly for Factual segments
        let effective_confidence = confidence * decay;

        self.config.w_relevance * segment.relevance_score
            + self.config.w_recency * recency
            + self.config.w_access_frequency * frequency
            + self.config.w_confidence * effective_confidence
    }

    /// Recency score based on last access time.
    /// Uses exponential decay matching the segment's decay class.
    fn recency_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        let age_secs = (Utc::now() - segment.last_accessed)
            .num_seconds()
            .max(0) as f64;
        let max = self.config.recency_max_age_secs as f64;
        // Exponential decay: faster initial drop, longer tail
        let lambda = (2.0_f64).ln() / (max / 2.0); // half-life at half the max age
        (-lambda * age_secs).exp() as f32
    }

    /// Frequency score: normalized access count. Saturates at 100 accesses.
    fn frequency_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        (segment.access_count as f32 / 100.0).min(1.0)
    }
}
