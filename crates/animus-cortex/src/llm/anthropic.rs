use animus_core::error::{AnimusError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{ReasoningEngine, ReasoningOutput, Role, StopReason, ToolCall, ToolDefinition, Turn, TurnContent};

/// How to authenticate with the Anthropic API.
#[derive(Clone)]
enum Auth {
    /// `x-api-key` header (standard purchased API key).
    ApiKey(String),
    /// `Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20` (OAuth token).
    OAuth(String),
}

/// Anthropic Claude API provider.
pub struct AnthropicEngine {
    client: reqwest::Client,
    auth: Auth,
    model: String,
    max_tokens: usize,
}

impl AnthropicEngine {
    pub fn new(api_key: String, model: String, max_tokens: usize) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build HTTP client"),
            auth: Auth::ApiKey(api_key),
            model,
            max_tokens,
        }
    }

    /// Create using an OAuth token (e.g. from Claude Code credentials).
    pub fn with_oauth(token: String, model: String, max_tokens: usize) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build HTTP client"),
            auth: Auth::OAuth(token),
            model,
            max_tokens,
        }
    }

    /// Create from environment variable ANTHROPIC_API_KEY.
    pub fn from_env(model: &str, max_tokens: usize) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            AnimusError::Llm("ANTHROPIC_API_KEY environment variable not set".to_string())
        })?;
        Ok(Self::new(api_key, model.to_string(), max_tokens))
    }

    /// Create from `ANTHROPIC_OAUTH_TOKEN` environment variable.
    pub fn from_oauth_env(model: &str, max_tokens: usize) -> Result<Self> {
        let token = std::env::var("ANTHROPIC_OAUTH_TOKEN").map_err(|_| {
            AnimusError::Llm("ANTHROPIC_OAUTH_TOKEN environment variable not set".to_string())
        })?;
        Ok(Self::with_oauth(token, model.to_string(), max_tokens))
    }

    /// Read OAuth token from Claude Code credentials file (`~/.claude/.credentials.json`).
    /// Returns `None` if the file doesn't exist or the token is not present.
    pub fn token_from_claude_code() -> Option<String> {
        let path = std::env::var("CLAUDE_CREDENTIALS_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .join(".claude/.credentials.json")
            });

        let data = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&data).ok()?;
        let token = v["claudeAiOauth"]["accessToken"].as_str()?.to_string();
        let expires_at = v["claudeAiOauth"]["expiresAt"].as_u64().unwrap_or(0);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if expires_at < now_ms.saturating_add(60_000) {
            return None; // Expired — caller should refresh
        }
        Some(token)
    }

    /// Try the best available auth: Claude Code OAuth → ANTHROPIC_OAUTH_TOKEN → ANTHROPIC_API_KEY.
    pub fn from_best_available(model: &str, max_tokens: usize) -> Result<Self> {
        if let Some(token) = Self::token_from_claude_code() {
            return Ok(Self::with_oauth(token, model.to_string(), max_tokens));
        }
        if let Ok(token) = std::env::var("ANTHROPIC_OAUTH_TOKEN") {
            return Ok(Self::with_oauth(token, model.to_string(), max_tokens));
        }
        Self::from_env(model, max_tokens)
    }
}

#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolDef>>,
}

#[derive(Serialize)]
struct ApiToolDef {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    usage: Usage,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: usize,
    output_tokens: usize,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

/// Convert a `Turn` into the JSON value expected by the Anthropic Messages API.
fn build_api_message(turn: &Turn) -> serde_json::Value {
    let role = match turn.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "user",
    };

    let content: Vec<serde_json::Value> = turn
        .content
        .iter()
        .map(|c| match c {
            TurnContent::Text(t) => serde_json::json!({
                "type": "text",
                "text": t,
            }),
            TurnContent::ToolUse { id, name, input } => serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
            TurnContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            }),
        })
        .collect();

    serde_json::json!({
        "role": role,
        "content": content,
    })
}

