//! OpenAI-compatible chat completions backend.
//!
//! Works with any endpoint that speaks the OpenAI `/v1/chat/completions` API:
//! - OpenAI (`https://api.openai.com`)
//! - Ollama (`http://127.0.0.1:11434`)
//! - LM Studio, vLLM, LocalAI, etc.

use animus_core::error::{AnimusError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ReasoningEngine, ReasoningOutput, Role, StopReason, ToolCall, ToolDefinition, Turn, TurnContent};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OaiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    max_tokens: usize,
    stream: bool,
    /// Ollama-specific: controls the KV-cache / context window size.
    /// Ignored by OpenAI (they don't error on unknown fields, but we skip
    /// serialising it when not set to stay compatible with strict validators).
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OaiToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: OaiFunction,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OaiFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct OaiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OaiToolFunction,
}

#[derive(Serialize)]
struct OaiToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize, Debug)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Usage,
}

#[derive(Deserialize, Debug)]
struct Choice {
    message: ChoiceMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OaiToolCall>>,
}

#[derive(Deserialize, Debug, Default)]
struct Usage {
    #[serde(default)]
    prompt_tokens: usize,
    #[serde(default)]
    completion_tokens: usize,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// OpenAI-compatible chat completions engine.
pub struct OpenAICompatEngine {
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: usize,
    /// Ollama `num_ctx` — KV-cache / context window size. `None` = server default.
    num_ctx: Option<usize>,
    /// Ollama `kv_cache_type` — KV-cache quantization ("q8_0", "q4_0", "f16").
    kv_cache_type: Option<String>,
    http: reqwest::Client,
}

impl OpenAICompatEngine {
    /// Create a new engine.
    ///
    /// - `base_url`: e.g. `"https://api.openai.com"` or `"http://127.0.0.1:11434"`
    /// - `api_key`: bearer token; pass `""` for unauthenticated (Ollama local)
    pub fn new(base_url: &str, api_key: &str, model: &str, max_tokens: usize) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| AnimusError::Llm(format!("openai-compat: failed to build HTTP client: {e}")))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            max_tokens,
            num_ctx: None,
            kv_cache_type: None,
            http,
        })
    }

    /// Convenience constructor for Ollama (no auth required).
    /// Sets `num_ctx = 32768` and `kv_cache_type = "q8_0"` (8-bit KV cache)
    /// so the full 32k context window fits comfortably in VRAM.
    pub fn for_ollama(ollama_url: &str, model: &str, max_tokens: usize) -> Result<Self> {
        let mut engine = Self::new(ollama_url, "", model, max_tokens)?;
        engine.num_ctx = Some(32_768);
        engine.kv_cache_type = Some("q8_0".to_string());
        Ok(engine)
    }

    /// Convenience constructor for OpenAI (requires API key).
    pub fn for_openai(api_key: &str, model: &str, max_tokens: usize) -> Result<Self> {
        Self::new("https://api.openai.com", api_key, model, max_tokens)
    }
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

