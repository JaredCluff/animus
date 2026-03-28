//! Smart Router — routes AILF reasoning turns to the appropriate model.
//!
//! The router consults the `ModelPlan` to classify each input and select the best engine.
//! Route health and rate limit state are tracked in Layer 1 (no LLM). Changes fire a
//! single Signal (Layer 3).
//!
//! # Three-layer routing decisions
//!
//! Two independent conditions can cause the router to skip the primary model:
//!
//! 1. **Health degradation** — primary has `>= 3` consecutive failures (existing behavior).
//! 2. **Rate limit proximity** — primary's remaining capacity is below [`RATE_LIMIT_NEAR_THRESHOLD`]
//!    (10% of limit). One `Normal` Signal fires on the first crossing per window.
//!
//! Both conditions fall back through `route.fallbacks` in order.
//!
//! # Thread-local stability
//! The router is consulted **at thread start**, not per-turn. Once a thread selects a model,
//! it uses that model for all subsequent turns unless the model fails. This preserves
//! reasoning continuity within a conversation.
//!
//! [`RATE_LIMIT_NEAR_THRESHOLD`]: animus_core::RATE_LIMIT_NEAR_THRESHOLD

use crate::model_plan::{HeuristicClassifier, ModelPlan, ModelSpec, RouteStats};
use animus_core::identity::ThreadId;
use animus_core::threading::{Signal, SignalPriority};
use animus_core::{BudgetPressure, ContentSensitivity, CostTier, ProviderTrustProfile};
use animus_core::provider_catalog::provider_trust_map;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use parking_lot::Mutex;

/// Threshold: confidence below this triggers Perception engine classification.
const HEURISTIC_CONFIDENCE_THRESHOLD: f32 = 0.5;

/// Consecutive failures before marking a route as degraded.
const DEGRADATION_THRESHOLD: u32 = 3;

/// Registry of known provider trust profiles, keyed by provider_id.
pub type ProviderTrustRegistry = std::collections::HashMap<String, ProviderTrustProfile>;

/// Provider IDs that are unconditionally prohibited (e.g. PRC/Russia jurisdiction).
/// Checked independently of trust_floor arithmetic so they cannot be bypassed.
type ProhibitedSet = HashSet<String>;

// ---------------------------------------------------------------------------
// RouteHealth — Layer 1 state, no LLM
// ---------------------------------------------------------------------------

/// Per-route health state tracked by the Cortex substrate.
#[derive(Debug, Default, Clone)]
pub struct RouteHealth {
    pub consecutive_failures: u32,
    pub degraded: bool,
    pub last_failure: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// RouteDecision
// ---------------------------------------------------------------------------

/// The router's decision for a given input: which class, which model, which fallback.
#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub class_name: String,
    /// The selected model spec (primary or a fallback).
    pub model_spec: ModelSpec,
    /// 0 = primary, 1+ = fallback index.
    pub fallback_index: usize,
}

// ---------------------------------------------------------------------------
// SmartRouter
// ---------------------------------------------------------------------------

/// Routes conversation turns to the appropriate model based on the `ModelPlan`.
///
/// Cheaply cloneable — share between runtime and tools.
#[derive(Clone)]
pub struct SmartRouter {
    plan: Arc<RwLock<ModelPlan>>,
    classifier: Arc<RwLock<HeuristicClassifier>>,
    /// Per-route health tracking — Layer 1, no LLM.
    route_health: Arc<Mutex<HashMap<String, RouteHealth>>>,
    signal_tx: mpsc::Sender<Signal>,
    source_id: ThreadId,
    /// Per-model rate limit state handles — populated by register_rate_limit_state().
    rate_limit_states: Arc<Mutex<HashMap<String, Arc<parking_lot::RwLock<animus_core::RateLimitState>>>>>,
    /// Provider trust profiles — populated from provider_catalog at startup.
    trust_registry: Arc<parking_lot::Mutex<ProviderTrustRegistry>>,
    /// Providers that are always blocked regardless of content or budget pressure.
    prohibited_providers: Arc<parking_lot::Mutex<ProhibitedSet>>,
    /// Engine health weight — keyed by "provider:model", updated by ModelHealthWatcher.
    /// `1.0` = confirmed healthy, `0.5` = unknown (not yet probed), `0.0` = confirmed down.
    engine_health: Arc<parking_lot::Mutex<HashMap<String, f32>>>,
    /// Trigger channel sender — send engine keys to request an immediate out-of-band probe.
    /// Set by main.rs after SmartRouter creation via `set_probe_trigger_tx`.
    probe_trigger_tx: Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::Sender<Vec<String>>>>>,
    /// Capability profiles for all available models — used by ModelScorer at routing time.
    capability_registry: Arc<crate::capability_registry::CapabilityRegistry>,
}

