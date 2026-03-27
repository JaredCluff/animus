// crates/animus-core/src/provider_catalog.rs
use crate::provider_meta::{CostTier, DataPolicy, OwnershipRisk, ProviderTrustProfile, QualityTier, SpeedTier};
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
}
