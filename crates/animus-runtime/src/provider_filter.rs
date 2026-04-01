//! Per-provider model allow lists.
//!
//! Controls which discovered models are registered for each cloud provider.
//!
//! **If no allow list is configured for a provider, all discovered models pass.**
//! **If an allow list is configured, only explicitly listed models are registered.**
//!
//! # Sources (highest wins)
//! 1. `data_dir/provider_filters.json` — Animus-managed persistent overrides
//! 2. Env vars `ANIMUS_{PROVIDER_UPPER}_ALLOW=model1,model2,...`
//!
//! # File format
//! ```json
//! {
//!   "gemini": ["models/gemini-2.5-flash", "models/gemini-2.0-flash-lite"],
//!   "cerebras": ["qwen-3-235b-a22b-instruct-2507"]
//! }
//! ```
//! An empty array for a provider clears any env-var restriction for that provider.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Maps provider name → set of allowed model IDs.
/// If a provider is absent from the map, all its models are allowed.
pub type ProviderAllows = HashMap<String, HashSet<String>>;

/// Load provider allow lists from env vars and the override file.
///
/// `providers` is the list of known cloud provider names (lowercase).
/// Env var names are derived by uppercasing: `ANIMUS_{PROVIDER}_ALLOW`.
pub fn load_provider_allows(data_dir: &Path, providers: &[&str]) -> ProviderAllows {
    let mut allows: ProviderAllows = HashMap::new();

    // Step 1: env vars (lowest priority)
    for name in providers {
        let env_key = format!("ANIMUS_{}_ALLOW", name.to_uppercase().replace('-', "_"));
        if let Ok(val) = std::env::var(&env_key) {
            let models: HashSet<String> = val
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            if !models.is_empty() {
                tracing::info!("provider_filter: {name} allow list from env ({} model(s))", models.len());
                allows.insert(name.to_string(), models);
            }
        }
    }

    // Step 2: file overrides (highest priority — Animus's persistent preferences)
    let path = data_dir.join("provider_filters.json");
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(json) => {
                    if let Some(obj) = json.as_object() {
                        for (provider, models_val) in obj {
                            if let Some(arr) = models_val.as_array() {
                                let models: HashSet<String> = arr
                                    .iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect();
                                if models.is_empty() {
                                    // Empty array = lift restriction for this provider
                                    allows.remove(provider.as_str());
                                    tracing::info!("provider_filter: {provider} restriction cleared by file");
                                } else {
                                    tracing::info!(
                                        "provider_filter: {provider} allow list from file ({} model(s))",
                                        models.len()
                                    );
                                    allows.insert(provider.clone(), models);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("provider_filters.json parse error: {e} — using env-var filters only");
                }
            },
            Err(e) => {
                tracing::warn!("provider_filters.json read error: {e} — using env-var filters only");
            }
        }
    }

    allows
}

/// Returns `true` if `model` should be registered for `provider`.
///
/// If the provider has no entry in `allows`, all models are permitted.
pub fn is_allowed(allows: &ProviderAllows, provider: &str, model: &str) -> bool {
    match allows.get(provider) {
        Some(set) => set.contains(model),
        None => true,
    }
}
