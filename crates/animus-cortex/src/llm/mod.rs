pub mod anthropic;
pub mod openai_compat;
pub use anthropic::AnthropicEngine;
pub use openai_compat::OpenAICompatEngine;

use animus_core::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Content within a single conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnContent {
    /// Plain text content.
    Text(String),
    /// A tool use request from the assistant.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// A tool result returned to the assistant.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

/// A single turn in a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub content: Vec<TurnContent>,
}

impl Turn {
    /// Convenience constructor for a simple text turn.
    pub fn text(role: Role, text: &str) -> Self {
        Self {
            role,
            content: vec![TurnContent::Text(text.to_string())],
        }
    }

    /// Extract the first text content block, if any.
    pub fn text_content(&self) -> Option<&str> {
        self.content.iter().find_map(|c| match c {
            TurnContent::Text(t) => Some(t.as_str()),
            _ => None,
        })
    }
}

/// Role in a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A tool call extracted from a reasoning response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// Normal end of turn.
    EndTurn,
    /// The model wants to invoke one or more tools.
    ToolUse,
    /// Hit the max_tokens limit.
    MaxTokens,
}

/// Definition of a tool the model may invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Output from a reasoning call.
#[derive(Debug, Clone)]
pub struct ReasoningOutput {
    pub content: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: StopReason,
}

/// Trait abstracting LLM providers.
#[async_trait]
pub trait ReasoningEngine: Send + Sync {
    /// Send a conversation and get a response.
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
        tools: Option<&[ToolDefinition]>,
    ) -> Result<ReasoningOutput>;

    /// Get the model's context window size in tokens.
    fn context_limit(&self) -> usize;

    /// Get the model identifier.
    fn model_name(&self) -> &str;

    /// Whether this engine supports the `/no_think` prefix for suppressing
    /// extended internal reasoning (Qwen3-style thinking models).
    /// When `true`, the thread layer may prepend `/no_think\n` to the user
    /// message to skip the thinking phase for simple inputs.
    fn supports_think_control(&self) -> bool {
        false
    }
}

/// Mock reasoning engine for testing.
pub struct MockEngine {
    response: String,
    context_limit: usize,
}

impl MockEngine {
    pub fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            context_limit: 8192,
        }
    }

    pub fn with_context_limit(mut self, limit: usize) -> Self {
        self.context_limit = limit;
        self
    }
}

#[async_trait]
impl ReasoningEngine for MockEngine {
    async fn reason(
        &self,
        _system: &str,
        _messages: &[Turn],
        _tools: Option<&[ToolDefinition]>,
    ) -> Result<ReasoningOutput> {
        Ok(ReasoningOutput {
            content: self.response.clone(),
            input_tokens: 100,
            output_tokens: self.response.len() / 4,
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        })
    }

    fn context_limit(&self) -> usize {
        self.context_limit
    }

    fn model_name(&self) -> &str {
        "mock-engine"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turn_text_convenience() {
        let turn = Turn::text(Role::User, "hello");
        assert_eq!(turn.role, Role::User);
        assert_eq!(turn.content.len(), 1);
        match &turn.content[0] {
            TurnContent::Text(t) => assert_eq!(t, "hello"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn test_turn_text_content() {
        let turn = Turn::text(Role::Assistant, "response");
        assert_eq!(turn.text_content(), Some("response"));
    }

    #[test]
    fn test_turn_tool_use_content() {
        let turn = Turn {
            role: Role::Assistant,
            content: vec![
                TurnContent::Text("I'll read that file.".to_string()),
                TurnContent::ToolUse {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "/tmp/test.txt"}),
                },
            ],
        };
        assert_eq!(turn.content.len(), 2);
        assert!(turn.text_content().is_some());
    }

    #[test]
    fn test_stop_reason_default() {
        let output = ReasoningOutput {
            content: "hello".to_string(),
            input_tokens: 10,
            output_tokens: 5,
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        };
        assert!(output.tool_calls.is_empty());
        assert_eq!(output.stop_reason, StopReason::EndTurn);
    }
}
