use animus_core::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single turn in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub content: String,
}

/// Role in a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

/// Output from a reasoning call.
#[derive(Debug, Clone)]
pub struct ReasoningOutput {
    pub content: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
}

/// Trait abstracting LLM providers.
#[async_trait]
pub trait ReasoningEngine: Send + Sync {
    /// Send a conversation and get a response.
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
    ) -> Result<ReasoningOutput>;

    /// Get the model's context window size in tokens.
    fn context_limit(&self) -> usize;

    /// Get the model identifier.
    fn model_name(&self) -> &str;
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
    ) -> Result<ReasoningOutput> {
        Ok(ReasoningOutput {
            content: self.response.clone(),
            input_tokens: 100,
            output_tokens: self.response.len() / 4,
        })
    }

    fn context_limit(&self) -> usize {
        self.context_limit
    }

    fn model_name(&self) -> &str {
        "mock-engine"
    }
}
