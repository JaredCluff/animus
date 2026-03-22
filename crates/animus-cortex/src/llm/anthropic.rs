use animus_core::error::{AnimusError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{ReasoningEngine, ReasoningOutput, Role, Turn};

/// Anthropic Claude API provider.
pub struct AnthropicEngine {
    client: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: usize,
}

impl AnthropicEngine {
    pub fn new(api_key: String, model: String, max_tokens: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
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
}

#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ApiMessage>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    usage: Usage,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
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

#[async_trait]
impl ReasoningEngine for AnthropicEngine {
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
    ) -> Result<ReasoningOutput> {
        let api_messages: Vec<ApiMessage> = messages
            .iter()
            .filter(|t| t.role != Role::System)
            .map(|t| ApiMessage {
                role: match t.role {
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                    Role::System => unreachable!(),
                },
                content: t.content.clone(),
            })
            .collect();

        let request = ApiRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: if system.is_empty() {
                None
            } else {
                Some(system.to_string())
            },
            messages: api_messages,
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
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

        let api_response: ApiResponse = serde_json::from_str(&body)
            .map_err(|e| AnimusError::Llm(format!("failed to parse response: {e}")))?;

        let content = api_response
            .content
            .into_iter()
            .filter_map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");

        Ok(ReasoningOutput {
            content,
            input_tokens: api_response.usage.input_tokens,
            output_tokens: api_response.usage.output_tokens,
        })
    }

    fn context_limit(&self) -> usize {
        200_000 // Claude context window
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
