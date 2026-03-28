//! CapabilityRegistry — builds and holds ModelCapabilityProfile for all available models.
//!
//! Sources (in priority order):
//! 1. `known_model_profiles()` — static registry of well-known cloud models.
//! 2. Ollama `/api/show` probe — confirms parameter count and context window for local models.
//! 3. `infer_profile()` — heuristic fallback from model name for anything not in the above.
//!
//! Built once at startup. Read-only after construction — no locking needed.

use animus_core::model_capability::{
    ModelCapabilityProfile, PrefillSpeed, ProfileSource, ReasoningSupport,
};
use animus_core::provider_catalog::known_model_profiles;
use animus_core::provider_meta::{CostTier, DataPolicy};
use std::collections::HashMap;

/// Registry of model capability profiles.
/// Constructed at startup; shared as `Arc<CapabilityRegistry>`.
pub struct CapabilityRegistry {
    profiles: HashMap<String, ModelCapabilityProfile>,
}

impl CapabilityRegistry {
    /// Build the registry.
    ///
    /// - `ollama_base_url`: if Some, probes Ollama `/api/show` for each `ollama_models` entry.
    /// - `ollama_models`: model names from Ollama `/api/tags` (without the "ollama:" prefix).
    /// - `other_available`: additional "provider:model" strings (cloud models).
    pub async fn build(
        ollama_base_url: Option<&str>,
        ollama_models: &[String],
        other_available: &[String],
    ) -> Self {
        let mut profiles = known_model_profiles();

        // Enrich with Ollama probe for local models
        if let Some(base_url) = ollama_base_url {
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            for model in ollama_models {
                let key = format!("ollama:{model}");
                if profiles.contains_key(&key) {
                    continue; // static registry already has it
                }
                let probed = probe_ollama_model(&http, base_url, model).await;
                let profile = probed.unwrap_or_else(|| infer_profile("ollama", model));
                profiles.insert(key, profile);
            }
        } else {
            // No Ollama URL — infer profiles for all local models
            for model in ollama_models {
                let key = format!("ollama:{model}");
                if !profiles.contains_key(&key) {
                    profiles.insert(key, infer_profile("ollama", model));
                }
            }
        }

        // Infer profiles for any remaining unknown cloud models
        for spec in other_available {
            if profiles.contains_key(spec) {
                continue;
            }
            if let Some((provider, model)) = spec.split_once(':') {
                profiles.insert(spec.clone(), infer_profile(provider, model));
            }
        }

        Self { profiles }
    }

    /// Look up a profile by provider + model_id.
    pub fn get(&self, provider: &str, model: &str) -> Option<&ModelCapabilityProfile> {
        let key = format!("{provider}:{model}");
        self.profiles.get(&key)
    }

    /// All profiles in the registry.
    pub fn all(&self) -> &HashMap<String, ModelCapabilityProfile> {
        &self.profiles
    }

    /// Empty registry for tests. Unknown models will get inferred profiles at score time.
    pub fn empty() -> Self {
        Self { profiles: HashMap::new() }
    }
}

// ---------------------------------------------------------------------------
// Ollama probe
// ---------------------------------------------------------------------------