#[async_trait]
impl ReasoningEngine for AnthropicEngine {
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
        tools: Option<&[ToolDefinition]>,
    ) -> Result<ReasoningOutput> {
        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .filter(|t| t.role != Role::System)
            .map(build_api_message)
            .collect();

        let api_tools = tools.map(|t| {
            t.iter()
                .map(|td| ApiToolDef {
                    name: td.name.clone(),
                    description: td.description.clone(),
                    input_schema: td.input_schema.clone(),
                })
                .collect::<Vec<_>>()
        });

        let request = ApiRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: if system.is_empty() {
                None
            } else {
                Some(system.to_string())
            },
            messages: api_messages,
            tools: api_tools,
        };

        let mut builder = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        builder = match &self.auth {
            Auth::ApiKey(key) => builder.header("x-api-key", key),
            Auth::OAuth(token) => builder
                .header("Authorization", format!("Bearer {token}"))
                .header("anthropic-beta", "oauth-2025-04-20"),
        };

        let response = builder
            .json(&request)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("HTTP request failed: {e}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AnimusError::Llm(format!("failed to read response: {e}")))?;

        if !status.is_success() {
            let error_msg = serde_json::from_str::<ApiError>(&body)
                .map(|e| e.error.message)
                .unwrap_or(body);
            return Err(AnimusError::Llm(format!(
                "API error ({}): {error_msg}",
                status
            )));
        }

        let api_response: ApiResponse = serde_json::from_str(&body).map_err(|e| {
            AnimusError::Llm(format!(
                "failed to parse response: {e}\nbody: {}",
                &body[..body.len().min(500)]
            ))
        })?;

        let content = api_response
            .content
            .iter()
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("");

        let tool_calls: Vec<ToolCall> = api_response
            .content
            .iter()
            .filter(|b| b.block_type == "tool_use")
            .filter_map(|b| {
                Some(ToolCall {
                    id: b.id.clone()?,
                    name: b.name.clone()?,
                    input: b.input.clone().unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();

        let stop_reason = match api_response.stop_reason.as_deref() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            _ => StopReason::EndTurn,
        };

        Ok(ReasoningOutput {
            content,
            input_tokens: api_response.usage.input_tokens,
            output_tokens: api_response.usage.output_tokens,
            tool_calls,
            stop_reason,
        })
    }

    fn context_limit(&self) -> usize {
        200_000 // Claude context window
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_content_block_text_deserialization() {
        let json = r#"{"type": "text", "text": "Hello world"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert_eq!(block.block_type, "text");
        assert_eq!(block.text.as_deref(), Some("Hello world"));
        assert!(block.id.is_none());
    }

    #[test]
    fn test_api_content_block_tool_use_deserialization() {
        let json = r#"{
            "type": "tool_use",
            "id": "toolu_01A",
            "name": "read_file",
            "input": {"path": "/tmp/test.txt"}
        }"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        assert_eq!(block.block_type, "tool_use");
        assert_eq!(block.id.as_deref(), Some("toolu_01A"));
        assert_eq!(block.name.as_deref(), Some("read_file"));
        assert!(block.input.is_some());
    }

    #[test]
    fn test_api_response_with_tool_use_stop_reason() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Let me read that."},
                {"type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "/x"}}
            ],
            "usage": {"input_tokens": 100, "output_tokens": 50},
            "stop_reason": "tool_use"
        }"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn test_tool_definition_serialization() {
        let tool = ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path"}
                },
                "required": ["path"]
            }),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["name"], "read_file");
        assert!(json["input_schema"]["properties"]["path"].is_object());
    }

    #[test]
    fn test_turn_with_tool_result_serializes_for_api() {
        let turn = Turn {
            role: Role::User,
            content: vec![TurnContent::ToolResult {
                tool_use_id: "toolu_01".to_string(),
                content: "file contents here".to_string(),
                is_error: false,
            }],
        };
        let msg = build_api_message(&turn);
        assert_eq!(msg["role"], "user");
    }
}
