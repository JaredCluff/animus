// crates/animus-core/src/provider_catalog.rs
use crate::model_capability::{ModelCapabilityProfile, PrefillSpeed, ProfileSource, ReasoningSupport};
use crate::provider_meta::{CostTier, DataPolicy, OwnershipRisk, ProviderTrustProfile, QualityTier, SpeedTier};
use std::collections::HashMap;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Static catalog of known LLM providers with pre-evaluated trust profiles.
/// Used by SmartRouter at startup and by TrustEvaluator as a fallback seed.
pub fn known_providers() -> Vec<ProviderTrustProfile> {
    vec![
        ProviderTrustProfile {
            provider_id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::NoRetention,
            effective_trust: 3,
            notes: "US AI safety company; API calls not used for training by default.".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "cerebras".to_string(),
            display_name: "Cerebras Systems".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::ShortWindow,
            effective_trust: 3,
            notes: "US hardware/inference company; free tier available.".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "groq".to_string(),
            display_name: "Groq".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::ShortWindow,
            effective_trust: 3,
            notes: "US inference hardware company; free tier available.".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "ollama".to_string(),
            display_name: "Ollama (local)".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::NoRetention,
            effective_trust: 3,
            notes: "Local inference — no data leaves the host. Weights may be from any origin.".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "nim".to_string(),
            display_name: "NVIDIA NIM".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::ShortWindow,
            effective_trust: 3,
            notes: "NVIDIA inference cloud; OpenAI-compatible API; $25 free credits tier.".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "openrouter".to_string(),
            display_name: "OpenRouter".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::ShortWindow,
            effective_trust: 2,
            notes: "US-based API aggregator; routes to many providers; free tier models available.".to_string(),
        },
        // ── Prohibited ───────────────────────────────────────────────────────
        // PRC National Intelligence Law 2017 requires entities to cooperate with
        // state intelligence. This applies to API endpoints regardless of model quality.
        // Running the same weights locally via Ollama is clean — only the API is prohibited.
        ProviderTrustProfile {
            provider_id: "qwen-api".to_string(),
            display_name: "Qwen API (Alibaba Cloud)".to_string(),
            hq_country: "CN".to_string(),
            ownership_risk: OwnershipRisk::Prohibited,
            data_policy: DataPolicy::Retained,
            effective_trust: 0,
            notes: "PRC National Intelligence Law 2017 — prohibited unconditionally.".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "deepseek-api".to_string(),
            display_name: "DeepSeek API".to_string(),
            hq_country: "CN".to_string(),
            ownership_risk: OwnershipRisk::Prohibited,
            data_policy: DataPolicy::Retained,
            effective_trust: 0,
            notes: "PRC jurisdiction — same prohibition as Qwen API.".to_string(),
        },
    ]
}

/// Build a lookup map from provider_id to trust profile.
pub fn provider_trust_map() -> std::collections::HashMap<String, ProviderTrustProfile> {
    known_providers()
        .into_iter()
        .map(|p| (p.provider_id.clone(), p))
        .collect()
}

/// A provider entry in providers.json — written by AccountRegistrar, read by ProvidersJsonWatcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub provider_id: String,
    pub display_name: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<ProviderModelEntry>,
    pub trust: ProviderTrustProfile,
    pub registered_at: DateTime<Utc>,
    pub registration_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModelEntry {
    pub model_id: String,
    pub cost_tier: CostTier,
    pub speed_tier: SpeedTier,
    pub quality_tier: QualityTier,
}