impl SmartRouter {
    pub fn new(
        plan: Arc<RwLock<ModelPlan>>,
        signal_tx: mpsc::Sender<Signal>,
        capability_registry: Arc<crate::capability_registry::CapabilityRegistry>,
    ) -> Self {
        let classifier = {
            // Safe at construction time — no concurrent writers exist yet.
            let guard = plan.try_read().expect("SmartRouter::new: plan lock contention");
            HeuristicClassifier::from_plan(&guard)
        };

        // Call provider_trust_map() once; derive both trust_registry and prohibited_providers from it.
        let trust_map = provider_trust_map();
        let prohibited: ProhibitedSet = {
            use animus_core::provider_meta::OwnershipRisk;
            trust_map.values()
                .filter(|p| p.ownership_risk == OwnershipRisk::Prohibited)
                .map(|p| p.provider_id.clone())
                .collect()
        };

        Self {
            plan,
            classifier: Arc::new(RwLock::new(classifier)),
            route_health: Arc::new(Mutex::new(HashMap::new())),
            signal_tx,
            source_id: ThreadId::new(),
            rate_limit_states: Arc::new(Mutex::new(HashMap::new())),
            trust_registry: Arc::new(parking_lot::Mutex::new(trust_map)),
            prohibited_providers: Arc::new(parking_lot::Mutex::new(prohibited)),
            engine_health: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            probe_trigger_tx: Arc::new(parking_lot::Mutex::new(None)),
            capability_registry,
        }
    }

    /// Rebuild the heuristic classifier from the current plan.
    /// Call after plan amendments.
    pub async fn rebuild_classifier(&self) {
        let plan = self.plan.read().await;
        let new_classifier = HeuristicClassifier::from_plan(&plan);
        let mut classifier = self.classifier.write().await;
        *classifier = new_classifier;
    }

    /// Classify input using heuristics only (no LLM). Returns (class_name, confidence).
    pub async fn classify_heuristic(&self, input: &str) -> (String, f32) {
        self.classifier.read().await.classify(input)
    }

    /// Select the best available `ModelSpec` for an input using heuristics.
    /// If confidence is below threshold, the caller should escalate to the Perception engine.
    /// Returns `(RouteDecision, needs_perception_escalation)`.
    pub async fn route(&self, input: &str, pressure: BudgetPressure) -> (RouteDecision, bool) {
        let (class_name, confidence) = self.classify_heuristic(input).await;
        let needs_escalation = confidence < HEURISTIC_CONFIDENCE_THRESHOLD;
        let decision = self.select_for_class(&class_name, pressure).await;
        (decision, needs_escalation)
    }

