use std::collections::HashMap;

use crate::llm::ReasoningEngine;

/// Cognitive function that a model serves.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum CognitiveRole {
    /// Fast triage of sensor events (Haiku-class).
    Perception,
    /// Periodic self-reflection and synthesis (Sonnet-class).
    Reflection,
    /// Active conversation and reasoning (Opus-class).
    Reasoning,
}

/// Routes cognitive functions to appropriate LLM engines.
pub struct EngineRegistry {
    engines: HashMap<CognitiveRole, Box<dyn ReasoningEngine>>,
    fallback: Box<dyn ReasoningEngine>,
}

impl EngineRegistry {
    pub fn new(fallback: Box<dyn ReasoningEngine>) -> Self {
        Self {
            engines: HashMap::new(),
            fallback,
        }
    }

    /// Assign an engine to a cognitive role.
    pub fn set_engine(&mut self, role: CognitiveRole, engine: Box<dyn ReasoningEngine>) {
        self.engines.insert(role, engine);
    }

    /// Get the engine for a cognitive role, falling back to default.
    pub fn engine_for(&self, role: CognitiveRole) -> &dyn ReasoningEngine {
        self.engines
            .get(&role)
            .map(|e| e.as_ref())
            .unwrap_or(self.fallback.as_ref())
    }

    /// Get the fallback engine (for backwards compatibility).
    pub fn fallback(&self) -> &dyn ReasoningEngine {
        self.fallback.as_ref()
    }
}

/// LLM provider type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Ollama,
    Mock,
}

impl std::str::FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "anthropic" => Ok(Self::Anthropic),
            "ollama" => Ok(Self::Ollama),
            "mock" => Ok(Self::Mock),
            _ => Err(format!("unknown provider: {s}")),
        }
    }
}

/// Configuration for building an engine.
pub struct EngineConfig {
    pub provider: Provider,
    pub model: String,
    pub max_tokens: usize,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockEngine;

    #[test]
    fn test_cognitive_role_variants() {
        let roles = [CognitiveRole::Perception, CognitiveRole::Reflection, CognitiveRole::Reasoning];
        assert_eq!(roles.len(), 3);
    }

    #[test]
    fn test_registry_returns_assigned_engine() {
        let mut registry = EngineRegistry::new(Box::new(MockEngine::new("fallback")));
        registry.set_engine(CognitiveRole::Reasoning, Box::new(MockEngine::new("opus")));

        assert_eq!(registry.engine_for(CognitiveRole::Reasoning).model_name(), "mock-engine");
    }

    #[test]
    fn test_registry_falls_back_to_default() {
        let registry = EngineRegistry::new(Box::new(MockEngine::new("fallback")));
        let engine = registry.engine_for(CognitiveRole::Perception);
        assert_eq!(engine.model_name(), "mock-engine");
    }

    #[test]
    fn test_provider_from_str() {
        use std::str::FromStr;
        assert_eq!(Provider::from_str("anthropic"), Ok(Provider::Anthropic));
        assert_eq!(Provider::from_str("ollama"), Ok(Provider::Ollama));
        assert_eq!(Provider::from_str("mock"), Ok(Provider::Mock));
        assert!(Provider::from_str("unknown").is_err());
    }
}
