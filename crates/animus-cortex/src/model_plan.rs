//! Self-Configuring Model Plan — living routing knowledge for the AILF.
//!
//! Animus builds its own cognitive routing plan from available models at startup
//! using capability profiles from the `CapabilityRegistry`. The plan is persisted,
//! reused until the config changes, and accumulates runtime performance data
//! (`RouteStats`) so the AILF can reflect on its own routing quality.
//!
//! # Three-layer compliance
//! - Layer 1: `RouteStats` and `RouteHealth` tracking — pure data, no LLM
//! - Layer 2: `SmartRouter` detects route degradation (consecutive failures)
//! - Layer 3: Signal fires once on degradation; AILF reasoning thread decides what to do

use animus_core::error::{AnimusError, Result};
use animus_core::{CostTier, QualityTier, SpeedTier};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ThinkLevel
// ---------------------------------------------------------------------------

/// How much extended thinking to apply for a model call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "budget")]
pub enum ThinkLevel {
    /// Never use extended thinking.
    Off,
    /// Use the `needs_thinking()` heuristic at call time (default for supporting models).
    Dynamic,
    /// Extended thinking with a fixed token budget.
    Minimal(u32),
    /// Maximum thinking with the given token budget.
    Full(u32),
}

// ---------------------------------------------------------------------------
// ModelSpec
// ---------------------------------------------------------------------------

/// A model + provider + think budget combination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub provider: String,   // "anthropic" | "ollama" | "openai"
    pub model: String,
    pub think: ThinkLevel,
    /// Cost tier for budget-pressure routing. None = assume Moderate (conservative).
    #[serde(default)]
    pub cost: Option<CostTier>,
    /// Speed tier for latency-sensitive routing.
    #[serde(default)]
    pub speed: Option<SpeedTier>,
    /// Quality tier for task-class routing.
    #[serde(default)]
    pub quality: Option<QualityTier>,
    /// Minimum provider effective_trust required to use this model. 0 = any provider.
    #[serde(default)]
    pub trust_floor: u8,
}

// ---------------------------------------------------------------------------
// RouteStats — Layer 1 performance tracking, no LLM
// ---------------------------------------------------------------------------

/// Running performance statistics for a route.
/// Accumulated by the Cortex substrate from actual usage — no LLM involvement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouteStats {
    pub turn_count: u64,
    pub failure_count: u64,
    /// Sum of all measured latencies; divide by turn_count for average.
    pub total_latency_ms: u64,
    /// Incremented when the quality gate records a correction on a turn that used this route.
    pub correction_count: u64,
    pub last_turn: Option<DateTime<Utc>>,
}

impl RouteStats {
    pub fn avg_latency_ms(&self) -> Option<u64> {
        if self.turn_count == 0 {
            None
        } else {
            Some(self.total_latency_ms / self.turn_count)
        }
    }

    pub fn success_rate(&self) -> f32 {
        if self.turn_count == 0 {
            1.0
        } else {
            1.0 - (self.failure_count as f32 / self.turn_count as f32)
        }
    }

    pub fn correction_rate(&self) -> f32 {
        if self.turn_count == 0 {
            0.0
        } else {
            self.correction_count as f32 / self.turn_count as f32
        }
    }
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

/// A routing entry for one task class: ranked candidate list + live stats.
/// Index 0 of `candidates` is the highest-scoring model at plan-build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// Ranked candidate list — index 0 is the highest initial scorer.
    pub candidates: Vec<ModelSpec>,
    /// Per-model stats keyed by "provider:model".
    #[serde(default)]
    pub model_stats: HashMap<String, RouteStats>,
    /// Class-level aggregate stats.
    #[serde(default)]
    pub stats: RouteStats,
}

impl Route {
    /// Convenience: primary model (index 0).
    pub fn primary(&self) -> Option<&ModelSpec> {
        self.candidates.first()
    }

    /// Convenience: fallbacks (index 1+).
    pub fn fallbacks(&self) -> &[ModelSpec] {
        if self.candidates.len() > 1 {
            &self.candidates[1..]
        } else {
            &[]
        }
    }