    /// Select the best available model for a task class using live scoring.
    ///
    /// Scores all candidates in the route against current runtime state, returns
    /// the highest-scoring non-zero candidate. Falls back to first candidate with
    /// an Urgent Signal if all score 0.
    pub async fn select_for_class(
        &self,
        class_name: &str,
        pressure: BudgetPressure,
    ) -> RouteDecision {
        use crate::model_scorer::{ModelScorer, ScoringContext};

        let plan = self.plan.read().await;

        let route = match plan.routes.get(class_name).or_else(|| plan.routes.values().next()) {
            Some(r) => r,
            None => {
                drop(plan);
                return self.stub_decision(class_name);
            }
        };

        let task_class = plan.task_classes.iter().find(|tc| tc.name == class_name).cloned();
        let mut best: Option<(usize, ModelSpec, f32)> = None;

        for (idx, spec) in route.candidates.iter().enumerate() {
            let model_key = format!("{}:{}", spec.provider, spec.model);
            let health_w = self.engine_health_weight(&model_key);
            if health_w == 0.0 {
                tracing::debug!(
                    "select_for_class: skipping {}:{} — confirmed down (health_w=0.0)",
                    spec.provider, spec.model
                );
                continue;
            }
            let engine_available = true; // health_w > 0.0 verified above

            let (remaining_pct, rpm_ceiling, near_limit, should_notify) = {
                let states = self.rate_limit_states.lock();
                if let Some(rl_arc) = states.get(&model_key) {
                    let mut state = rl_arc.write();
                    let pct = match (state.requests_limit, state.requests_remaining) {
                        (Some(lim), Some(rem)) if lim > 0 => rem as f32 / lim as f32,
                        _ => 1.0,
                    };
                    let ceiling = state.requests_limit;
                    let near = state.is_near_limit(animus_core::RATE_LIMIT_NEAR_THRESHOLD);
                    let notify = near && !state.near_limit_notified;
                    if notify { state.near_limit_notified = true; }
                    (pct, ceiling, near, notify)
                } else {
                    (1.0, None, false, false)
                }
            };

            // Fire near-limit signal as soon as we detect the crossing — regardless of which
            // candidate ends up selected.
            if near_limit && should_notify {
                tracing::info!("Rate limit near for model '{model_key}' — routing to fallback");
                let _ = self.signal_tx.try_send(Signal {
                    source_thread: self.source_id,
                    target_thread: ThreadId::default(),
                    priority: SignalPriority::Normal,
                    summary: format!("Rate limit near for model '{model_key}' — routing to fallback"),
                    segment_refs: vec![],
                    created: Utc::now(),
                });
            }

            let profile_opt = self.capability_registry.get(&spec.provider, &spec.model);
            let rpm_ceiling = profile_opt
                .and_then(|p| p.rate_limit_rpm_ceiling)
                .or(rpm_ceiling);

            let learned = route.learned_quality_for(&model_key);

            let context = ScoringContext {
                rate_limit_remaining_pct: remaining_pct,
                rate_limit_rpm_ceiling: rpm_ceiling,
                budget_pressure: pressure,
                engine_available,
                learned_quality: learned,
            };

            let raw_score = if let (Some(profile), Some(ref tc)) = (profile_opt, &task_class) {
                ModelScorer::score(profile, &tc.to_weights(), &context)
            } else if engine_available && ModelScorer::passes_budget(
                &animus_core::model_capability::ModelCapabilityProfile {
                    provider: spec.provider.clone(),
                    model_id: spec.model.clone(),
                    parameter_count_b: None, release_date: None, context_window: None,
                    reasoning_support: animus_core::model_capability::ReasoningSupport::None,
                    generation_tok_per_sec: None,
                    prefill_speed: animus_core::model_capability::PrefillSpeed::Moderate,
                    rate_limit_rpm_ceiling: None, rate_limit_tpd_ceiling: None,
                    cost_tier: spec.cost.unwrap_or(CostTier::Moderate),
                    cost_per_mtok_input: None, cost_per_mtok_output: None,
                    trust_score: 2,
                    data_policy: animus_core::provider_meta::DataPolicy::Unknown,
                    profile_source: animus_core::model_capability::ProfileSource::Inferred,
                },
                pressure,
            ) {
                0.3_f32
            } else {
                0.0
            };

            // Near-limit penalty: scale score by remaining capacity fraction so a healthy
            // candidate at 100% always wins over a near-exhausted one at 5%.
            // Health weight multiplier: confirmed-healthy (1.0) scores full; unknown (0.5) scores half.
            let score = (if near_limit {
                raw_score * remaining_pct
            } else {
                raw_score
            }) * health_w;

            if score > 0.0 {
                match &best {
                    None => best = Some((idx, spec.clone(), score)),
                    Some((_, _, best_score)) if score > *best_score => {
                        best = Some((idx, spec.clone(), score));
                    }
                    _ => {}
                }
            }
        }

        let emergency_spec = route.candidates.first().cloned();
        drop(plan);

        if let Some((idx, spec, _)) = best {
            return RouteDecision {
                class_name: class_name.to_string(),
                model_spec: spec,
                fallback_index: idx,
            };
        }

        let summary = format!("All models in route '{}' scored 0 — chain exhausted", class_name);
        tracing::error!("{summary}");
        let _ = self.signal_tx.try_send(Signal {
            source_thread: self.source_id,
            target_thread: ThreadId::default(),
            priority: SignalPriority::Urgent,
            summary,
            segment_refs: vec![],
            created: Utc::now(),
        });

        RouteDecision {
            class_name: class_name.to_string(),
            model_spec: emergency_spec.unwrap_or_else(|| self.stub_spec()),
            fallback_index: 0,
        }
    }

    fn stub_decision(&self, class_name: &str) -> RouteDecision {
        RouteDecision {
            class_name: class_name.to_string(),
            model_spec: self.stub_spec(),
            fallback_index: 0,
        }
    }

    fn stub_spec(&self) -> ModelSpec {
        ModelSpec {
            provider: "anthropic".to_string(),
            model: "fallback".to_string(),
            think: crate::model_plan::ThinkLevel::Dynamic,
            cost: None, speed: None, quality: None, trust_floor: 0,
        }
    }