async fn probe_ollama_model(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> Option<ModelCapabilityProfile> {
    #[derive(serde::Deserialize)]
    struct ShowResp {
        details: Option<ShowDetails>,
    }
    #[derive(serde::Deserialize)]
    struct ShowDetails {
        parameter_size: Option<String>, // e.g. "35.1B"
        context_length: Option<u32>,
    }

    let url = format!("{base_url}/api/show");
    let body = serde_json::json!({ "name": model });
    let resp = http.post(&url).json(&body).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let show: ShowResp = resp.json().await.ok()?;
    let details = show.details?;

    let param_count = details
        .parameter_size
        .as_deref()
        .and_then(parse_param_size_str);

    Some(ModelCapabilityProfile {
        provider: "ollama".to_string(),
        model_id: model.to_string(),
        parameter_count_b: param_count,
        release_date: None,
        context_window: details.context_length,
        reasoning_support: detect_reasoning_support(model),
        generation_tok_per_sec: None,
        prefill_speed: PrefillSpeed::Slow,
        rate_limit_rpm_ceiling: None,
        rate_limit_tpd_ceiling: None,
        cost_tier: CostTier::Free,
        cost_per_mtok_input: Some(0.0),
        cost_per_mtok_output: Some(0.0),
        trust_score: 3,
        data_policy: DataPolicy::NoRetention,
        profile_source: ProfileSource::OllamaProbed,
    })
}

/// Parse Ollama `parameter_size` strings like "35.1B", "9B", "0.8B".
fn parse_param_size_str(s: &str) -> Option<f32> {
    let upper = s.to_uppercase();
    let num = upper.trim_end_matches('B').trim();
    num.parse::<f32>().ok()
}

// ---------------------------------------------------------------------------
// Inference from model name
// ---------------------------------------------------------------------------

/// Infer a capability profile from provider + model name heuristics.
/// Used when no static registry entry and no Ollama probe data is available.
pub(crate) fn infer_profile(provider: &str, model: &str) -> ModelCapabilityProfile {
    let param_count = extract_param_count_from_name(model);
    let reasoning = detect_reasoning_support(model);

    let (prefill_speed, generation_tok_per_sec, cost_tier) = match provider {
        "cerebras" => (PrefillSpeed::Instant, Some(2000.0_f32), CostTier::Free),
        "groq"     => (PrefillSpeed::Instant, Some(800.0_f32),  CostTier::Free),
        "anthropic" => (PrefillSpeed::Fast,   Some(100.0_f32),  CostTier::Expensive),
        "openrouter" => (PrefillSpeed::Fast,  None,             CostTier::Free),
        "nim"       => (PrefillSpeed::Fast,   None,             CostTier::Free),
        "ollama"    => (PrefillSpeed::Slow,   None,             CostTier::Free),
        _           => (PrefillSpeed::Moderate, None,           CostTier::Moderate),
    };

    let (trust_score, data_policy) = match provider {
        "ollama"     => (3_u8, DataPolicy::NoRetention),
        "anthropic"  => (3,    DataPolicy::NoRetention),
        "cerebras" | "groq" | "nim" | "openrouter" => (3, DataPolicy::ShortWindow),
        _            => (2,    DataPolicy::Unknown),
    };

    ModelCapabilityProfile {
        provider: provider.to_string(),
        model_id: model.to_string(),
        parameter_count_b: param_count,
        release_date: None,
        context_window: None,
        reasoning_support: reasoning,
        generation_tok_per_sec,
        prefill_speed,
        rate_limit_rpm_ceiling: None,
        rate_limit_tpd_ceiling: None,
        cost_tier,
        cost_per_mtok_input: None,
        cost_per_mtok_output: None,
        trust_score,
        data_policy,
        profile_source: ProfileSource::Inferred,
    }
}

/// Extract parameter count (in billions) from model name heuristics.
/// Recognises patterns: "35b", "9b", "0.8b", "70b", "120b" (case-insensitive).
pub(crate) fn extract_param_count_from_name(model: &str) -> Option<f32> {
    let lower = model.to_lowercase();
    for part in lower.split(|c: char| !c.is_alphanumeric() && c != '.') {
        if part.ends_with('b') && part.len() > 1 {
            let num_str = &part[..part.len() - 1];
            if let Ok(n) = num_str.parse::<f32>() {
                if n > 0.0 && n < 10_000.0 {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// Detect extended thinking support from model name.
pub(crate) fn detect_reasoning_support(model: &str) -> ReasoningSupport {
    let lower = model.to_lowercase();
    if lower.contains("qwen3") || lower.contains("qwq") || lower.contains("deepseek-r") {
        ReasoningSupport::ExtendedThinking { max_budget_tokens: 16_384 }
    } else {
        ReasoningSupport::None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_has_known_cloud_models_without_ollama() {
        let registry = CapabilityRegistry::build(None, &[], &[
            "anthropic:claude-opus-4-6".to_string(),
            "cerebras:llama3.1-8b".to_string(),
        ]).await;
        assert!(registry.get("anthropic", "claude-opus-4-6").is_some());
        assert!(registry.get("cerebras", "llama3.1-8b").is_some());
    }

    #[tokio::test]
    async fn unknown_model_gets_inferred_profile() {
        let registry = CapabilityRegistry::build(None, &[], &[
            "ollama:qwen3.5:35b".to_string(),
        ]).await;
        // Note: "ollama:qwen3.5:35b" splits on first ':' only — provider="ollama", model="qwen3.5:35b"
        let p = registry.get("ollama", "qwen3.5:35b");
        assert!(p.is_some(), "unknown model should get inferred profile");
        let p = p.unwrap();
        assert_eq!(p.profile_source, ProfileSource::Inferred);
        assert!(p.parameter_count_b.unwrap_or(0.0) > 30.0,
            "35b model should parse param_count > 30");
    }

    #[tokio::test]
    async fn inferred_cerebras_has_instant_speed_and_free_cost() {
        let registry = CapabilityRegistry::build(None, &[], &[
            "cerebras:some-new-model-72b".to_string(),
        ]).await;
        let p = registry.get("cerebras", "some-new-model-72b").unwrap();
        assert_eq!(p.prefill_speed, PrefillSpeed::Instant);
        assert_eq!(p.cost_tier, CostTier::Free);
    }

    #[test]
    fn detect_reasoning_qwen3() {
        let r = detect_reasoning_support("qwen3.5:35b");
        assert!(matches!(r, ReasoningSupport::ExtendedThinking { .. }));
    }

    #[test]
    fn detect_reasoning_llama_none() {
        assert_eq!(detect_reasoning_support("llama3.1-8b"), ReasoningSupport::None);
    }

    #[test]
    fn extract_param_count_35b() {
        let v = extract_param_count_from_name("qwen3.5:35b").unwrap();
        assert!((v - 35.0).abs() < 1.0);
    }

    #[test]
    fn extract_param_count_120b() {
        let v = extract_param_count_from_name("gpt-oss:120b").unwrap();
        assert!((v - 120.0).abs() < 1.0);
    }

    #[test]
    fn extract_param_count_unknown_returns_none() {
        assert_eq!(extract_param_count_from_name("mymodel"), None);
    }
}
