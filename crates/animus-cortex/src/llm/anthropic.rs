use animus_core::error::{AnimusError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{ReasoningEngine, ReasoningOutput, Role, StopReason, ToolCall, ToolDefinition, Turn, TurnContent};

const TOKEN_REFRESH_URL: &str = "https://console.anthropic.com/v1/oauth/token";
/// Claude Code's public OAuth client ID (the same one used by the Claude CLI).
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// How to authenticate with the Anthropic API.
#[derive(Clone)]
enum Auth {
    /// `x-api-key` header (standard purchased API key).
    ApiKey(String),
    /// `Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20` (static OAuth token).
    OAuth(String),
    /// Read OAuth token from Claude Code credentials file on every request, refreshing if expired.
    ClaudeCode,
}

// ---------------------------------------------------------------------------
// Claude Code credential types (for JSON parsing and refresh)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClaudeCodeCredentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: OAuthCredentials,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OAuthCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
    #[serde(rename = "expiresAt")]
    expires_at: u64, // Unix timestamp in milliseconds
}

impl OAuthCredentials {
    fn is_expired(&self) -> bool {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.expires_at < now_ms.saturating_add(60_000)
    }
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    grant_type: &'a str,
    refresh_token: &'a str,
    client_id: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

fn claude_credentials_path() -> PathBuf {
    std::env::var("CLAUDE_CREDENTIALS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".claude/.credentials.json")
        })
}

fn claude_credentials_exist() -> bool {
    claude_credentials_path().exists()
}

/// Get a valid OAuth token from the Claude Code credentials file.
/// Refreshes the token automatically if expired and saves the new credentials.
async fn get_claude_code_token(client: &reqwest::Client) -> Result<String> {
    let path = claude_credentials_path();
    let data = std::fs::read_to_string(&path).map_err(|e| {
        AnimusError::Llm(format!("cannot read Claude Code credentials at {}: {e}", path.display()))
    })?;
    let mut creds: ClaudeCodeCredentials = serde_json::from_str(&data).map_err(|e| {
        AnimusError::Llm(format!("failed to parse Claude Code credentials: {e}"))
    })?;

    if !creds.claude_ai_oauth.is_expired() {
        return Ok(creds.claude_ai_oauth.access_token.clone());
    }

    // Token expired — refresh it
    tracing::info!("OAuth access token expired — refreshing");
    let req = RefreshRequest {
        grant_type: "refresh_token",
        refresh_token: &creds.claude_ai_oauth.refresh_token,
        client_id: CLAUDE_CLIENT_ID,
    };
    let resp = client
        .post(TOKEN_REFRESH_URL)
        .json(&req)
        .send()
        .await
        .map_err(|e| AnimusError::Llm(format!("token refresh request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AnimusError::Llm(format!("token refresh failed ({status}): {body}")));
    }

    let refreshed: RefreshResponse = resp.json().await.map_err(|e| {
        AnimusError::Llm(format!("failed to parse token refresh response: {e}"))
    })?;

    let expires_in_ms = refreshed.expires_in.unwrap_or(3600) * 1000;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    creds.claude_ai_oauth.access_token = refreshed.access_token.clone();
    creds.claude_ai_oauth.expires_at = now_ms + expires_in_ms;
    if let Some(rt) = refreshed.refresh_token {
        creds.claude_ai_oauth.refresh_token = rt;
    }

    // Persist refreshed token
    if let Ok(updated) = serde_json::to_string_pretty(&creds) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &updated).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    tracing::info!("OAuth token refreshed successfully");
    Ok(refreshed.access_token)
}

/// Resolved auth — ready to apply as HTTP headers.
enum ResolvedAuth {
    ApiKey(String),
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

    /// Create using a static OAuth token.
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

    /// Create using Claude Code's stored credentials (auto-refreshes on expiry).
    pub fn from_claude_code(model: &str, max_tokens: usize) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build HTTP client"),
            auth: Auth::ClaudeCode,
            model: model.to_string(),
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

    /// Try the best available auth:
    /// `CLAUDE_CODE_OAUTH_TOKEN` → credentials file (with refresh) → `ANTHROPIC_OAUTH_TOKEN` → `ANTHROPIC_API_KEY`.
    ///
    /// `CLAUDE_CODE_OAUTH_TOKEN` is injected by the Claude Code CLI into child processes and is
    /// always fresh for the lifetime of the session — the cleanest path for container deployments
    /// launched from a Claude Code terminal.
    pub fn from_best_available(model: &str, max_tokens: usize) -> Result<Self> {
        // Claude Code CLI injects this per-session token automatically
        if let Ok(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
            if !token.is_empty() {
                return Ok(Self::with_oauth(token, model.to_string(), max_tokens));
            }
        }
        // Also handle the legacy typo variant
        if let Ok(token) = std::env::var("CLAUDE_CODE_OATH_TOKEN") {
            if !token.is_empty() {
                return Ok(Self::with_oauth(token, model.to_string(), max_tokens));
            }
        }
        // Credentials file: reads and refreshes the stored OAuth token automatically
        if claude_credentials_exist() {
            return Ok(Self::from_claude_code(model, max_tokens));
        }
        if let Ok(token) = std::env::var("ANTHROPIC_OAUTH_TOKEN") {
            return Ok(Self::with_oauth(token, model.to_string(), max_tokens));
        }
        Self::from_env(model, max_tokens)
    }

    /// Resolve the current auth to headers-ready values.
    async fn resolve_auth(&self) -> Result<ResolvedAuth> {
        match &self.auth {
            Auth::ApiKey(key) => Ok(ResolvedAuth::ApiKey(key.clone())),
            Auth::OAuth(token) => Ok(ResolvedAuth::OAuth(token.clone())),
            Auth::ClaudeCode => {
                let token = get_claude_code_token(&self.client).await?;
                Ok(ResolvedAuth::OAuth(token))
            }
        }
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

        let resolved = self.resolve_auth().await?;
        let mut builder = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        builder = match resolved {
            ResolvedAuth::ApiKey(key) => builder.header("x-api-key", key),
            ResolvedAuth::OAuth(token) => builder
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

        tracing::debug!(
            stop_reason = ?api_response.stop_reason,
            tool_calls = tool_calls.len(),
            input_tokens = api_response.usage.input_tokens,
            "LLM response"
        );

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