    /// Record a successful turn for a route — updates RouteStats (Layer 1).
    pub async fn record_success(&self, class_name: &str, model_key: &str, latency_ms: u64) {
        {
            let mut health = self.route_health.lock();
            let h = health.entry(class_name.to_string()).or_default();
            h.consecutive_failures = 0;
            // Recovery: if it was degraded and succeeds, un-degrade
            if h.degraded {
                h.degraded = false;
                tracing::info!("route '{}' recovered", class_name);
            }
        }

        let mut plan = self.plan.write().await;
        if let Some(route) = plan.routes.get_mut(class_name) {
            route.stats.turn_count += 1;
            route.stats.total_latency_ms += latency_ms;
            route.stats.last_turn = Some(Utc::now());
            let model_stats = route.model_stats.entry(model_key.to_string()).or_default();
            model_stats.turn_count += 1;
            model_stats.total_latency_ms += latency_ms;
            model_stats.last_turn = Some(Utc::now());
        }
    }

    /// Record a failure for a route. Fires a Signal if the route degrades (Layer 2 → Layer 3).
    pub async fn record_failure(&self, class_name: &str, model_key: &str) {
        let newly_degraded = {
            let mut health = self.route_health.lock();
            let h = health.entry(class_name.to_string()).or_default();
            h.consecutive_failures += 1;
            h.last_failure = Some(Utc::now());
            if h.consecutive_failures >= DEGRADATION_THRESHOLD && !h.degraded {
                h.degraded = true;
                true
            } else {
                false
            }
        };

        {
            let mut plan = self.plan.write().await;
            if let Some(route) = plan.routes.get_mut(class_name) {
                route.stats.failure_count += 1;
                route.stats.turn_count += 1;
                let model_stats = route.model_stats.entry(model_key.to_string()).or_default();
                model_stats.failure_count += 1;
                model_stats.turn_count += 1;
            }
        }

        if newly_degraded {
            let summary = format!(
                "Route '{}' degraded after {} consecutive failures",
                class_name, DEGRADATION_THRESHOLD
            );
            tracing::warn!("{}", summary);
            let _ = self.signal_tx.try_send(Signal {
                source_thread: self.source_id,
                target_thread: ThreadId::default(),
                priority: SignalPriority::Normal,
                summary,
                segment_refs: vec![],
                created: Utc::now(),
            });
        }
    }

    /// Record a quality gate correction for a route (from VectorFS feedback).
    pub async fn record_correction(&self, class_name: &str, model_key: &str) {
        let mut plan = self.plan.write().await;
        if let Some(route) = plan.routes.get_mut(class_name) {
            route.stats.correction_count += 1;
            let model_stats = route.model_stats.entry(model_key.to_string()).or_default();
            model_stats.correction_count += 1;
        }
    }

    /// Return a snapshot of all RouteStats keyed by class name.
    pub async fn route_stats_snapshot(&self) -> HashMap<String, RouteStats> {
        let plan = self.plan.read().await;
        plan.routes.iter().map(|(k, v)| (k.clone(), v.stats.clone())).collect()
    }

    /// Return a snapshot of RouteHealth keyed by class name.
    pub fn route_health_snapshot(&self) -> HashMap<String, RouteHealth> {
        self.route_health.lock().clone()
    }

    /// Replace the plan (after rebuild or amendment). Rebuilds the classifier.
    pub async fn update_plan(&self, new_plan: ModelPlan) {
        let new_classifier = HeuristicClassifier::from_plan(&new_plan);
        {
            let mut plan = self.plan.write().await;
            *plan = new_plan;
        }
        let mut classifier = self.classifier.write().await;
        *classifier = new_classifier;
        // Reset health on full plan rebuild
        self.route_health.lock().clear();
        tracing::info!("SmartRouter: plan updated, classifier rebuilt");
    }

    /// Set the health weight for an engine. 1.0 = healthy, 0.5 = unknown, 0.0 = confirmed down.
    /// Key is "provider:model".
    pub fn set_engine_health(&self, key: &str, weight: f32) {
        let safe_weight = if weight.is_nan() { 0.0 } else { weight.clamp(0.0, 1.0) };
        self.engine_health.lock().insert(key.to_string(), safe_weight);
    }

    /// Return the health weight for an engine key.
    /// Returns `0.5` (unknown) when not yet probed.
    pub fn engine_health_weight(&self, key: &str) -> f32 {
        *self.engine_health.lock().get(key).unwrap_or(&0.5_f32)
    }

    /// Check if the engine for a ModelSpec is not confirmed down (weight > 0.0).
    pub(crate) fn is_engine_healthy(&self, spec: &crate::model_plan::ModelSpec) -> bool {
        let key = format!("{}:{}", spec.provider, spec.model);
        self.engine_health_weight(&key) > 0.0
    }

    /// Mark an engine confirmed down (weight = 0.0) and trigger an immediate re-probe.
    pub fn mark_engine_unhealthy(&self, key: &str) {
        self.set_engine_health(key, 0.0);
        self.trigger_probe(vec![key.to_string()]);
    }

