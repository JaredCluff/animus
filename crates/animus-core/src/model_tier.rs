//! Model tier classification — assigns each engine to a quality/cost tier.
//!
//! Tiers are stable capability facts derived from provider identity and model size.
//! They do not change based on runtime health or rate limits — those are handled
//! separately by the routing layer.
//!
//! # Tier definitions
//! - **Tier1 (Premium)**: Anthropic Claude — full reasoning, extended thinking, proven tool use.
//! - **Tier2 (Fast/Quality)**: Cerebras (1500–3000 tok/s), Groq (400–800 tok/s), NIM ≥100B.
//! - **Tier3 (Free/Variable)**: OpenRouter free models, NIM <100B — good quality, variable availability.
//! - **Tier4 (Local)**: Ollama — private, zero cost, available whenever the host is running.

use crate::model_capability::ModelCapabilityProfile;
use serde::{Deserialize, Serialize};

/// Engine quality/cost tier for routing decisions.
///
/// Lower ordinal = higher preference under normal conditions.
/// The routing layer applies pressure- and class-specific adjustments on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ModelTier {
    /// Premium — Anthropic Claude.
    /// Full reasoning, extended thinking, proven tool use. Selected first for quality-critical tasks.
    Tier1 = 1,
    /// Fast/Quality — Cerebras, Groq, NIM ≥100B.
    /// Instant or fast prefill, quality free/cheap inference. Default first tier for most tasks.
    Tier2 = 2,
    /// Free/Variable — OpenRouter free models, NIM <100B.
    /// Good quality but availability varies. Used as backup when Tier1/2 are unavailable or over budget.
    Tier3 = 3,
    /// Local — Ollama.
    /// Private, zero cost, always available when the host machine is running.
    /// Required for Critical-sensitivity content. Last resort for cloud tasks.
    Tier4 = 4,
}

impl ModelTier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tier1 => "Premium",
            Self::Tier2 => "Fast/Quality",
            Self::Tier3 => "Free/Variable",
            Self::Tier4 => "Local",
        }
    }
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tier{} ({})", *self as u8, self.label())
    }
}

/// Derive the routing tier for a model from its capability profile.
///
/// This is a static capability fact — tier assignment does not change based on
/// runtime health, rate limits, or budget pressure. Those are applied by the
/// routing layer on top of the tier structure.
///
/// # NIM threshold
/// NIM models with ≥100B parameters → Tier2 (quality + speed on large hardware).
/// NIM models with <100B parameters → Tier3 (good but less reliable on complex tasks).
/// Unknown NIM parameter count → Tier3 (conservative default).
pub fn tier_from_profile(profile: &ModelCapabilityProfile) -> ModelTier {
    match profile.provider.as_str() {
        "anthropic" => ModelTier::Tier1,

        // Instant inference hardware — always Tier2 regardless of model size
        "cerebras" | "groq" => ModelTier::Tier2,

        // Local inference — always Tier4 regardless of model capability
        "ollama" => ModelTier::Tier4,

        // NIM: tier by parameter count (≥100B = large, quality inference)
        "nim" => match profile.parameter_count_b {
            Some(b) if b >= 100.0 => ModelTier::Tier2,
            _ => ModelTier::Tier3,
        },

        // OpenRouter aggregates many providers — treat as variable availability
        "openrouter" => ModelTier::Tier3,

        // Unknown providers: conservative default (variable availability)
        _ => ModelTier::Tier3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_capability::{ModelCapabilityProfile, PrefillSpeed, ProfileSource, ReasoningSupport};
    use crate::provider_meta::{CostTier, DataPolicy};

    fn profile(provider: &str, param_b: Option<f32>) -> ModelCapabilityProfile {
        ModelCapabilityProfile {
            provider: provider.to_string(),
            model_id: "test".to_string(),
            parameter_count_b: param_b,
            release_date: None,
            context_window: None,
            reasoning_support: ReasoningSupport::None,
            generation_tok_per_sec: None,
            prefill_speed: PrefillSpeed::Moderate,
            rate_limit_rpm_ceiling: None,
            rate_limit_tpd_ceiling: None,
            cost_tier: CostTier::Free,
            cost_per_mtok_input: None,
            cost_per_mtok_output: None,
            trust_score: 3,
            data_policy: DataPolicy::NoRetention,
            is_chat_model: true,
            supports_tool_use: true,
            profile_source: ProfileSource::Inferred,
        }
    }

    #[test]
    fn anthropic_is_tier1() {
        assert_eq!(tier_from_profile(&profile("anthropic", Some(200.0))), ModelTier::Tier1);
    }

    #[test]
    fn cerebras_and_groq_are_tier2() {
        assert_eq!(tier_from_profile(&profile("cerebras", Some(32.0))), ModelTier::Tier2);
        assert_eq!(tier_from_profile(&profile("groq", Some(70.0))), ModelTier::Tier2);
    }

    #[test]
    fn nim_large_is_tier2() {
        assert_eq!(tier_from_profile(&profile("nim", Some(405.0))), ModelTier::Tier2);
        assert_eq!(tier_from_profile(&profile("nim", Some(100.0))), ModelTier::Tier2);
    }

    #[test]
    fn nim_small_is_tier3() {
        assert_eq!(tier_from_profile(&profile("nim", Some(8.0))), ModelTier::Tier3);
        assert_eq!(tier_from_profile(&profile("nim", Some(70.0))), ModelTier::Tier3);
        assert_eq!(tier_from_profile(&profile("nim", None)), ModelTier::Tier3);
    }

    #[test]
    fn openrouter_is_tier3() {
        assert_eq!(tier_from_profile(&profile("openrouter", Some(70.0))), ModelTier::Tier3);
    }

    #[test]
    fn ollama_is_tier4() {
        assert_eq!(tier_from_profile(&profile("ollama", Some(35.0))), ModelTier::Tier4);
    }

    #[test]
    fn tier_ordering() {
        assert!(ModelTier::Tier1 < ModelTier::Tier2);
        assert!(ModelTier::Tier2 < ModelTier::Tier3);
        assert!(ModelTier::Tier3 < ModelTier::Tier4);
    }
}
