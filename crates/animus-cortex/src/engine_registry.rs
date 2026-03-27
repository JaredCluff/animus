use std::collections::HashMap;
use std::sync::Arc;

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
    /// Secondary lookup by "provider:model" string — used by SmartRouter spec-based dispatch.
    by_name: HashMap<String, Arc<dyn ReasoningEngine>>,
    fallback: Box<dyn ReasoningEngine>,
}

impl EngineRegistry {
    pub fn new(fallback: Box<dyn ReasoningEngine>) -> Self {
        Self {
            engines: HashMap::new(),
            by_name: HashMap::new(),
            fallback,
        }
    }

    /// Assign an engine to a cognitive role.
    pub fn set_engine(&mut self, role: CognitiveRole, engine: Box<dyn ReasoningEngine>) {
        self.engines.insert(role, engine);
    }

    /// Register an engine by provider+model name for spec-based routing.
    /// Call this alongside `set_engine` so the SmartRouter can look engines up by ModelSpec.
    pub fn register_named(&mut self, provider: &str, model: &str, engine: Arc<dyn ReasoningEngine>) {
        let key = format!("{provider}:{model}");
        self.by_name.insert(key, engine);
    }

    /// Look up an engine by provider+model string (from a ModelSpec).
    /// Returns None if the engine was not registered via `register_named`.
    pub fn engine_by_spec(&self, provider: &str, model: &str) -> Option<Arc<dyn ReasoningEngine>> {
        let key = format!("{provider}:{model}");
        self.by_name.get(&key).cloned()
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

    /// Hot-add an engine by name at runtime (autonomous provider hot-reload).
    pub fn add_named(&mut self, provider: &str, model: &str, engine: Arc<dyn ReasoningEngine>) {
        self.register_named(provider, model, engine);
    }

    /// Returns all registered named engine keys in "provider:model" format.
    pub fn named_model_ids(&self) -> Vec<String> {
        self.by_name.keys().cloned().collect()
    }

    /// Iterate over all named engines as ("provider:model", Arc<Engine>) pairs.
    /// Used by ModelHealthWatcher to discover probe endpoints.
    pub fn iter_named(&self) -> impl Iterator<Item = (&str, Arc<dyn ReasoningEngine>)> + '_ {
        self.by_name.iter().map(|(k, v)| (k.as_str(), v.clone()))
    }
}

/// LLM provider type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Ollama,
    OpenAI,
    Mock,
}

impl std::str::FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "anthropic" => Ok(Self::Anthropic),
            "ollama" => Ok(Self::Ollama),
            "openai" | "openai-compat" | "openai_compat" => Ok(Self::OpenAI),
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

    #[test]
    fn test_register_named_and_engine_by_spec() {
        let mut registry = EngineRegistry::new(Box::new(MockEngine::new("fallback")));
        let engine = Arc::new(MockEngine::new("claude-opus-4"));
        registry.register_named("anthropic", "claude-opus-4", engine);

        let found = registry.engine_by_spec("anthropic", "claude-opus-4");
        assert!(found.is_some());
        assert_eq!(found.unwrap().model_name(), "mock-engine");
    }

    #[test]
    fn test_engine_by_spec_returns_none_for_unknown() {
        let registry = EngineRegistry::new(Box::new(MockEngine::new("fallback")));
        assert!(registry.engine_by_spec("anthropic", "unknown-model").is_none());
    }

    #[test]
    fn test_add_named_delegates_to_register_named() {
        let mut registry = EngineRegistry::new(Box::new(MockEngine::new("fallback")));
        let engine = Arc::new(MockEngine::new("llama3"));
        registry.add_named("ollama", "llama3", engine);

        assert!(registry.engine_by_spec("ollama", "llama3").is_some());
    }
}