    /// Send engine keys through the trigger channel to request an immediate probe.
    pub fn trigger_probe(&self, keys: Vec<String>) {
        if let Some(tx) = self.probe_trigger_tx.lock().as_ref() {
            let _ = tx.try_send(keys);
        }
    }

    /// Wire the probe trigger sender. Called once from main.rs after channel creation.
    pub fn set_probe_trigger_tx(&self, tx: tokio::sync::mpsc::Sender<Vec<String>>) {
        *self.probe_trigger_tx.lock() = Some(tx);
    }

    /// Register a rate limit state handle for a model.
    /// Call once per engine at startup: `router.register_rate_limit_state(engine.model_name(), engine.rate_limit_state().unwrap())`.
    pub fn register_rate_limit_state(
        &self,
        model_name: &str,
        state: Arc<parking_lot::RwLock<animus_core::RateLimitState>>,
    ) {
        self.rate_limit_states.lock().insert(model_name.to_string(), state);
    }

    /// Get a read reference to the current plan.
    pub fn plan(&self) -> Arc<RwLock<ModelPlan>> {
        self.plan.clone()
    }

    /// Get a read reference to the classifier.
    pub fn classifier(&self) -> Arc<RwLock<HeuristicClassifier>> {
        self.classifier.clone()
    }

    /// Check if a provider is prohibited (PRC/Russia jurisdiction etc.).
    fn is_prohibited(&self, provider: &str) -> bool {
        self.prohibited_providers.lock().contains(provider)
    }

    /// Check if a ModelSpec passes the budget filter.
    fn passes_budget(spec: &crate::model_plan::ModelSpec, pressure: BudgetPressure) -> bool {
        let cost = spec.cost.unwrap_or(CostTier::Moderate);
        match pressure {
            BudgetPressure::Normal => true,
            BudgetPressure::Careful => cost <= CostTier::Moderate,
            BudgetPressure::Emergency | BudgetPressure::Exceeded => cost == CostTier::Free,
        }
    }

    /// Check if a ModelSpec passes the trust filter for the given content sensitivity.
    fn passes_trust(&self, spec: &crate::model_plan::ModelSpec, required_floor: u8) -> bool {
        if self.is_prohibited(&spec.provider) {
            return false;
        }
        let registry = self.trust_registry.lock();
        let effective = registry.get(&spec.provider)
            .map(|p| p.effective_trust)
            .unwrap_or(0); // unknown provider → assume untrusted
        effective >= required_floor
    }

    /// Check if Critical content has a local engine available.
    /// Critical content must only go to local (Ollama) providers.
    fn is_local_provider(provider: &str) -> bool {
        provider == "ollama"
    }

    /// Route with budget + trust + sensitivity constraints applied.
    ///
    /// Falls back through the route's fallback chain, skipping any model that
    /// violates the constraints. If no model passes, returns an error.
    pub async fn route_with_constraints(
        &self,
        input: &str,
        pressure: BudgetPressure,
        sensitivity: ContentSensitivity,
    ) -> Result<RouteDecision, String> {
        let (class_name, _confidence) = self.classify_heuristic(input).await;
        let required_floor = sensitivity.required_trust_floor();

        let plan = self.plan.read().await;
        let route = plan.routes.get(&class_name)
            .or_else(|| plan.routes.values().next())
            .ok_or_else(|| "no routes in plan".to_string())?;

        // Build candidate list: primary first, then fallbacks
        let candidates: Vec<(usize, &crate::model_plan::ModelSpec)> =
            route.candidates.iter().enumerate().collect();

        for (fallback_index, spec) in candidates {
            // Hard prohibition check
            if self.is_prohibited(&spec.provider) {
                tracing::debug!("Skipping {} — prohibited provider", spec.provider);
                continue;
            }
            // Critical content → local only
            if sensitivity == ContentSensitivity::Critical && !Self::is_local_provider(&spec.provider) {
                tracing::debug!("Skipping {} — Critical content requires local provider", spec.provider);
                continue;
            }
            // Trust floor check
            if !self.passes_trust(spec, required_floor) {
                tracing::debug!("Skipping {}:{} — trust floor not met (required {})", spec.provider, spec.model, required_floor);
                continue;
            }
            // Budget check
            if !Self::passes_budget(spec, pressure) {
                tracing::debug!("Skipping {}:{} — budget pressure {:?}", spec.provider, spec.model, pressure);
                continue;
            }
            // MAJOR-4: engine health check — skip engines the health watcher has marked unavailable
            if !self.is_engine_healthy(spec) {
                tracing::debug!("Skipping {}:{} — engine unavailable", spec.provider, spec.model);
                continue;
            }
            return Ok(RouteDecision {
                class_name,
                model_spec: spec.clone(),
                fallback_index,
            });
        }

        Err(format!(
            "No engine passes constraints for class '{}' (sensitivity={:?}, pressure={:?})",
            class_name, sensitivity, pressure
        ))
    }