    /// Learned quality for a specific model.
    /// Returns `None` when fewer than 5 turns recorded (insufficient data).
    pub fn learned_quality_for(&self, model_key: &str) -> Option<f32> {
        let stats = self.model_stats.get(model_key)?;
        if stats.turn_count < 5 {
            return None;
        }
        let q = stats.success_rate() * (1.0 - stats.correction_rate());
        Some(q.clamp(0.0, 1.0))
    }
}

// ---------------------------------------------------------------------------
// TaskClass
// ---------------------------------------------------------------------------

/// A task classification with routing weight hints.
/// Keywords are compiled into heuristic patterns by `HeuristicClassifier`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskClass {
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,

    #[serde(default = "default_weight_quality")]
    pub weight_quality: f32,
    #[serde(default = "default_weight_speed")]
    pub weight_speed: f32,
    #[serde(default = "default_weight_reasoning")]
    pub weight_reasoning: f32,
    #[serde(default = "default_weight_cost")]
    pub weight_cost: f32,
    #[serde(default)]
    pub latency_budget_ms: Option<u32>,
}

fn default_weight_quality()   -> f32 { 0.5 }
fn default_weight_speed()     -> f32 { 0.3 }
fn default_weight_reasoning() -> f32 { 0.3 }
fn default_weight_cost()      -> f32 { 0.3 }

