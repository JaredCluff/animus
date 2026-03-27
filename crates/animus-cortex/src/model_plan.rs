//! Self-Configuring Model Plan — living routing knowledge for the AILF.
//!
//! Animus builds its own cognitive routing plan from available models at startup.
//! The plan is persisted, reused until the config changes, and accumulates runtime
//! performance data (`RouteStats`) so the AILF can reflect on its own routing quality.
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

/// A routing entry for one task class: primary model + fallback chain + live stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub primary: ModelSpec,
    pub fallbacks: Vec<ModelSpec>,
    #[serde(default)]
    pub stats: RouteStats,
}

// ---------------------------------------------------------------------------
// TaskClass
// ---------------------------------------------------------------------------

/// An LLM-defined task classification.
/// Keywords are compiled into heuristic patterns by `HeuristicClassifier`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskClass {
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
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

    /// Build a rule-based default plan when no model is reachable at bootstrap.
    /// Assigns models based on simple size heuristics (more models = more routes).
    pub fn default_plan(available_models: &[String]) -> Self {
        let now = Utc::now();
        let config_hash = Self::config_hash_for(available_models);

        // Sort models by a rough size proxy: longer names / larger numbers = bigger model
        let mut sorted = available_models.to_vec();
        sorted.sort_by(|a, b| {
            // Extract trailing numbers as proxy for param count (e.g., "qwen3.5:35b" → 35)
            let size_a = extract_size_hint(a);
            let size_b = extract_size_hint(b);
            size_b.cmp(&size_a) // descending: biggest first
        });

        let largest = sorted.first().map(|s| s.as_str()).unwrap_or("unknown");
        let smallest = sorted.last().map(|s| s.as_str()).unwrap_or("unknown");
        let second = sorted.get(1).map(|s| s.as_str()).unwrap_or(largest);

        // Determine providers from model names
        let provider_for = |model: &str| -> String {
            if model.contains('/') || model.starts_with("claude") || model.starts_with("gpt") {
                "anthropic".to_string()
            } else {
                "ollama".to_string()
            }
        };

        let task_classes = vec![
            TaskClass {
                name: "Conversational".to_string(),
                description: "Casual chat, greetings, simple questions, short exchanges".to_string(),
                keywords: vec!["hello".to_string(), "hi ".to_string(), "thanks".to_string(), "okay".to_string(), "sure".to_string()],
            },
            TaskClass {
                name: "Analytical".to_string(),
                description: "Deep analysis, reasoning, complex problem solving".to_string(),
                keywords: vec!["analyze".to_string(), "explain".to_string(), "why ".to_string(), "how does".to_string(), "compare".to_string(), "evaluate".to_string()],
            },
            TaskClass {
                name: "Technical".to_string(),
                description: "Code, debugging, architecture, systems".to_string(),
                keywords: vec!["code".to_string(), "function".to_string(), "debug".to_string(), "implement".to_string(), "rust".to_string(), "error".to_string()],
            },
            TaskClass {
                name: "ToolExecution".to_string(),
                description: "Tasks requiring tool use, file operations, web access".to_string(),
                keywords: vec!["fetch".to_string(), "read file".to_string(), "write".to_string(), "search".to_string(), "run".to_string()],
            },
        ];

        let mut routes = HashMap::new();
        routes.insert("Conversational".to_string(), Route {
            primary: ModelSpec { provider: provider_for(smallest), model: smallest.to_string(), think: ThinkLevel::Off, cost: None, speed: None, quality: None, trust_floor: 0 },
            fallbacks: vec![ModelSpec { provider: provider_for(largest), model: largest.to_string(), think: ThinkLevel::Dynamic, cost: None, speed: None, quality: None, trust_floor: 0 }],
            stats: RouteStats::default(),
        });
        routes.insert("Analytical".to_string(), Route {
            primary: ModelSpec { provider: provider_for(largest), model: largest.to_string(), think: ThinkLevel::Dynamic, cost: None, speed: None, quality: None, trust_floor: 0 },
            fallbacks: vec![ModelSpec { provider: provider_for(second), model: second.to_string(), think: ThinkLevel::Dynamic, cost: None, speed: None, quality: None, trust_floor: 0 }],
            stats: RouteStats::default(),
        });
        routes.insert("Technical".to_string(), Route {
            primary: ModelSpec { provider: provider_for(second), model: second.to_string(), think: ThinkLevel::Dynamic, cost: None, speed: None, quality: None, trust_floor: 0 },
            fallbacks: vec![ModelSpec { provider: provider_for(largest), model: largest.to_string(), think: ThinkLevel::Dynamic, cost: None, speed: None, quality: None, trust_floor: 0 }],
            stats: RouteStats::default(),
        });
        routes.insert("ToolExecution".to_string(), Route {
            primary: ModelSpec { provider: provider_for(largest), model: largest.to_string(), think: ThinkLevel::Off, cost: None, speed: None, quality: None, trust_floor: 0 },
            fallbacks: vec![ModelSpec { provider: provider_for(second), model: second.to_string(), think: ThinkLevel::Off, cost: None, speed: None, quality: None, trust_floor: 0 }],
            stats: RouteStats::default(),
        });

        Self {
            id: Uuid::new_v4(),
            created: now,
            config_hash,
            task_classes,
            routes,
            build_reason: "Rule-based default (no LLM reachable at bootstrap)".to_string(),
        }
    }

    /// Extract the JSON plan prompt to send to the LLM for plan building.
    pub fn build_prompt(available_models: &[String]) -> String {
        let model_list = available_models.join(", ");
        format!(
            r#"You are configuring your own cognitive routing plan.

Available models: {model_list}

Your task:
1. Define 4–6 task classes that cover the types of inputs you handle.
   For each class, provide: a name, a description, and 5–10 characteristic keywords.
2. Assign each task class a primary model and 1–2 fallback models from the available list.
3. Specify the think budget per model assignment.
   Think budget options: "off", "dynamic", "minimal_N" (e.g. "minimal_4000"), "full_N" (e.g. "full_8000").
   Use "dynamic" for models with thinking capability (Qwen3-style or Claude extended thinking).
   Use "off" for fast/small models or tool execution tasks.

Respond with JSON only (no markdown, no explanation):
{{
  "task_classes": [
    {{"name": "ClassName", "description": "What this class covers", "keywords": ["kw1", "kw2", ...]}}
  ],
  "routes": {{
    "ClassName": {{
      "primary": {{"provider": "ollama|anthropic|openai", "model": "model-name", "think": "dynamic"}},
      "fallbacks": [
        {{"provider": "ollama", "model": "smaller-model", "think": "off"}}
      ]
    }}
  }},
  "build_reason": "Brief explanation of your routing decisions"
}}"#,
            model_list = model_list
        )
    }

    /// Parse a plan from an LLM response string.
    /// Handles JSON wrapped in markdown code blocks.
    pub fn parse_from_response(response: &str, config_hash: String) -> Option<Self> {
        let json_str = extract_json(response)?;

        #[derive(Deserialize)]
        struct PlanResponse {
            task_classes: Vec<TaskClass>,
            routes: HashMap<String, RouteJson>,
            build_reason: Option<String>,
        }

        #[derive(Deserialize)]
        struct RouteJson {
            primary: ModelSpecJson,
            #[serde(default)]
            fallbacks: Vec<ModelSpecJson>,
        }

        #[derive(Deserialize)]
        struct ModelSpecJson {
            provider: String,
            model: String,
            think: Option<String>,
        }

        fn parse_think(s: Option<&str>) -> ThinkLevel {
            match s {
                None | Some("dynamic") => ThinkLevel::Dynamic,
                Some("off") => ThinkLevel::Off,
                Some(s) if s.starts_with("minimal_") => {
                    s[8..].parse::<u32>().map(ThinkLevel::Minimal).unwrap_or(ThinkLevel::Dynamic)
                }
                Some(s) if s.starts_with("full_") => {
                    s[5..].parse::<u32>().map(ThinkLevel::Full).unwrap_or(ThinkLevel::Full(8000))
                }
                _ => ThinkLevel::Dynamic,
            }
        }

        let parsed: PlanResponse = serde_json::from_str(json_str).ok()?;

        // Validate: every route class must have a matching task_class entry
        let class_names: std::collections::HashSet<&str> =
            parsed.task_classes.iter().map(|c| c.name.as_str()).collect();
        for route_name in parsed.routes.keys() {
            if !class_names.contains(route_name.as_str()) {
                tracing::warn!("model plan parse: route '{}' has no matching task_class — skipping plan", route_name);
                return None;
            }
        }

        let routes = parsed.routes.into_iter().map(|(name, r)| {
            let route = Route {
                primary: ModelSpec {
                    provider: r.primary.provider,
                    model: r.primary.model,
                    think: parse_think(r.primary.think.as_deref()),
                    cost: None,
                    speed: None,
                    quality: None,
                    trust_floor: 0,
                },
                fallbacks: r.fallbacks.into_iter().map(|f| ModelSpec {
                    provider: f.provider,
                    model: f.model,
                    think: parse_think(f.think.as_deref()),
                    cost: None,
                    speed: None,
                    quality: None,
                    trust_floor: 0,
                }).collect(),
                stats: RouteStats::default(),
            };
            (name, route)
        }).collect();

        Some(Self {
            id: Uuid::new_v4(),
            created: Utc::now(),
            config_hash,
            task_classes: parsed.task_classes,
            routes,
            build_reason: parsed.build_reason.unwrap_or_else(|| "LLM-built plan".to_string()),
        })
    }
}

