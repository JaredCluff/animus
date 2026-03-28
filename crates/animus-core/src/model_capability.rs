//! Rich per-model capability descriptor.
//!
//! Used by `ModelScorer` in `animus-cortex` to rank candidates without LLM involvement.
//! Constitution Principle 8: routing decisions cost zero tokens.
//! Constitution Principle 9: Animus derives its own routing from real capability data.

use crate::provider_meta::{CostTier, DataPolicy};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// How well a model supports extended reasoning/thinking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReasoningSupport {
    /// Standard autoregressive output; no special reasoning mechanism.
    None,
    /// Implicit chain-of-thought from RLHF/training; no special API parameter.
    ChainOfThought,
    /// Explicit extended thinking via API parameter.
    /// Covers Claude `budget_tokens`, Qwen3 `/think` prefix, DeepSeek-R1 `<think>` mode.
    ExtendedThinking {
        /// Maximum token budget available for the thinking block.
        max_budget_tokens: u32,
    },
}

/// Time-to-first-token characterization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefillSpeed {
    /// < 200 ms TTFT — custom inference silicon (Cerebras WSE, Groq LPU).
    Instant,
    /// 200 ms – 1 s TTFT — optimized cloud inference.
    Fast,
    /// 1 – 3 s TTFT — standard cloud or well-resourced local GPU.
    Moderate,
    /// > 3 s TTFT — large model on consumer GPU or CPU inference.
    Slow,
}

/// How the capability profile was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileSource {
    /// Hardcoded from provider documentation / known facts.
    StaticRegistry,
    /// Confirmed via Ollama `/api/show` at startup.
    OllamaProbed,
    /// Inferred from model name heuristics (size parsing, family detection).
    Inferred,
}

/// Rich capability descriptor for one model.
///
/// Combines static facts (parameter count, reasoning support) with probed measurements
/// (generation speed from Ollama) and provider metadata (cost, trust, rate limit ceiling).
///
/// `None` fields are always handled gracefully by `ModelScorer` — it applies conservative
/// defaults rather than failing. A profile with all `None` is still useful.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilityProfile {
    pub provider: String,
    pub model_id: String,

    // --- Static identity ---
    /// Approximate parameter count in billions. Used in quality scoring (log₂ scale).
    /// `None` → scored conservatively at 0.3 (below average assumption).
    pub parameter_count_b: Option<f32>,
    /// Release date. Used for recency decay — newer models score higher, all else equal.
    /// `None` → neutral recency (0.5).
    pub release_date: Option<NaiveDate>,
    /// Maximum context window in tokens.
    pub context_window: Option<u32>,

    // --- Reasoning capability ---
    pub reasoning_support: ReasoningSupport,

    // --- Speed ---
    /// Measured token generation rate in tokens/second.
    /// `None` → fall back to `prefill_speed` category for speed scoring.
    pub generation_tok_per_sec: Option<f32>,
    /// Time-to-first-token characterization.
    pub prefill_speed: PrefillSpeed,

    // --- Rate limit ceilings (provider tier / key-level) ---
    /// Maximum requests per minute on the current key/tier. Used in capacity scoring.
    pub rate_limit_rpm_ceiling: Option<u32>,
    /// Maximum tokens per day on the current key/tier.
    pub rate_limit_tpd_ceiling: Option<u64>,

    // --- Cost ---
    pub cost_tier: CostTier,
    /// Input cost in USD per million tokens. `None` → use `cost_tier` only.
    pub cost_per_mtok_input: Option<f32>,
    /// Output cost in USD per million tokens.
    pub cost_per_mtok_output: Option<f32>,

    // --- Trust ---
    /// From `ProviderTrustProfile.effective_trust` (0–3). 0 = prohibited; scores 0.0 always.
    pub trust_score: u8,
    pub data_policy: DataPolicy,

    /// How this profile was obtained.
    pub profile_source: ProfileSource,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn sample_profile() -> ModelCapabilityProfile {
        ModelCapabilityProfile {
            provider: "anthropic".to_string(),
            model_id: "claude-opus-4-6".to_string(),
            parameter_count_b: Some(200.0),
            release_date: Some(NaiveDate::from_ymd_opt(2025, 10, 1).unwrap()),
            context_window: Some(200_000),
            reasoning_support: ReasoningSupport::ExtendedThinking { max_budget_tokens: 32_000 },
            generation_tok_per_sec: Some(80.0),
            prefill_speed: PrefillSpeed::Fast,
            rate_limit_rpm_ceiling: Some(50),
            rate_limit_tpd_ceiling: Some(5_000_000),
            cost_tier: CostTier::Expensive,
            cost_per_mtok_input: Some(15.0),
            cost_per_mtok_output: Some(75.0),
            trust_score: 3,
            data_policy: DataPolicy::NoRetention,
            profile_source: ProfileSource::StaticRegistry,
        }
    }

    #[test]
    fn serde_roundtrip() {
        let p = sample_profile();
        let json = serde_json::to_string(&p).unwrap();
        let p2: ModelCapabilityProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn reasoning_support_extended_serde() {
        let r = ReasoningSupport::ExtendedThinking { max_budget_tokens: 16_000 };
        let json = serde_json::to_string(&r).unwrap();
        let r2: ReasoningSupport = serde_json::from_str(&json).unwrap();
        assert!(matches!(r2, ReasoningSupport::ExtendedThinking { max_budget_tokens: 16_000 }));
    }

    #[test]
    fn prefill_speed_instant_serde() {
        let p = PrefillSpeed::Instant;
        let json = serde_json::to_string(&p).unwrap();
        let p2: PrefillSpeed = serde_json::from_str(&json).unwrap();
        assert_eq!(p2, PrefillSpeed::Instant);
    }
}
