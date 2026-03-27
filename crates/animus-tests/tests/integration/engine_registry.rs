use std::str::FromStr;
use animus_cortex::engine_registry::{CognitiveRole, EngineRegistry, Provider};
use animus_cortex::MockEngine;

#[test]
fn test_engine_registry_fallback() {
    let registry = EngineRegistry::new(std::sync::Arc::new(MockEngine::new("default")));

    // All roles should return the fallback
    assert_eq!(registry.engine_for(CognitiveRole::Perception).model_name(), "mock-engine");
    assert_eq!(registry.engine_for(CognitiveRole::Reflection).model_name(), "mock-engine");
    assert_eq!(registry.engine_for(CognitiveRole::Reasoning).model_name(), "mock-engine");
}

#[test]
fn test_engine_registry_per_role() {
    let mut registry = EngineRegistry::new(std::sync::Arc::new(MockEngine::new("default")));
    registry.set_engine(CognitiveRole::Reasoning, std::sync::Arc::new(MockEngine::new("opus")));

    // Reasoning should use assigned engine, others use fallback
    assert_eq!(registry.engine_for(CognitiveRole::Reasoning).model_name(), "mock-engine");
    assert_eq!(registry.engine_for(CognitiveRole::Perception).model_name(), "mock-engine");
}

#[test]
fn test_provider_parsing() {
    assert_eq!(Provider::from_str("Anthropic"), Ok(Provider::Anthropic));
    assert_eq!(Provider::from_str("MOCK"), Ok(Provider::Mock));
    assert!(Provider::from_str("invalid").is_err());
}
