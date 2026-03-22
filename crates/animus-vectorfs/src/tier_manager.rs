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
            let segment = match self.store.get(id) {
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
                            let _ = self.store.set_tier(id, Tier::Cold);
                        }
                    }
                }
                Tier::Cold => {
                    if score >= self.config.warm_threshold {
                        tracing::debug!("promoting segment {} to Warm (score={score:.3})", id);
                        let _ = self.store.set_tier(id, Tier::Warm);
                    }
                }
                Tier::Hot => {} // filtered above
            }
        }
    }

    /// Compute the tier score for a segment.
    fn compute_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        let recency = self.recency_score(segment);
        let frequency = self.frequency_score(segment);

        self.config.w_relevance * segment.relevance_score
            + self.config.w_recency * recency
            + self.config.w_access_frequency * frequency
            + self.config.w_confidence * segment.confidence
    }

    /// Recency score: 1.0 for just accessed, decays to 0.0 at max age.
    fn recency_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        let age_secs = (Utc::now() - segment.last_accessed)
            .num_seconds()
            .max(0) as f64;
        let max = self.config.recency_max_age_secs as f64;
        (1.0 - (age_secs / max).min(1.0)) as f32
    }

    /// Frequency score: normalized access count. Saturates at 100 accesses.
    fn frequency_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        (segment.access_count as f32 / 100.0).min(1.0)
    }
}
