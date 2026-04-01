use crate::telos::Autonomy;
use crate::tools::{Tool, ToolContext, ToolResult};
use serde_json::Value;

pub struct SetProviderFilterTool;

#[async_trait::async_trait]
impl Tool for SetProviderFilterTool {
    fn name(&self) -> &str { "set_provider_filter" }

    fn description(&self) -> &str {
        "Set an explicit allow list for a cloud provider's models. \
         If an allow list is configured for a provider, only listed models are registered \
         on the next startup — all others from that provider are ignored. \
         Pass an empty allow array to clear any restriction for that provider. \
         Changes are written to provider_filters.json and take effect on restart."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider name, e.g. 'gemini', 'groq', 'cerebras', 'nim', 'openrouter'"
                },
                "allow": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Model IDs to permit. Empty array removes the restriction for this provider."
                }
            },
            "required": ["provider", "allow"]
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }

    async fn execute(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let provider = params["provider"].as_str().unwrap_or("").to_string();
        if provider.is_empty() {
            return Ok(ToolResult {
                content: "provider is required".to_string(),
                is_error: true,
            });
        }

        let allow: Vec<String> = params["allow"]
            .as_array()
            .ok_or("allow must be an array")?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let path = ctx.data_dir.join("provider_filters.json");

        // Load existing filters so we only update the target provider
        let mut filters: serde_json::Map<String, Value> = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str::<Value>(&content)
                    .ok()
                    .and_then(|v| v.into_object())
                    .unwrap_or_default(),
                Err(_) => serde_json::Map::new(),
            }
        } else {
            serde_json::Map::new()
        };

        let action_desc = if allow.is_empty() {
            filters.remove(&provider);
            format!("restriction cleared — all discovered {provider} models will be registered on next restart")
        } else {
            filters.insert(
                provider.clone(),
                Value::Array(allow.iter().map(|m| Value::String(m.clone())).collect()),
            );
            format!(
                "allow list set to {} model(s): {}",
                allow.len(),
                allow.join(", ")
            )
        };

        // Register path with self-event filter to avoid perception feedback loop
        if let Some(filter) = &ctx.self_event_filter {
            filter.register(path.to_string_lossy().to_string()).await;
        }

        // Atomic write: serialize → tmp → rename
        let json = serde_json::to_vec_pretty(&Value::Object(filters))
            .map_err(|e| format!("serialize error: {e}"))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write error: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename error: {e}"))?;

        Ok(ToolResult {
            content: format!("Provider '{provider}': {action_desc}. Restart to apply."),
            is_error: false,
        })
    }
}

// serde_json::Value doesn't expose into_object() — add a local extension.
trait ValueExt {
    fn into_object(self) -> Option<serde_json::Map<String, Value>>;
}

impl ValueExt for Value {
    fn into_object(self) -> Option<serde_json::Map<String, Value>> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }
}
