use animus_core::sensorium::*;
use serde::{Deserialize, Serialize};

/// What to do when a rule matches an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleAction {
    Ignore,
    Promote,
}

/// A rule for Tier 1 attention filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionRule {
    pub event_types: Vec<EventType>,
    pub path_patterns: Vec<String>,
    pub action: RuleAction,
}

/// Two-tier attention filter for event triage.
pub struct AttentionFilter {
    rules: Vec<AttentionRule>,
}

impl AttentionFilter {
    pub fn new(rules: Vec<AttentionRule>) -> Self {
        Self { rules }
    }

    /// Tier 1: Fast rule-based evaluation (microseconds).
    pub fn tier1_evaluate(&self, event: &SensorEvent) -> AttentionDecision {
        for rule in &self.rules {
            if !rule.event_types.contains(&event.event_type) {
                continue;
            }
            if self.rule_matches(rule, event) {
                return match rule.action {
                    RuleAction::Ignore => AttentionDecision::Drop {
                        reason: "matched ignore rule".to_string(),
                    },
                    RuleAction::Promote => AttentionDecision::Pass { promoted: true },
                };
            }
        }
        AttentionDecision::Pass { promoted: false }
    }

    /// Tier 2: Embedding similarity evaluation.
    pub fn tier2_evaluate(
        &self,
        event_embedding: &[f32],
        goal_embeddings: &[Vec<f32>],
        threshold: f32,
    ) -> AttentionDecision {
        let max_similarity = goal_embeddings
            .iter()
            .map(|goal| cosine_similarity(event_embedding, goal))
            .fold(f32::NEG_INFINITY, f32::max);

        if max_similarity >= threshold {
            AttentionDecision::Pass { promoted: true }
        } else {
            AttentionDecision::Drop {
                reason: format!(
                    "below attention threshold: {max_similarity:.3} < {threshold:.3}"
                ),
            }
        }
    }

    fn rule_matches(&self, rule: &AttentionRule, event: &SensorEvent) -> bool {
        if rule.path_patterns.is_empty() {
            return true;
        }
        if let Some(path) = event.data.get("path").and_then(|v| v.as_str()) {
            rule.path_patterns.iter().any(|p| glob_match(p, path))
        } else {
            false
        }
    }
}

fn glob_match(pattern: &str, path: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("**/") {
        path.ends_with(suffix) || path.contains(&format!("/{suffix}"))
    } else if let Some(prefix) = pattern.strip_suffix("/**") {
        path.starts_with(prefix)
    } else if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            path.starts_with(parts[0]) && path.ends_with(parts[1])
        } else {
            path == pattern
        }
    } else {
        path == pattern
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }
    dot / (mag_a * mag_b)
}