impl TaskClass {
    /// Convert to `TaskWeights` for use with `ModelScorer`.
    pub fn to_weights(&self) -> crate::model_scorer::TaskWeights {
        crate::model_scorer::TaskWeights {
            weight_quality: self.weight_quality,
            weight_speed: self.weight_speed,
            weight_reasoning: self.weight_reasoning,
            weight_cost: self.weight_cost,
            latency_budget_ms: self.latency_budget_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// ModelPlan
// ---------------------------------------------------------------------------

/// The full routing plan — built by Animus, persisted, reused until config changes.
///
/// This is living knowledge: `RouteStats` accumulate from actual usage so the AILF
/// can reflect on its own routing quality via introspective tools and propose amendments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPlan {
    pub id: Uuid,
    pub created: DateTime<Utc>,
    /// sha256 of sorted "provider:model" strings — used to detect config changes.
    pub config_hash: String,
    pub task_classes: Vec<TaskClass>,
    /// class_name → Route
    pub routes: HashMap<String, Route>,
    pub build_reason: String,
}

impl ModelPlan {
    /// Load a plan from disk. Returns None if the file doesn't exist or is invalid.
    pub fn load(path: &Path) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// Persist the plan to disk atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| AnimusError::Storage(format!("model plan serialize: {e}")))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json)
            .map_err(|e| AnimusError::Storage(format!("model plan write: {e}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| AnimusError::Storage(format!("model plan rename: {e}")))
    }

    /// Compute a config hash from the available model set.
    /// Sorted to be stable regardless of discovery order.
    pub fn config_hash_for(models: &[String]) -> String {
        let mut sorted = models.to_vec();
        sorted.sort();
        sorted.dedup();
        let mut hasher = Sha256::new();
        for m in &sorted {
            hasher.update(m.as_bytes());
            hasher.update(b"|");
        }
        hex::encode(hasher.finalize())
    }

    /// Build a plan deterministically from capability profiles — no LLM required.
    pub fn build_from_capabilities(
        registry: &crate::capability_registry::CapabilityRegistry,
        available: &[String],
        task_classes: Vec<TaskClass>,
    ) -> Self {
        use crate::model_scorer::{ModelScorer, ScoringContext};
        use animus_core::budget::BudgetPressure;

        let config_hash = Self::config_hash_for(available);
        let mut routes = HashMap::new();

        // Optimistic context at plan-build time (no live state yet)
        let build_ctx = ScoringContext {
            rate_limit_remaining_pct: 1.0,
            rate_limit_rpm_ceiling: None,
            budget_pressure: BudgetPressure::Normal,
            engine_available: true,
            learned_quality: None,
        };

        for task_class in &task_classes {
            let weights = task_class.to_weights();
            let mut scored: Vec<(ModelSpec, f32)> = available
                .iter()
                .filter_map(|key| {
                    let (provider, model) = key.split_once(':')?;
                    let profile = registry.get(provider, model)?;
                    if profile.trust_score == 0 {
                        return None;
                    }
                    let score = ModelScorer::score(profile, &weights, &build_ctx);
                    if score == 0.0 {
                        return None;
                    }
                    let think = think_level_for_profile(profile, task_class);
                    Some((
                        ModelSpec {
                            provider: provider.to_string(),
                            model: model.to_string(),
                            think,
                            cost: Some(profile.cost_tier),
                            speed: None,
                            quality: None,
                            trust_floor: 0,
                        },
                        score,
                    ))
                })
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let candidates: Vec<ModelSpec> = scored.into_iter().map(|(s, _)| s).collect();

            routes.insert(
                task_class.name.clone(),
                Route {
                    candidates,
                    model_stats: HashMap::new(),
                    stats: RouteStats::default(),
                },
            );
        }

        Self {
            id: Uuid::new_v4(),
            created: Utc::now(),
            config_hash,
            task_classes,
            routes,
            build_reason: "Deterministic capability scoring — zero LLM tokens".to_string(),
        }
    }

    /// Generate a prompt asking the LLM to annotate (not build) the existing plan.
    pub fn annotation_prompt(&self) -> String {
        let mut lines = vec![
            "Review the cognitive routing plan below. For each task class, explain in 1–2 sentences why the ranked model order makes sense, considering capability, reasoning support, speed, cost, and trust.".to_string(),
            "Your explanation will be stored as a self-knowledge segment in VectorFS.".to_string(),
            String::new(),
        ];
        for (class_name, route) in &self.routes {
            lines.push(format!("  {class_name}:"));
            for (i, spec) in route.candidates.iter().take(3).enumerate() {
                let label = if i == 0 { "primary".to_string() } else { format!("fallback {i}") };
                lines.push(format!("    {label}: {}:{} (think={:?})", spec.provider, spec.model, spec.think));
            }
        }
        lines.join("\n")
    }

    /// Deprecated: LLM-built plans are replaced by `build_from_capabilities`.
    #[deprecated(since = "0.2.0", note = "Use build_from_capabilities instead")]
    pub fn parse_from_response(_response: &str, _config_hash: String) -> Option<Self> {
        None
    }
}

// ---------------------------------------------------------------------------
// think_level_for_profile helper
// ---------------------------------------------------------------------------

fn think_level_for_profile(
    profile: &animus_core::model_capability::ModelCapabilityProfile,
    task: &TaskClass,
) -> ThinkLevel {
    use animus_core::model_capability::ReasoningSupport;
    match &profile.reasoning_support {
        ReasoningSupport::ExtendedThinking { .. } => {
            if task.weight_reasoning >= 0.4 {
                ThinkLevel::Dynamic
            } else {
                ThinkLevel::Off
            }
        }
        _ => ThinkLevel::Off,
    }
}

// ---------------------------------------------------------------------------
// default_task_classes
// ---------------------------------------------------------------------------

/// Default task classes with calibrated routing weights.
pub fn default_task_classes() -> Vec<TaskClass> {
    vec![
        TaskClass {
            name: "Conversational".to_string(),
            description: "Casual chat, greetings, short exchanges, simple questions".to_string(),
            keywords: vec!["hello".to_string(), "hi ".to_string(), "thanks".to_string(),
                           "okay".to_string(), "sure".to_string(), "what is".to_string()],
            weight_quality: 0.4, weight_speed: 0.6, weight_reasoning: 0.1,
            weight_cost: 0.4, latency_budget_ms: None,
        },
        TaskClass {
            name: "Analytical".to_string(),
            description: "Deep analysis, complex reasoning, multi-step problem solving".to_string(),
            keywords: vec!["analyze".to_string(), "explain".to_string(), "why ".to_string(),
                           "how does".to_string(), "compare".to_string(), "evaluate".to_string(),
                           "reason".to_string(), "think through".to_string()],
            weight_quality: 0.8, weight_speed: 0.1, weight_reasoning: 0.7,
            weight_cost: 0.2, latency_budget_ms: None,
        },
        TaskClass {
            name: "Technical".to_string(),
            description: "Code, debugging, architecture, systems design, implementation".to_string(),
            keywords: vec!["code".to_string(), "function".to_string(), "debug".to_string(),
                           "implement".to_string(), "rust".to_string(), "error".to_string(),
                           "fix".to_string(), "build".to_string(), "refactor".to_string()],
            weight_quality: 0.7, weight_speed: 0.3, weight_reasoning: 0.5,
            weight_cost: 0.2, latency_budget_ms: None,
        },
        TaskClass {
            name: "ToolExecution".to_string(),
            description: "Tasks requiring tool use, file operations, web access, memory search".to_string(),
            keywords: vec!["fetch".to_string(), "read file".to_string(), "write".to_string(),
                           "search".to_string(), "run".to_string(), "remember".to_string(),
                           "store".to_string(), "retrieve".to_string()],
            weight_quality: 0.5, weight_speed: 0.5, weight_reasoning: 0.2,
            weight_cost: 0.3, latency_budget_ms: None,
        },
        TaskClass {
            name: "Voice".to_string(),
            description: "Realtime voice or near-realtime responses where latency is critical".to_string(),
            keywords: vec!["voice".to_string(), "speak".to_string(), "realtime".to_string(),
                           "quickly".to_string(), "fast reply".to_string()],
            weight_quality: 0.2, weight_speed: 0.9, weight_reasoning: 0.0,
            weight_cost: 0.5, latency_budget_ms: Some(2_000),
        },
    ]
}

// ---------------------------------------------------------------------------
// HeuristicClassifier
// ---------------------------------------------------------------------------

/// Compiled from a `ModelPlan`'s `TaskClass` keywords at startup.
/// Classifies inputs at zero LLM cost; returns confidence for escalation decisions.
pub struct HeuristicClassifier {
    /// (class_name, lowercase keywords)
    patterns: Vec<(String, Vec<String>)>,
    /// Class to use when no keyword matches (the class with the most keywords wins on ties).
    default_class: String,
}

impl HeuristicClassifier {
    pub fn from_plan(plan: &ModelPlan) -> Self {
        let patterns = plan.task_classes.iter().map(|tc| {
            let kws = tc.keywords.iter().map(|k| k.to_lowercase()).collect();
            (tc.name.clone(), kws)
        }).collect();

        let default_class = plan.task_classes
            .first()
            .map(|tc| tc.name.clone())
            .unwrap_or_else(|| "Conversational".to_string());

        Self { patterns, default_class }
    }

    /// Returns (class_name, confidence).
    /// confidence >= 0.5 → heuristic is confident; < 0.5 → escalate to Perception engine.
    pub fn classify(&self, input: &str) -> (String, f32) {
        let lower = input.to_lowercase();
        let mut scores: Vec<(&str, usize)> = self.patterns.iter().map(|(name, kws)| {
            let hits = kws.iter().filter(|kw| lower.contains(kw.as_str())).count();
            (name.as_str(), hits)
        }).collect();

        // Sort descending by hit count
        scores.sort_by(|a, b| b.1.cmp(&a.1));

        let top = scores.first().copied().unwrap_or((&self.default_class, 0));
        let second = scores.get(1).copied().unwrap_or(("", 0));

        if top.1 == 0 {
            // No matches — return default with low confidence
            return (self.default_class.clone(), 0.2);
        }

        // Confidence: ratio of top to top+second; higher gap = more confident
        let total = top.1 + second.1;
        let confidence = if total == 0 {
            0.5
        } else {
            (top.1 as f32 / total as f32).min(1.0)
        };

        // Boost if clear winner
        let confidence = if second.1 == 0 { confidence.max(0.8) } else { confidence };

        (top.0.to_string(), confidence)
    }

    /// Return all patterns (for introspective tool).
    pub fn patterns(&self) -> &[(String, Vec<String>)] {
        &self.patterns
    }

    /// Add or replace keywords for a class.
    pub fn update_pattern(&mut self, class_name: &str, keywords: Vec<String>) {
        let kws_lower = keywords.into_iter().map(|k| k.to_lowercase()).collect();
        if let Some(entry) = self.patterns.iter_mut().find(|(name, _)| name == class_name) {
            entry.1 = kws_lower;
        } else {
            self.patterns.push((class_name.to_string(), kws_lower));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_registry::CapabilityRegistry;

    fn two_ollama_models() -> Vec<String> {
        vec!["ollama:qwen3.5:35b".to_string(), "ollama:qwen3.5:9b".to_string()]
    }

    #[tokio::test]
    async fn build_from_capabilities_produces_ranked_candidates() {
        let registry = CapabilityRegistry::build(None, &[], &two_ollama_models()).await;
        let task_classes = default_task_classes();
        let plan = ModelPlan::build_from_capabilities(&registry, &two_ollama_models(), task_classes);
        assert!(!plan.routes.is_empty());
        // Most routes should have candidates; Voice may be empty with slow local models
        // (latency_budget_ms filter correctly excludes models that can't meet the 2s budget)
        let non_empty = plan.routes.values().filter(|r| !r.candidates.is_empty()).count();
        assert!(non_empty >= 4, "expected at least 4 routes with candidates, got {non_empty}");
    }

    #[tokio::test]
    async fn build_from_capabilities_excludes_prohibited() {
        let available = vec!["ollama:qwen3.5:35b".to_string()];
        let registry = CapabilityRegistry::build(None, &[], &available).await;
        let plan = ModelPlan::build_from_capabilities(&registry, &available, default_task_classes());
        for route in plan.routes.values() {
            for spec in &route.candidates {
                assert_ne!(spec.provider, "qwen-api");
                assert_ne!(spec.provider, "deepseek-api");
            }
        }
    }

    #[test]
    fn route_stats_roundtrip() {
        let stats = RouteStats {
            turn_count: 5, failure_count: 1, total_latency_ms: 2500,
            correction_count: 0, last_turn: None,
        };
        assert!((stats.success_rate() - 0.8).abs() < 0.001);
        assert_eq!(stats.avg_latency_ms(), Some(500));
    }

    #[test]
    fn learned_quality_none_under_min_turns() {
        let route = Route {
            candidates: vec![],
            model_stats: {
                let mut m = HashMap::new();
                m.insert("ollama:qwen3.5:35b".to_string(), RouteStats {
                    turn_count: 3, failure_count: 0, total_latency_ms: 0,
                    correction_count: 0, last_turn: None,
                });
                m
            },
            stats: RouteStats::default(),
        };
        assert!(route.learned_quality_for("ollama:qwen3.5:35b").is_none());
    }

    #[test]
    fn learned_quality_some_after_min_turns() {
        let route = Route {
            candidates: vec![],
            model_stats: {
                let mut m = HashMap::new();
                m.insert("ollama:qwen3.5:35b".to_string(), RouteStats {
                    turn_count: 10, failure_count: 1, total_latency_ms: 5000,
                    correction_count: 1, last_turn: None,
                });
                m
            },
            stats: RouteStats::default(),
        };
        let q = route.learned_quality_for("ollama:qwen3.5:35b").unwrap();
        assert!(q > 0.0 && q <= 1.0);
    }

    #[test]
    fn config_hash_is_order_stable() {
        let h1 = ModelPlan::config_hash_for(&["ollama:qwen3.5:35b".to_string(), "anthropic:claude-sonnet-4-6".to_string()]);
        let h2 = ModelPlan::config_hash_for(&["anthropic:claude-sonnet-4-6".to_string(), "ollama:qwen3.5:35b".to_string()]);
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn plan_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("model_plan.json");
        let registry = CapabilityRegistry::build(None, &[], &two_ollama_models()).await;
        let plan = ModelPlan::build_from_capabilities(&registry, &two_ollama_models(), default_task_classes());
        plan.save(&path).unwrap();
        let loaded = ModelPlan::load(&path).unwrap();
        assert_eq!(plan.id, loaded.id);
        assert_eq!(plan.config_hash, loaded.config_hash);
    }

    #[test]
    fn heuristic_classifier_basic() {
        let classes = default_task_classes();
        let plan = ModelPlan {
            id: uuid::Uuid::new_v4(), created: chrono::Utc::now(),
            config_hash: "x".to_string(), task_classes: classes,
            routes: HashMap::new(),
            build_reason: "test".to_string(),
        };
        let classifier = HeuristicClassifier::from_plan(&plan);
        let (class, conf) = classifier.classify("implement a rust function to parse JSON");
        assert_eq!(class, "Technical");
        assert!(conf >= 0.5);
    }

    #[test]
    fn config_hash_changes_on_new_model() {
        let h1 = ModelPlan::config_hash_for(&["ollama:qwen3.5:35b".to_string()]);
        let h2 = ModelPlan::config_hash_for(&["ollama:qwen3.5:35b".to_string(), "ollama:qwen3.5:9b".to_string()]);
        assert_ne!(h1, h2);
    }
}
