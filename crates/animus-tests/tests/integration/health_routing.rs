//! Integration tests for health-weighted routing in SmartRouter.
//!
//! Covers:
//! - confirmed-down engine is skipped and the fallback is selected
//! - all engines down returns an emergency/stub spec without panicking
//! - route_all_candidates excludes zero-weight engines

use animus_core::{BudgetPressure, ContentSensitivity};
use animus_cortex::model_plan::default_task_classes;
use animus_cortex::{CapabilityRegistry, ModelPlan, SmartRouter};
use animus_core::threading::Signal;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Build a SmartRouter with two ollama candidates: a primary and a fallback.
/// Returns (router, signal_rx, primary_key, fallback_key).
async fn make_two_candidate_router() -> (SmartRouter, mpsc::Receiver<Signal>, String, String) {
    let primary = "ollama:qwen3.5:35b".to_string();
    let fallback = "ollama:qwen3.5:9b".to_string();
    let available = vec![primary.clone(), fallback.clone()];

    let registry = CapabilityRegistry::build(None, &[], &available).await;
    let plan = ModelPlan::build_from_capabilities(&registry, &available, default_task_classes());
    let plan_arc = Arc::new(RwLock::new(plan));
    let (tx, rx) = mpsc::channel(32);
    let registry_arc = Arc::new(registry);
    let router = SmartRouter::new(plan_arc, tx, registry_arc);

    (router, rx, primary, fallback)
}

/// Test 1: When the primary engine is confirmed down (health weight 0.0),
/// select_for_class should route to the fallback engine instead.
#[tokio::test]
async fn confirmed_down_primary_routes_to_fallback() {
    let (router, _rx, primary_key, fallback_key) = make_two_candidate_router().await;

    // Mark primary as confirmed down, fallback as healthy.
    router.set_engine_health(&primary_key, 0.0);
    router.set_engine_health(&fallback_key, 1.0);

    let decision = router
        .select_for_class("Analytical", BudgetPressure::Normal)
        .await;

    let selected_key = format!("{}:{}", decision.model_spec.provider, decision.model_spec.model);
    assert_ne!(
        selected_key, primary_key,
        "confirmed-down primary should be skipped, but it was selected"
    );
    assert_eq!(
        selected_key, fallback_key,
        "fallback should be selected when primary is confirmed down"
    );
}

/// Test 2: When ALL engines are confirmed down (health weight 0.0),
/// select_for_class must not panic and must return some result (an emergency spec).
#[tokio::test]
async fn all_down_returns_emergency_spec() {
    let (router, _rx, primary_key, fallback_key) = make_two_candidate_router().await;

    // Mark all engines as confirmed down.
    router.set_engine_health(&primary_key, 0.0);
    router.set_engine_health(&fallback_key, 0.0);

    // Must not panic; returns emergency RouteDecision using the first candidate as the spec.
    let decision = router
        .select_for_class("Analytical", BudgetPressure::Normal)
        .await;

    // The class name must be populated.
    assert!(!decision.class_name.is_empty(), "class_name should not be empty");

    // The model_spec must contain a non-empty provider and model.
    assert!(
        !decision.model_spec.provider.is_empty(),
        "emergency spec provider should not be empty"
    );
    assert!(
        !decision.model_spec.model.is_empty(),
        "emergency spec model should not be empty"
    );
}

/// Test 3: route_all_candidates must exclude zero-weight engines and include
/// engines with non-zero health weight.
#[tokio::test]
async fn route_all_candidates_excludes_zero_weight() {
    let (router, _rx, primary_key, fallback_key) = make_two_candidate_router().await;

    // Primary confirmed down, fallback confirmed healthy.
    router.set_engine_health(&primary_key, 0.0);
    router.set_engine_health(&fallback_key, 1.0);

    let candidates = router
        .route_all_candidates("anything", BudgetPressure::Normal, ContentSensitivity::Public)
        .await;

    // The confirmed-down primary must not appear.
    let has_primary = candidates.iter().any(|d| {
        format!("{}:{}", d.model_spec.provider, d.model_spec.model) == primary_key
    });
    assert!(
        !has_primary,
        "zero-weight engine should be excluded from route_all_candidates"
    );

    // The healthy fallback must appear.
    let has_fallback = candidates.iter().any(|d| {
        format!("{}:{}", d.model_spec.provider, d.model_spec.model) == fallback_key
    });
    assert!(
        has_fallback,
        "healthy engine should be included in route_all_candidates"
    );
}
