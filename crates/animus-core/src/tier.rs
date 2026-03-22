use serde::{Deserialize, Serialize};

/// Configuration for tier scoring weights and thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    /// Weight for relevance to active goals.
    pub w_relevance: f32,
    /// Weight for recency decay.
    pub w_recency: f32,
    /// Weight for access frequency.
    pub w_access_frequency: f32,
    /// Weight for confidence.
    pub w_confidence: f32,

    /// Score above which a segment is promoted to Warm.
    pub warm_threshold: f32,
    /// Score below which a segment is demoted to Cold (after delay).
    pub cold_threshold: f32,
    /// Minimum time in seconds below cold_threshold before demotion.
    pub cold_delay_secs: u64,

    /// Maximum age in seconds for recency decay (older = 0 contribution).
    pub recency_max_age_secs: u64,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            w_relevance: 0.4,
            w_recency: 0.25,
            w_access_frequency: 0.2,
            w_confidence: 0.15,
            warm_threshold: 0.4,
            cold_threshold: 0.2,
            cold_delay_secs: 3600,        // 1 hour
            recency_max_age_secs: 86400 * 7, // 7 days
        }
    }
}
