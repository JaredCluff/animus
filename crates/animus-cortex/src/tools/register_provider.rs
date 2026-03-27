use animus_core::provider_catalog::{load_providers_json, ProviderEntry, ProviderModelEntry};
use animus_core::provider_meta::{DataPolicy, OwnershipRisk, ProviderTrustProfile};
use animus_core::{CostTier, QualityTier, SpeedTier};
use crate::telos::Autonomy;
use crate::tools::{Tool, ToolContext, ToolResult};
use serde_json::Value;

pub struct RegisterProviderTool;

#[async_trait::async_trait]
impl Tool for RegisterProviderTool {
    fn name(&self) -> &str { "register_provider" }

    fn description(&self) -> &str {
        "Register a new LLM API provider. Appends to providers.json. \
         Prohibited providers (PRC/Russia jurisdiction) are rejected unconditionally. \
         The hot-reload watcher will pick up the new entry within 30 seconds."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "provider_id": {
                    "type": "string",
                    "description": "Unique lowercase identifier, e.g. 'groq'"
                },
                "display_name": { "type": "string" },
                "base_url": {
                    "type": "string",
                    "description": "OpenAI-compatible endpoint base URL"
                },
                "api_key": { "type": "string" },
                "models": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "model_id":     { "type": "string" },
                            "cost_tier":    { "type": "string", "enum": ["Free","Cheap","Moderate","Expensive"] },
                            "speed_tier":   { "type": "string", "enum": ["Fast","Medium","Slow"] },
                            "quality_tier": { "type": "string", "enum": ["High","Medium","Low"] }
                        },
                        "required": ["model_id","cost_tier","speed_tier","quality_tier"]
                    }
                },
                "hq_country": {
                    "type": "string",
                    "description": "ISO 3166-1 alpha-2 country code, e.g. 'US'"
                },
                "ownership_risk": {
                    "type": "string",
                    "enum": ["Clean","Minor","Major","Prohibited"]
                },
                "data_policy": {
                    "type": "string",
                    "enum": ["NoRetention","ShortWindow","Retained","Unknown"]
                },
                "notes": { "type": "string" }
            },
            "required": [
                "provider_id","display_name","base_url","api_key",
                "models","hq_country","ownership_risk","data_policy"
            ]
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }

    async fn execute(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let provider_id = params["provider_id"].as_str().unwrap_or("").to_string();

        let ownership_risk: OwnershipRisk = serde_json::from_value(
            Value::String(params["ownership_risk"].as_str().unwrap_or("").to_string())
        ).map_err(|e| format!("invalid ownership_risk: {e}"))?;

        if ownership_risk == OwnershipRisk::Prohibited {
            return Ok(ToolResult {
                content: format!(
                    "Refused: provider '{}' has OwnershipRisk::Prohibited. \
                     PRC/Russia-jurisdiction providers are unconditionally blocked.",
                    provider_id
                ),
                is_error: true,
            });
        }

        let data_policy: DataPolicy = serde_json::from_value(
            Value::String(params["data_policy"].as_str().unwrap_or("").to_string())
        ).map_err(|e| format!("invalid data_policy: {e}"))?;

        let effective_trust = ProviderTrustProfile::compute_effective_trust(ownership_risk, data_policy);

        let trust = ProviderTrustProfile {
            provider_id: provider_id.clone(),
            display_name: params["display_name"].as_str().unwrap_or("").to_string(),
            hq_country: params["hq_country"].as_str().unwrap_or("??").to_string(),
            ownership_risk,
            data_policy,
            effective_trust,
            notes: params["notes"].as_str().unwrap_or("").to_string(),
        };

        let models: Vec<ProviderModelEntry> = params["models"]
            .as_array()
            .ok_or("models must be an array")?
            .iter()
            .map(|m| {
                Ok(ProviderModelEntry {
                    model_id: m["model_id"].as_str().ok_or("missing model_id")?.to_string(),
                    cost_tier: serde_json::from_value::<CostTier>(m["cost_tier"].clone())
                        .map_err(|e: serde_json::Error| e.to_string())?,
                    speed_tier: serde_json::from_value::<SpeedTier>(m["speed_tier"].clone())
                        .map_err(|e: serde_json::Error| e.to_string())?,
                    quality_tier: serde_json::from_value::<QualityTier>(m["quality_tier"].clone())
                        .map_err(|e: serde_json::Error| e.to_string())?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let entry = ProviderEntry {
            provider_id: provider_id.clone(),
            display_name: params["display_name"].as_str().unwrap_or("").to_string(),
            base_url: params["base_url"].as_str().unwrap_or("").to_string(),
            api_key: params["api_key"].as_str().unwrap_or("").to_string(),
            models,
            trust,
            registered_at: chrono::Utc::now(),
            registration_source: "tool".to_string(),
        };

        // Register the path with the self-event filter before writing, so perception
        // doesn't trigger a feedback loop when the file change is detected.
        let path = ctx.data_dir.join("providers.json");
        if let Some(filter) = &ctx.self_event_filter {
            filter.register(path.to_string_lossy().to_string()).await;
        }

        // Load existing providers, upsert (remove old entry for same provider_id, append new).
        let mut providers = load_providers_json(&path);
        providers.retain(|p| p.provider_id != provider_id);
        providers.push(entry);

        // Atomic write: serialize → tmp file → rename.
        let json = serde_json::to_vec_pretty(&providers)
            .map_err(|e| format!("serialize error: {e}"))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write error: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename error: {e}"))?;

        Ok(ToolResult {
            content: format!(
                "Provider '{}' registered (effective_trust={}). \
                 The hot-reload watcher will pick it up within 30s.",
                provider_id, effective_trust
            ),
            is_error: false,
        })
    }
}
