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
    /// Engine availability — keyed by "provider:model", updated by ModelHealthWatcher.
    /// `true` = available (default when not yet probed), `false` = unavailable.
    engine_health: Arc<parking_lot::Mutex<HashMap<String, bool>>>,
}

impl SmartRouter {
    pub fn new(
        plan: Arc<RwLock<ModelPlan>>,
        signal_tx: mpsc::Sender<Signal>,
    ) -> Self {
        // Build classifier from current plan (synchronously — plan is already loaded)
        // We need a blocking read here. Use try_read for now, fall back to default.
        let classifier = {
            // Safe at construction time — no concurrent writers exist yet.
            let guard = plan.try_read().expect("SmartRouter::new: plan lock contention");
            HeuristicClassifier::from_plan(&guard)
        };

        Self {
            plan,
            classifier: Arc::new(RwLock::new(classifier)),
            route_health: Arc::new(Mutex::new(HashMap::new())),
            signal_tx,
            source_id: ThreadId::new(),
            rate_limit_states: Arc::new(Mutex::new(HashMap::new())),
            trust_registry: {
                let map = provider_trust_map();
                Arc::new(parking_lot::Mutex::new(map))
            },
            prohibited_providers: {
                use animus_core::provider_meta::OwnershipRisk;
                let map = provider_trust_map();
                let prohibited: ProhibitedSet = map.values()
                    .filter(|p| p.ownership_risk == OwnershipRisk::Prohibited)
                    .map(|p| p.provider_id.clone())
                    .collect();
                Arc::new(parking_lot::Mutex::new(prohibited))
            },
            engine_health: Arc::new(parking_lot::Mutex::new(HashMap::new())),
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
    pub async fn route(&self, input: &str) -> (RouteDecision, bool) {
        let (class_name, confidence) = self.classify_heuristic(input).await;
        let needs_escalation = confidence < HEURISTIC_CONFIDENCE_THRESHOLD;

        let decision = self.select_for_class(&class_name).await;
        (decision, needs_escalation)
    }

    /// Select a `RouteDecision` for a given task class name, respecting fallback order
    /// and skipping degraded primaries.
    pub async fn select_for_class(&self, class_name: &str) -> RouteDecision {
        let plan = self.plan.read().await;
        let health = self.route_health.lock();

        let route = plan.routes.get(class_name)
            .or_else(|| plan.routes.values().next()); // fall back to any route

        let Some(route) = route else {
            // No plan at all — return a stub that the runtime will handle via EngineRegistry fallback
            return RouteDecision {
                class_name: class_name.to_string(),
                model_spec: ModelSpec {
                    provider: "anthropic".to_string(),
                    model: "fallback".to_string(),
                    think: crate::model_plan::ThinkLevel::Dynamic,
                    cost: None,
                    speed: None,
                    quality: None,
                    trust_floor: 0,
                },
                fallback_index: 0,
            };
        };

        let route_health = health.get(class_name);
        let primary_degraded = route_health.map(|h| h.degraded).unwrap_or(false);

        if !primary_degraded {
            // Layer 2 + 3: rate limit check with single write-lock (atomic read + arm flag)
            // Write lock prevents TOCTOU: reading is_near_limit() and setting near_limit_notified
            // happen inside the same guard. Drop all rate_limit_states locks before try_send.
            let (near_limit, should_notify) = {
                let states = self.rate_limit_states.lock();
                if let Some(rl_arc) = states.get(&route.primary.model) {
                    let mut state = rl_arc.write(); // write lock — need to set flag atomically
                    let near = state.is_near_limit(animus_core::RATE_LIMIT_NEAR_THRESHOLD);
                    let notify = near && !state.near_limit_notified;
                    if notify {
                        state.near_limit_notified = true; // arm: SmartRouter owns this write
                    }
                    (near, notify)
                } else {
                    (false, false)
                }
            }; // all rate_limit_states locks dropped here — safe to try_send

            if near_limit {
                // Layer 3: fire one Signal on threshold crossing (try_send is non-blocking — no .await needed)
                if should_notify {
                    tracing::info!("Rate limit near for model '{}' — routing to fallback", route.primary.model);
                    let _ = self.signal_tx.try_send(Signal {
                        source_thread: self.source_id,
                        target_thread: ThreadId::default(),
                        priority: SignalPriority::Normal,
                        summary: format!(
                            "Rate limit near for model '{}' — routing to fallback",
                            route.primary.model
                        ),
                        segment_refs: vec![],
                        created: Utc::now(),
                    });
                }

                // Route to first non-degraded fallback (plan and health still in scope — no re-acquire needed)
                for (i, fallback) in route.fallbacks.iter().enumerate() {
                    let fb_key = format!("{}:fallback:{}", class_name, i);
                    let fb_degraded = health.get(&fb_key).map(|h| h.degraded).unwrap_or(false);
                    if !fb_degraded {
                        return RouteDecision {
                            class_name: class_name.to_string(),
                            model_spec: fallback.clone(),
                            fallback_index: i + 1,
                        };
                    }
                }

                // No fallback available — return primary anyway
                return RouteDecision {
                    class_name: class_name.to_string(),
                    model_spec: route.primary.clone(),
                    fallback_index: 0,
                };
            }

            return RouteDecision {
                class_name: class_name.to_string(),
                model_spec: route.primary.clone(),
                fallback_index: 0,
            };
        }

        // Primary degraded — try fallbacks
        for (i, fallback) in route.fallbacks.iter().enumerate() {
            let fb_key = format!("{}:fallback:{}", class_name, i);
            let fb_degraded = health.get(&fb_key).map(|h| h.degraded).unwrap_or(false);
            if !fb_degraded {
                return RouteDecision {
                    class_name: class_name.to_string(),
                    model_spec: fallback.clone(),
                    fallback_index: i + 1,
                };
            }
        }

        // All degraded — fire Urgent signal and return primary anyway
        drop(health);
        drop(plan);
        let summary = format!("All models in route '{}' are degraded — chain exhausted", class_name);
        tracing::error!("{}", summary);
        let _ = self.signal_tx.try_send(Signal {
            source_thread: self.source_id,
            target_thread: ThreadId::default(),
            priority: SignalPriority::Urgent,
            summary,
            segment_refs: vec![],
            created: Utc::now(),
        });

        // Return primary as last resort
        let plan = self.plan.read().await;
        let route = plan.routes.get(class_name)
            .or_else(|| plan.routes.values().next())
            .expect("route must exist since we got here");
        RouteDecision {
            class_name: class_name.to_string(),
            model_spec: route.primary.clone(),
            fallback_index: 0,
        }
    }

    /// Record a successful turn for a route — updates RouteStats (Layer 1).
    pub async fn record_success(&self, class_name: &str, latency_ms: u64) {
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
        }
    }

    /// Record a failure for a route. Fires a Signal if the route degrades (Layer 2 → Layer 3).
    pub async fn record_failure(&self, class_name: &str) {
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
    pub async fn record_correction(&self, class_name: &str) {
        let mut plan = self.plan.write().await;
        if let Some(route) = plan.routes.get_mut(class_name) {
            route.stats.correction_count += 1;
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

    /// Update the availability state of an engine (called by ModelHealthWatcher).
    /// Key is "provider:model" — must match the ModelSpec in the plan.
    pub fn set_engine_health(&self, key: &str, available: bool) {
        self.engine_health.lock().insert(key.to_string(), available);
    }

    /// Check if an engine is currently available.
    /// Returns `true` (optimistic) when not yet probed.
    pub fn is_engine_available(&self, key: &str) -> bool {
        *self.engine_health.lock().get(key).unwrap_or(&true)
    }

    /// Check if the engine for a ModelSpec is healthy.
    fn is_engine_healthy(&self, spec: &crate::model_plan::ModelSpec) -> bool {
        let key = format!("{}:{}", spec.provider, spec.model);
        self.is_engine_available(&key)
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
            std::iter::once((0, &route.primary))
            .chain(route.fallbacks.iter().enumerate().map(|(i, f)| (i + 1, f)))
            .collect();

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
            std::iter::once((0, &route.primary))
            .chain(route.fallbacks.iter().enumerate().map(|(i, f)| (i + 1, f)))
            .collect();

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

    fn make_router() -> (SmartRouter, mpsc::Receiver<Signal>) {
        let plan = ModelPlan::default_plan(&[
            "ollama:qwen3.5:35b".to_string(),
            "ollama:qwen3.5:9b".to_string(),
        ]);
        let plan_arc = Arc::new(RwLock::new(plan));
        let (tx, rx) = mpsc::channel(32);
        let router = SmartRouter::new(plan_arc, tx);
        (router, rx)
    }

    #[tokio::test]
    async fn routes_to_primary_when_rate_limit_is_ok() {
        let (router, _rx) = make_router();
        // Register primary with healthy (50%) remaining capacity
        router.register_rate_limit_state("ollama:qwen3.5:35b", make_ok_state());
        let decision = router.select_for_class("Analytical").await;
        assert_eq!(decision.fallback_index, 0, "should use primary when rate limit is healthy");
    }

    #[tokio::test]
    async fn routes_to_fallback_when_primary_near_limit() {
        let (router, _rx) = make_router();
        // Register the Analytical primary as near-limit
        let state = make_near_limit_state();
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        let decision = router.select_for_class("Analytical").await;
        // Should have used a fallback (fallback_index > 0)
        assert!(decision.fallback_index > 0, "expected fallback route, got primary");
    }

    #[tokio::test]
    async fn fires_signal_on_first_near_limit_crossing() {
        let (router, mut rx) = make_router();
        let state = make_near_limit_state();
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        router.select_for_class("Analytical").await;
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
        let (router, mut rx) = make_router();
        let state = Arc::new(ParkingRwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(50),
            near_limit_notified: true, // already fired — flag was set by a prior routing call
            ..Default::default()
        }));
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        let decision = router.select_for_class("Analytical").await;
        // No new Signal (flag was already set)
        assert!(rx.try_recv().is_err(), "should not fire duplicate Signal");
        // But routing must still avoid the primary — signaling and routing are independent
        assert!(decision.fallback_index > 0, "should still route to fallback when near-limit, even if no new signal fired");
    }

    #[tokio::test]
    async fn route_returns_decision() {
        let (router, _rx) = make_router();
        let (decision, _escalate) = router.route("implement a rust function").await;
        assert!(!decision.class_name.is_empty());
        assert!(!decision.model_spec.model.is_empty());
    }

    #[tokio::test]
    async fn record_success_clears_failures() {
        let (router, _rx) = make_router();
        router.record_failure("Technical").await;
        router.record_failure("Technical").await;
        router.record_success("Technical", 500).await;
        let health = router.route_health_snapshot();
        let h = health.get("Technical").unwrap();
        assert_eq!(h.consecutive_failures, 0);
        assert!(!h.degraded);
    }

    #[tokio::test]
    async fn record_failure_degrades_after_threshold() {
        let (router, mut rx) = make_router();
        router.record_failure("Technical").await;
        router.record_failure("Technical").await;
        router.record_failure("Technical").await; // threshold hit

        let health = router.route_health_snapshot();
        assert!(health["Technical"].degraded);

        // Should have sent a signal
        let signal = rx.try_recv();
        assert!(signal.is_ok());
        assert!(signal.unwrap().summary.contains("degraded"));
    }

    #[tokio::test]
    async fn degraded_route_falls_back() {
        let (router, _rx) = make_router();
        // Mark "Analytical" primary as degraded
        {
            let mut h = router.route_health.lock();
            h.entry("Analytical".to_string()).or_default().degraded = true;
        }
        let decision = router.select_for_class("Analytical").await;
        assert_eq!(decision.fallback_index, 1); // using first fallback
    }

    #[tokio::test]
    async fn route_stats_accumulate() {
        let (router, _rx) = make_router();
        router.record_success("Technical", 300).await;
        router.record_success("Technical", 700).await;
        let stats = router.route_stats_snapshot().await;
        let s = &stats["Technical"];
        assert_eq!(s.turn_count, 2);
        assert_eq!(s.avg_latency_ms(), Some(500));
    }

    #[tokio::test]
    async fn prohibited_provider_never_selected() {
        let (router, _rx) = make_router();
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
}
