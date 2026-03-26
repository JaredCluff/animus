//! Smart Router — routes AILF reasoning turns to the appropriate model.
//!
//! The router consults the `ModelPlan` to classify each input and select the best engine.
//! Route health is tracked in Layer 1 (no LLM). Degradation fires a single Signal (Layer 3).
//!
//! # Thread-local stability
//! The router is consulted **at thread start**, not per-turn. Once a thread selects a model,
//! it uses that model for all subsequent turns unless the model fails. This preserves
//! reasoning continuity within a conversation.

use crate::model_plan::{HeuristicClassifier, ModelPlan, ModelSpec, RouteStats};
use animus_core::identity::ThreadId;
use animus_core::threading::{Signal, SignalPriority};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use parking_lot::Mutex;

/// Threshold: confidence below this triggers Perception engine classification.
const HEURISTIC_CONFIDENCE_THRESHOLD: f32 = 0.5;

/// Consecutive failures before marking a route as degraded.
const DEGRADATION_THRESHOLD: u32 = 3;

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
    rate_limit_states: Arc<Mutex<HashMap<String, std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>>>>,
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

    /// Register a rate limit state handle for a model.
    /// Call once per engine at startup: `router.register_rate_limit_state(engine.model_name(), engine.rate_limit_state().unwrap())`.
    pub fn register_rate_limit_state(
        &self,
        model_name: &str,
        state: std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>,
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

    fn make_near_limit_state() -> std::sync::Arc<ParkingRwLock<RateLimitState>> {
        std::sync::Arc::new(ParkingRwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(50), // 5% — near limit
            near_limit_notified: false,
            ..Default::default()
        }))
    }

    fn make_ok_state() -> std::sync::Arc<ParkingRwLock<RateLimitState>> {
        std::sync::Arc::new(ParkingRwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(500), // 50% — fine
            near_limit_notified: false,
            ..Default::default()
        }))
    }

    #[tokio::test]
    async fn register_rate_limit_state_stores_handle() {
        let (router, _rx) = make_router();
        let state = make_ok_state();
        // Analytical primary is "ollama:qwen3.5:35b" in the test plan
        router.register_rate_limit_state("ollama:qwen3.5:35b", state.clone());
        // verify it doesn't panic and the map has an entry
        // (no public accessor — test via routing behavior below)
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
        // Signal should have been sent
        let signal = rx.try_recv();
        assert!(signal.is_ok(), "expected a Signal to be fired");
        assert_eq!(signal.unwrap().priority, SignalPriority::Normal);
    }

    #[tokio::test]
    async fn does_not_fire_duplicate_signal_when_already_notified() {
        let (router, mut rx) = make_router();
        let state = std::sync::Arc::new(ParkingRwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(50),
            near_limit_notified: true, // already fired — flag was set by a prior routing call
            ..Default::default()
        }));
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        router.select_for_class("Analytical").await;
        // No new Signal
        assert!(rx.try_recv().is_err(), "should not fire duplicate Signal");
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
}