fn turns_to_messages(system: &str, turns: &[Turn]) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(turns.len() + 1);

    if !system.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(Value::String(system.to_string())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    for turn in turns {
        match turn.role {
            Role::System => {
                let text = turn.content.iter().filter_map(|c| match c {
                    TurnContent::Text(t) => Some(t.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n");
                messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: Some(Value::String(text)),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            Role::User => {
                // Tool results become separate tool messages; plain text becomes user message.
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_results: Vec<(String, String)> = Vec::new(); // (tool_call_id, content)

                for c in &turn.content {
                    match c {
                        TurnContent::Text(t) => text_parts.push(t.clone()),
                        TurnContent::ToolResult { tool_use_id, content, .. } => {
                            tool_results.push((tool_use_id.clone(), content.clone()));
                        }
                        TurnContent::ToolUse { .. } => {} // shouldn't appear in user turns
                    }
                }

                if !text_parts.is_empty() {
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: Some(Value::String(text_parts.join("\n"))),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }

                for (tcid, content) in tool_results {
                    messages.push(ChatMessage {
                        role: "tool".to_string(),
                        content: Some(Value::String(content)),
                        tool_calls: None,
                        tool_call_id: Some(tcid),
                        name: None,
                    });
                }
            }
            Role::Assistant => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<OaiToolCall> = Vec::new();

                for c in &turn.content {
                    match c {
                        TurnContent::Text(t) => text_parts.push(t.clone()),
                        TurnContent::ToolUse { id, name, input } => {
                            tool_calls.push(OaiToolCall {
                                id: id.clone(),
                                kind: "function".to_string(),
                                function: OaiFunction {
                                    name: name.clone(),
                                    arguments: input.to_string(),
                                },
                            });
                        }
                        TurnContent::ToolResult { .. } => {}
                    }
                }

                let content = if text_parts.is_empty() {
                    None
                } else {
                    Some(Value::String(text_parts.join("\n")))
                };

                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content,
                    tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                    tool_call_id: None,
                    name: None,
                });
            }
        }
    }

    messages
}

fn tools_to_oai(tools: &[ToolDefinition]) -> Vec<OaiTool> {
    tools.iter().map(|t| OaiTool {
        kind: "function",
        function: OaiToolFunction {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        },
    }).collect()
}

// ---------------------------------------------------------------------------
// ReasoningEngine impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ReasoningEngine for OpenAICompatEngine {
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
        tools: Option<&[ToolDefinition]>,
    ) -> Result<ReasoningOutput> {
        let oai_messages = turns_to_messages(system, messages);
        let oai_tools = tools.map(tools_to_oai);
        let has_tools = oai_tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false);

        let options = match (self.num_ctx, self.kv_cache_type.as_deref()) {
            (Some(n), Some(kv)) => Some(serde_json::json!({"num_ctx": n, "kv_cache_type": kv})),
            (Some(n), None)     => Some(serde_json::json!({"num_ctx": n})),
            (None,    Some(kv)) => Some(serde_json::json!({"kv_cache_type": kv})),
            (None,    None)     => None,
        };

        let req = ChatRequest {
            model: &self.model,
            messages: oai_messages,
            tool_choice: if has_tools { Some("auto") } else { None },
            tools: oai_tools,
            max_tokens: self.max_tokens,
            stream: false,
            options,
        };

        let mut request = self.http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&req);

        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let resp = request
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("openai-compat: request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Llm(format!("openai-compat: {status}: {body}")));
        }

        let chat_resp: ChatResponse = resp
            .json()
            .await
            .map_err(|e| AnimusError::Llm(format!("openai-compat: response parse failed: {e}")))?;

        let choice = chat_resp.choices.into_iter().next()
            .ok_or_else(|| AnimusError::Llm("openai-compat: empty choices".to_string()))?;

        let content = choice.message.content.unwrap_or_default();
        let raw_tool_calls = choice.message.tool_calls.unwrap_or_default();

        let tool_calls: Vec<ToolCall> = raw_tool_calls.iter().map(|tc| {
            let input = serde_json::from_str::<Value>(&tc.function.arguments)
                .unwrap_or(Value::String(tc.function.arguments.clone()));
            ToolCall {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                input,
            }
        }).collect();

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            _ => StopReason::EndTurn,
        };

        tracing::debug!(
            model = %self.model,
            input_tokens = chat_resp.usage.prompt_tokens,
            output_tokens = chat_resp.usage.completion_tokens,
            tool_calls = tool_calls.len(),
            "openai-compat response"
        );

        Ok(ReasoningOutput {
            content,
            input_tokens: chat_resp.usage.prompt_tokens,
            output_tokens: chat_resp.usage.completion_tokens,
            tool_calls,
            stop_reason,
        })
    }

    fn context_limit(&self) -> usize {
        // Conservative default; Ollama and OpenAI models vary widely.
        // Overriding is not critical — the context assembler limits anyway.
        32_768
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