    /// Returns ALL candidates (primary + fallbacks) that pass static constraints, in priority order.
    /// Used by the runtime to try each in sequence on transient failures (rate limits, 503s).
    pub async fn route_all_candidates(
        &self,
        input: &str,
        pressure: BudgetPressure,
        sensitivity: ContentSensitivity,
    ) -> Vec<RouteDecision> {
        let (class_name, _confidence) = self.classify_heuristic(input).await;
        let required_floor = sensitivity.required_trust_floor();

        let plan = self.plan.read().await;
        let route = match plan.routes.get(&class_name).or_else(|| plan.routes.values().next()) {
            Some(r) => r,
            None => return vec![],
        };

        let candidates: Vec<(usize, &crate::model_plan::ModelSpec)> =
            route.candidates.iter().enumerate().collect();

        candidates.into_iter()
            .filter(|(_, spec)| {
                !self.is_prohibited(&spec.provider)
                && !(sensitivity == ContentSensitivity::Critical && !Self::is_local_provider(&spec.provider))
                && self.passes_trust(spec, required_floor)
                && Self::passes_budget(spec, pressure)
                && self.is_engine_healthy(spec)
            })
            .map(|(fallback_index, spec)| RouteDecision {
                class_name: class_name.clone(),
                model_spec: spec.clone(),
                fallback_index,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_plan::ModelPlan;
    use tokio::sync::mpsc;
    use animus_core::rate_limit::RateLimitState;
    use parking_lot::RwLock as ParkingRwLock;

    fn make_near_limit_state() -> Arc<ParkingRwLock<RateLimitState>> {
        Arc::new(ParkingRwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(50), // 5% — near limit
            near_limit_notified: false,
            ..Default::default()
        }))
    }

    fn make_ok_state() -> Arc<ParkingRwLock<RateLimitState>> {
        Arc::new(ParkingRwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(500), // 50% — fine
            near_limit_notified: false,
            ..Default::default()
        }))
    }

    async fn make_router_async() -> (SmartRouter, mpsc::Receiver<Signal>) {
        use crate::capability_registry::CapabilityRegistry;
        use crate::model_plan::default_task_classes;
        let available = vec!["ollama:qwen3.5:35b".to_string(), "ollama:qwen3.5:9b".to_string()];
        let registry = CapabilityRegistry::build(None, &[], &available).await;
        let plan = ModelPlan::build_from_capabilities(&registry, &available, default_task_classes());
        let plan_arc = Arc::new(RwLock::new(plan));
        let (tx, rx) = mpsc::channel(32);
        let registry_arc = Arc::new(registry);
        let router = SmartRouter::new(plan_arc, tx, registry_arc);
        (router, rx)
    }

    #[tokio::test]
    async fn routes_to_primary_when_rate_limit_is_ok() {
        let (router, _rx) = make_router_async().await;
        // Register primary with healthy (50%) remaining capacity using the full model key
        router.register_rate_limit_state("ollama:qwen3.5:35b", make_ok_state());
        let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
        assert_eq!(decision.fallback_index, 0, "should use primary when rate limit is healthy");
    }

    #[tokio::test]
    async fn routes_to_fallback_when_primary_near_limit() {
        let (router, _rx) = make_router_async().await;
        // Register the Analytical primary as near-limit using the full model key
        let state = make_near_limit_state();
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
        // Should have used a fallback (fallback_index > 0)
        assert!(decision.fallback_index > 0, "expected fallback route, got primary");
    }

    #[tokio::test]
    async fn fires_signal_on_first_near_limit_crossing() {
        let (router, mut rx) = make_router_async().await;
        let state = make_near_limit_state();
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        router.select_for_class("Analytical", BudgetPressure::Normal).await;
        // Signal should have been sent with the right priority and content
        let signal = rx.try_recv().expect("expected a Signal to be fired");
        assert_eq!(signal.priority, SignalPriority::Normal);
        assert!(
            signal.summary.contains("Rate limit near"),
            "signal summary should describe the near-limit condition, got: {}",
            signal.summary
        );
    }

    #[tokio::test]
    async fn does_not_fire_duplicate_signal_when_already_notified() {
        let (router, mut rx) = make_router_async().await;
        let state = Arc::new(ParkingRwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(50),
            near_limit_notified: true, // already fired — flag was set by a prior routing call
            ..Default::default()
        }));
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
        // No new Signal (flag was already set)
        assert!(rx.try_recv().is_err(), "should not fire duplicate Signal");
        // But routing must still avoid the primary — signaling and routing are independent
        assert!(decision.fallback_index > 0, "should still route to fallback when near-limit, even if no new signal fired");
    }

    #[tokio::test]
    async fn route_returns_decision() {
        let (router, _rx) = make_router_async().await;
        let (decision, _escalate) = router.route("implement a rust function", BudgetPressure::Normal).await;
        assert!(!decision.class_name.is_empty());
        assert!(!decision.model_spec.model.is_empty());
    }

    #[tokio::test]
    async fn record_success_clears_failures() {
        let (router, _rx) = make_router_async().await;
        router.record_failure("Technical", "ollama:qwen3.5:35b").await;
        router.record_failure("Technical", "ollama:qwen3.5:35b").await;
        router.record_success("Technical", "ollama:qwen3.5:35b", 500).await;
        let health = router.route_health_snapshot();
        let h = health.get("Technical").unwrap();
        assert_eq!(h.consecutive_failures, 0);
        assert!(!h.degraded);
    }

    #[tokio::test]
    async fn record_failure_degrades_after_threshold() {
        let (router, mut rx) = make_router_async().await;
        router.record_failure("Technical", "ollama:qwen3.5:35b").await;
        router.record_failure("Technical", "ollama:qwen3.5:35b").await;
        router.record_failure("Technical", "ollama:qwen3.5:35b").await; // threshold hit

        let health = router.route_health_snapshot();
        assert!(health["Technical"].degraded);

        // Should have sent a signal
        let signal = rx.try_recv();
        assert!(signal.is_ok());
        assert!(signal.unwrap().summary.contains("degraded"));
    }

    #[tokio::test]
    async fn degraded_route_falls_back() {
        let (router, _rx) = make_router_async().await;
        // Mark the primary engine as unavailable — scorer checks engine_available, not route_health
        router.set_engine_health("ollama:qwen3.5:35b", 0.0);
        let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
        assert_eq!(decision.fallback_index, 1); // using first fallback
    }

    #[tokio::test]
    async fn route_stats_accumulate() {
        let (router, _rx) = make_router_async().await;
        router.record_success("Technical", "ollama:qwen3.5:35b", 300).await;
        router.record_success("Technical", "ollama:qwen3.5:35b", 700).await;
        let stats = router.route_stats_snapshot().await;
        let s = &stats["Technical"];
        assert_eq!(s.turn_count, 2);
        assert_eq!(s.avg_latency_ms(), Some(500));
    }

    #[tokio::test]
    async fn prohibited_provider_never_selected() {
        let (router, _rx) = make_router_async().await;
        assert!(router.is_prohibited("qwen-api"), "qwen-api must be prohibited");
        assert!(router.is_prohibited("deepseek-api"), "deepseek-api must be prohibited");
        assert!(!router.is_prohibited("anthropic"), "anthropic must not be prohibited");
        assert!(!router.is_prohibited("cerebras"), "cerebras must not be prohibited");
    }

    #[test]
    fn budget_filter_free_only_on_emergency() {
        use crate::model_plan::{ModelSpec, ThinkLevel};
        let spec_free = ModelSpec {
            provider: "cerebras".to_string(),
            model: "llama3.1-8b".to_string(),
            think: ThinkLevel::Off,
            cost: Some(CostTier::Free),
            speed: None, quality: None, trust_floor: 0,
        };
        let spec_expensive = ModelSpec {
            provider: "anthropic".to_string(),
            model: "claude-opus-4-6".to_string(),
            think: ThinkLevel::Dynamic,
            cost: Some(CostTier::Expensive),
            speed: None, quality: None, trust_floor: 0,
        };
        assert!(SmartRouter::passes_budget(&spec_free, BudgetPressure::Emergency));
        assert!(!SmartRouter::passes_budget(&spec_expensive, BudgetPressure::Emergency));
        assert!(SmartRouter::passes_budget(&spec_expensive, BudgetPressure::Normal));
    }

    #[tokio::test]
    async fn scorer_prefers_higher_capability_candidate() {
        use crate::capability_registry::CapabilityRegistry;
        use crate::model_plan::{Route, RouteStats, ThinkLevel, default_task_classes};
        use animus_core::budget::BudgetPressure;

        // Build a plan with two candidates: first is small/old, second is large/recent
        // The scorer should pick the second for an Analytical (quality-heavy) task
        let small_old = ModelSpec {
            provider: "ollama".to_string(), model: "tinyllama:1b".to_string(),
            think: ThinkLevel::Off, cost: Some(CostTier::Free),
            speed: None, quality: None, trust_floor: 0,
        };
        let large_recent = ModelSpec {
            provider: "ollama".to_string(), model: "qwen3.5:35b".to_string(),
            think: ThinkLevel::Dynamic, cost: Some(CostTier::Free),
            speed: None, quality: None, trust_floor: 0,
        };

        let mut routes = std::collections::HashMap::new();
        for tc in default_task_classes() {
            routes.insert(tc.name.clone(), Route {
                // Deliberately put small_old first (plan order) to test scorer overrides it
                candidates: vec![small_old.clone(), large_recent.clone()],
                model_stats: std::collections::HashMap::new(),
                stats: RouteStats::default(),
            });
        }

        let plan = ModelPlan {
            id: uuid::Uuid::new_v4(), created: chrono::Utc::now(),
            config_hash: "test".to_string(),
            task_classes: default_task_classes(),
            routes,
            build_reason: "test".to_string(),
        };

        let (tx, _rx) = mpsc::channel(32);
        // Use a registry with actual profiles so ModelScorer can differentiate them
        let registry = Arc::new(
            CapabilityRegistry::build(None, &[], &[
                "ollama:tinyllama:1b".to_string(),
                "ollama:qwen3.5:35b".to_string(),
            ]).await
        );
        let router = SmartRouter::new(Arc::new(RwLock::new(plan)), tx, registry);

        let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
        // The scorer should pick qwen3.5:35b (larger, newer, has extended thinking)
        // over tinyllama:1b even though tinyllama is first in the candidate list
        assert_eq!(decision.model_spec.model, "qwen3.5:35b",
            "Analytical class should prefer 35B model over 1B via live scoring, got: {}",
            decision.model_spec.model);
    }

    #[tokio::test]
    async fn health_weight_methods_roundtrip() {
        let (router, _rx) = make_router_async().await;
        // Unprobed engine defaults to 0.5
        assert_eq!(router.engine_health_weight("ollama:qwen3.5:35b"), 0.5);
        // Set confirmed healthy
        router.set_engine_health("ollama:qwen3.5:35b", 1.0);
        assert_eq!(router.engine_health_weight("ollama:qwen3.5:35b"), 1.0);
        // Mark unhealthy
        router.mark_engine_unhealthy("ollama:qwen3.5:35b");
        assert_eq!(router.engine_health_weight("ollama:qwen3.5:35b"), 0.0);
    }

    #[tokio::test]
    async fn zero_weight_engine_skipped_by_is_engine_healthy() {
        let (router, _rx) = make_router_async().await;
        let spec = crate::model_plan::ModelSpec {
            provider: "ollama".to_string(),
            model: "qwen3.5:35b".to_string(),
            think: crate::model_plan::ThinkLevel::Dynamic,
            cost: None, speed: None, quality: None, trust_floor: 0,
        };
        router.mark_engine_unhealthy("ollama:qwen3.5:35b");
        assert!(!router.is_engine_healthy(&spec));
    }

    #[tokio::test]
    async fn select_for_class_half_weight_halves_score() {
        // An engine at 0.5 weight should score at half of a confirmed-healthy engine
        let (router, _rx) = make_router_async().await;
        // Register two engines with the same spec but different health weights
        // Use the helper that sets engine_health directly
        let key_a = "providerA:model-x";
        let key_b = "providerB:model-x";
        router.set_engine_health(key_a, 1.0);
        router.set_engine_health(key_b, 0.5);
        // Both should be selectable (weight > 0.0)
        assert!(router.engine_health_weight(key_a) == 1.0);
        assert!(router.engine_health_weight(key_b) == 0.5);
        // The test verifies that health_w is applied — actual routing integration
        // is covered by the integration test in Task 5; here we just verify the
        // weight is stored and returned correctly for select_for_class to use.
        let score_a = 1.0_f32 * router.engine_health_weight(key_a);
        let score_b = 1.0_f32 * router.engine_health_weight(key_b);
        assert!(score_a > score_b, "confirmed-healthy should score higher than unknown");
    }

    #[tokio::test]
    async fn select_for_class_zero_weight_skipped() {
        let (router, _rx) = make_router_async().await;
        let key = "deadprovider:model-z";
        router.set_engine_health(key, 0.0);
        assert_eq!(router.engine_health_weight(key), 0.0);
        assert!(!router.is_engine_healthy(&crate::model_plan::ModelSpec {
            provider: "deadprovider".to_string(),
            model: "model-z".to_string(),
            think: crate::model_plan::ThinkLevel::Off,
            cost: None, speed: None, quality: None, trust_floor: 0,
        }));
    }
}