/// Load and deserialize providers.json, returning an empty vec on any error.
pub fn load_providers_json(path: &std::path::Path) -> Vec<ProviderEntry> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Static capability profiles for known cloud models.
/// Used by `CapabilityRegistry` at startup; enriched by Ollama probes for local models.
/// Prohibited providers (PRC/Russia) are never included here.
pub fn known_model_profiles() -> HashMap<String, ModelCapabilityProfile> {
    use chrono::NaiveDate;
    use crate::provider_meta::{CostTier, DataPolicy};

    let mut m: HashMap<String, ModelCapabilityProfile> = HashMap::new();

    macro_rules! add {
        ($key:expr, $val:expr) => { m.insert($key.to_string(), $val); };
    }

    // ── Anthropic ────────────────────────────────────────────────────────────
    add!("anthropic:claude-opus-4-6", ModelCapabilityProfile {
        provider: "anthropic".to_string(), model_id: "claude-opus-4-6".to_string(),
        parameter_count_b: Some(200.0),
        release_date: Some(NaiveDate::from_ymd_opt(2025, 10, 1).unwrap()),
        context_window: Some(200_000),
        reasoning_support: ReasoningSupport::ExtendedThinking { max_budget_tokens: 32_000 },
        generation_tok_per_sec: Some(80.0), prefill_speed: PrefillSpeed::Fast,
        rate_limit_rpm_ceiling: Some(50), rate_limit_tpd_ceiling: Some(5_000_000),
        cost_tier: CostTier::Expensive, cost_per_mtok_input: Some(15.0), cost_per_mtok_output: Some(75.0),
        trust_score: 3, data_policy: DataPolicy::NoRetention,
        profile_source: ProfileSource::StaticRegistry,
    });
    add!("anthropic:claude-sonnet-4-6", ModelCapabilityProfile {
        provider: "anthropic".to_string(), model_id: "claude-sonnet-4-6".to_string(),
        parameter_count_b: Some(70.0),
        release_date: Some(NaiveDate::from_ymd_opt(2025, 10, 1).unwrap()),
        context_window: Some(200_000),
        reasoning_support: ReasoningSupport::ExtendedThinking { max_budget_tokens: 16_000 },
        generation_tok_per_sec: Some(120.0), prefill_speed: PrefillSpeed::Fast,
        rate_limit_rpm_ceiling: Some(50), rate_limit_tpd_ceiling: Some(10_000_000),
        cost_tier: CostTier::Moderate, cost_per_mtok_input: Some(3.0), cost_per_mtok_output: Some(15.0),
        trust_score: 3, data_policy: DataPolicy::NoRetention,
        profile_source: ProfileSource::StaticRegistry,
    });
    add!("anthropic:claude-haiku-4-5-20251001", ModelCapabilityProfile {
        provider: "anthropic".to_string(), model_id: "claude-haiku-4-5-20251001".to_string(),
        parameter_count_b: Some(20.0),
        release_date: Some(NaiveDate::from_ymd_opt(2025, 10, 1).unwrap()),
        context_window: Some(200_000),
        reasoning_support: ReasoningSupport::None,
        generation_tok_per_sec: Some(200.0), prefill_speed: PrefillSpeed::Fast,
        rate_limit_rpm_ceiling: Some(50), rate_limit_tpd_ceiling: Some(25_000_000),
        cost_tier: CostTier::Cheap, cost_per_mtok_input: Some(0.8), cost_per_mtok_output: Some(4.0),
        trust_score: 3, data_policy: DataPolicy::NoRetention,
        profile_source: ProfileSource::StaticRegistry,
    });

    // ── Cerebras (free tier — custom WSE silicon, ~3000 tok/s) ───────────────
    add!("cerebras:llama3.1-8b", ModelCapabilityProfile {
        provider: "cerebras".to_string(), model_id: "llama3.1-8b".to_string(),
        parameter_count_b: Some(8.0),
        release_date: Some(NaiveDate::from_ymd_opt(2024, 7, 1).unwrap()),
        context_window: Some(128_000),
        reasoning_support: ReasoningSupport::None,
        generation_tok_per_sec: Some(3000.0), prefill_speed: PrefillSpeed::Instant,
        rate_limit_rpm_ceiling: Some(30), rate_limit_tpd_ceiling: Some(1_000_000),
        cost_tier: CostTier::Free, cost_per_mtok_input: Some(0.0), cost_per_mtok_output: Some(0.0),
        trust_score: 3, data_policy: DataPolicy::ShortWindow,
        profile_source: ProfileSource::StaticRegistry,
    });
    add!("cerebras:llama3.3-70b", ModelCapabilityProfile {
        provider: "cerebras".to_string(), model_id: "llama3.3-70b".to_string(),
        parameter_count_b: Some(70.0),
        release_date: Some(NaiveDate::from_ymd_opt(2024, 12, 1).unwrap()),
        context_window: Some(128_000),
        reasoning_support: ReasoningSupport::None,
        generation_tok_per_sec: Some(2100.0), prefill_speed: PrefillSpeed::Instant,
        rate_limit_rpm_ceiling: Some(30), rate_limit_tpd_ceiling: Some(1_000_000),
        cost_tier: CostTier::Free, cost_per_mtok_input: Some(0.0), cost_per_mtok_output: Some(0.0),
        trust_score: 3, data_policy: DataPolicy::ShortWindow,
        profile_source: ProfileSource::StaticRegistry,
    });
    // Qwen3.5-32b on Cerebras: speed AND extended thinking
    add!("cerebras:qwen3.5-32b", ModelCapabilityProfile {
        provider: "cerebras".to_string(), model_id: "qwen3.5-32b".to_string(),
        parameter_count_b: Some(32.0),
        release_date: Some(NaiveDate::from_ymd_opt(2025, 9, 1).unwrap()),
        context_window: Some(32_000),
        reasoning_support: ReasoningSupport::ExtendedThinking { max_budget_tokens: 16_384 },
        generation_tok_per_sec: Some(1500.0), prefill_speed: PrefillSpeed::Instant,
        rate_limit_rpm_ceiling: Some(30), rate_limit_tpd_ceiling: Some(1_000_000),
        cost_tier: CostTier::Free, cost_per_mtok_input: Some(0.0), cost_per_mtok_output: Some(0.0),
        trust_score: 3, data_policy: DataPolicy::ShortWindow,
        profile_source: ProfileSource::StaticRegistry,
    });

    // ── Groq (free tier — LPU inference, ~800 tok/s) ─────────────────────────
    add!("groq:llama3.1-8b-instant", ModelCapabilityProfile {
        provider: "groq".to_string(), model_id: "llama3.1-8b-instant".to_string(),
        parameter_count_b: Some(8.0),
        release_date: Some(NaiveDate::from_ymd_opt(2024, 7, 1).unwrap()),
        context_window: Some(128_000),
        reasoning_support: ReasoningSupport::None,
        generation_tok_per_sec: Some(800.0), prefill_speed: PrefillSpeed::Instant,
        rate_limit_rpm_ceiling: Some(30), rate_limit_tpd_ceiling: Some(500_000),
        cost_tier: CostTier::Free, cost_per_mtok_input: Some(0.0), cost_per_mtok_output: Some(0.0),
        trust_score: 3, data_policy: DataPolicy::ShortWindow,
        profile_source: ProfileSource::StaticRegistry,
    });
    add!("groq:llama3.3-70b-versatile", ModelCapabilityProfile {
        provider: "groq".to_string(), model_id: "llama3.3-70b-versatile".to_string(),
        parameter_count_b: Some(70.0),
        release_date: Some(NaiveDate::from_ymd_opt(2024, 12, 1).unwrap()),
        context_window: Some(128_000),
        reasoning_support: ReasoningSupport::None,
        generation_tok_per_sec: Some(400.0), prefill_speed: PrefillSpeed::Instant,
        rate_limit_rpm_ceiling: Some(30), rate_limit_tpd_ceiling: Some(100_000),
        cost_tier: CostTier::Free, cost_per_mtok_input: Some(0.0), cost_per_mtok_output: Some(0.0),
        trust_score: 3, data_policy: DataPolicy::ShortWindow,
        profile_source: ProfileSource::StaticRegistry,
    });

    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_meta::OwnershipRisk;

    #[test]
    fn anthropic_is_clean() {
        let map = provider_trust_map();
        assert_eq!(map["anthropic"].ownership_risk, OwnershipRisk::Clean);
        assert_eq!(map["anthropic"].effective_trust, 3);
    }

    #[test]
    fn prc_providers_prohibited() {
        let map = provider_trust_map();
        assert_eq!(map["qwen-api"].ownership_risk, OwnershipRisk::Prohibited);
        assert_eq!(map["qwen-api"].effective_trust, 0);
        assert_eq!(map["deepseek-api"].ownership_risk, OwnershipRisk::Prohibited);
    }

    #[test]
    fn no_duplicate_provider_ids() {
        let providers = known_providers();
        let mut ids = std::collections::HashSet::new();
        for p in &providers {
            assert!(ids.insert(p.provider_id.as_str()), "duplicate provider_id: {}", p.provider_id);
        }
    }

    #[test]
    fn known_model_profiles_has_expected_keys() {
        let profiles = known_model_profiles();
        assert!(profiles.contains_key("anthropic:claude-opus-4-6"), "missing claude-opus-4-6");
        assert!(profiles.contains_key("anthropic:claude-sonnet-4-6"), "missing claude-sonnet-4-6");
        assert!(profiles.contains_key("cerebras:llama3.1-8b"), "missing cerebras llama3.1-8b");
        assert!(profiles.contains_key("cerebras:llama3.3-70b"), "missing cerebras llama3.3-70b");
        assert!(profiles.contains_key("groq:llama3.1-8b-instant"), "missing groq llama3.1-8b");
        assert!(profiles.contains_key("groq:llama3.3-70b-versatile"), "missing groq llama3.3-70b");
    }

    #[test]
    fn no_prohibited_in_known_profiles() {
        let profiles = known_model_profiles();
        for (key, p) in &profiles {
            assert!(p.trust_score > 0, "profile {key} has trust_score 0 — prohibited models must not be in known_model_profiles");
        }
    }

    #[test]
    fn anthropic_profiles_have_extended_thinking() {
        use crate::model_capability::ReasoningSupport;
        let profiles = known_model_profiles();
        let opus = profiles.get("anthropic:claude-opus-4-6").unwrap();
        assert!(matches!(opus.reasoning_support, ReasoningSupport::ExtendedThinking { .. }));
        let sonnet = profiles.get("anthropic:claude-sonnet-4-6").unwrap();
        assert!(matches!(sonnet.reasoning_support, ReasoningSupport::ExtendedThinking { .. }));
        let haiku = profiles.get("anthropic:claude-haiku-4-5-20251001").unwrap();
        assert_eq!(haiku.reasoning_support, ReasoningSupport::None,
            "haiku does not have extended thinking");
    }

    #[test]
    fn cerebras_profiles_are_instant_and_free() {
        use crate::model_capability::PrefillSpeed;
        use crate::provider_meta::CostTier;
        let profiles = known_model_profiles();
        let c = profiles.get("cerebras:llama3.1-8b").unwrap();
        assert_eq!(c.prefill_speed, PrefillSpeed::Instant);
        assert_eq!(c.cost_tier, CostTier::Free);
    }
}