/// Extract JSON from an LLM response that may be wrapped in markdown code blocks.
fn extract_json(s: &str) -> Option<&str> {
    let s = s.trim();
    // Strip ```json ... ``` or ``` ... ```
    if let Some(inner) = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")) {
        if let Some(end) = inner.rfind("```") {
            return Some(inner[..end].trim());
        }
    }
    // Plain JSON
    if s.starts_with('{') {
        Some(s)
    } else {
        // Try to find the first '{' in the response
        s.find('{').map(|i| s[i..].trim())
    }
}

/// Extract a rough size hint (parameter count) from a model name string.
fn extract_size_hint(model: &str) -> u64 {
    // Look for patterns like "35b", "9b", "70b", "120b", "0.8b"
    let lower = model.to_lowercase();
    for part in lower.split(|c: char| !c.is_alphanumeric() && c != '.') {
        if part.ends_with('b') {
            let num_str = &part[..part.len() - 1];
            if let Ok(n) = num_str.parse::<f64>() {
                return (n * 10.0) as u64; // multiply by 10 to preserve decimal (e.g., 0.8 → 8)
            }
        }
    }
    0
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

    #[test]
    fn route_stats_empty() {
        let stats = RouteStats::default();
        assert_eq!(stats.avg_latency_ms(), None);
        assert_eq!(stats.success_rate(), 1.0);
        assert_eq!(stats.correction_rate(), 0.0);
    }

    #[test]
    fn route_stats_with_data() {
        let stats = RouteStats {
            turn_count: 10,
            failure_count: 2,
            total_latency_ms: 5000,
            correction_count: 1,
            last_turn: None,
        };
        assert_eq!(stats.avg_latency_ms(), Some(500));
        assert!((stats.success_rate() - 0.8).abs() < 0.001);
        assert!((stats.correction_rate() - 0.1).abs() < 0.001);
    }

    #[test]
    fn config_hash_is_order_stable() {
        let h1 = ModelPlan::config_hash_for(&["ollama:qwen3.5:35b".to_string(), "anthropic:claude-sonnet-4-6".to_string()]);
        let h2 = ModelPlan::config_hash_for(&["anthropic:claude-sonnet-4-6".to_string(), "ollama:qwen3.5:35b".to_string()]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn config_hash_changes_on_new_model() {
        let h1 = ModelPlan::config_hash_for(&["ollama:qwen3.5:35b".to_string()]);
        let h2 = ModelPlan::config_hash_for(&["ollama:qwen3.5:35b".to_string(), "ollama:qwen3.5:9b".to_string()]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn default_plan_builds_from_models() {
        let plan = ModelPlan::default_plan(&[
            "ollama:qwen3.5:35b".to_string(),
            "ollama:qwen3.5:9b".to_string(),
        ]);
        assert!(!plan.routes.is_empty());
        assert!(!plan.task_classes.is_empty());
        assert_eq!(plan.build_reason.contains("Rule-based"), true);
    }

    #[test]
    fn extract_json_handles_markdown_wrapper() {
        let resp = "```json\n{\"foo\": 1}\n```";
        assert_eq!(extract_json(resp), Some("{\"foo\": 1}"));
    }

    #[test]
    fn extract_json_handles_plain() {
        let resp = r#"{"foo": 1}"#;
        assert_eq!(extract_json(resp), Some(r#"{"foo": 1}"#));
    }

    #[test]
    fn heuristic_classifier_basic() {
        let plan = ModelPlan::default_plan(&["ollama:qwen3.5:35b".to_string()]);
        let classifier = HeuristicClassifier::from_plan(&plan);

        let (class, conf) = classifier.classify("implement a rust function to parse JSON");
        assert_eq!(class, "Technical");
        assert!(conf >= 0.5);

        let (class, _) = classifier.classify("hello how are you");
        assert_eq!(class, "Conversational");
    }

    #[test]
    fn heuristic_classifier_low_confidence_on_ambiguous() {
        let plan = ModelPlan::default_plan(&["ollama:qwen3.5:35b".to_string()]);
        let classifier = HeuristicClassifier::from_plan(&plan);
        // No keywords match → low confidence
        let (_, conf) = classifier.classify("xyzzy plugh foobar");
        assert!(conf < 0.5);
    }

    #[test]
    fn plan_parse_from_response() {
        let response = r#"{
  "task_classes": [
    {"name": "Technical", "description": "Code tasks", "keywords": ["code", "function", "debug"]},
    {"name": "Conversational", "description": "Chat", "keywords": ["hello", "thanks"]}
  ],
  "routes": {
    "Technical": {
      "primary": {"provider": "ollama", "model": "qwen3.5:35b", "think": "dynamic"},
      "fallbacks": [{"provider": "ollama", "model": "qwen3.5:9b", "think": "off"}]
    },
    "Conversational": {
      "primary": {"provider": "ollama", "model": "qwen3.5:9b", "think": "off"},
      "fallbacks": []
    }
  },
  "build_reason": "test plan"
}"#;
        let plan = ModelPlan::parse_from_response(response, "testhash".to_string());
        assert!(plan.is_some());
        let plan = plan.unwrap();
        assert_eq!(plan.task_classes.len(), 2);
        assert!(plan.routes.contains_key("Technical"));
        assert_eq!(plan.routes["Technical"].primary.model, "qwen3.5:35b");
        assert_eq!(plan.routes["Technical"].primary.think, ThinkLevel::Dynamic);
    }

    #[test]
    fn plan_serde_roundtrip() {
        let plan = ModelPlan::default_plan(&["ollama:qwen3.5:35b".to_string()]);
        let json = serde_json::to_string(&plan).unwrap();
        let restored: ModelPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan.id, restored.id);
        assert_eq!(plan.config_hash, restored.config_hash);
    }

    #[test]
    fn plan_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("model_plan.json");
        let plan = ModelPlan::default_plan(&["ollama:qwen3.5:35b".to_string()]);
        plan.save(&path).unwrap();
        let loaded = ModelPlan::load(&path).unwrap();
        assert_eq!(plan.id, loaded.id);
    }
}
