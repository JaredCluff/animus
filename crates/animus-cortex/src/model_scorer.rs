//! Deterministic multi-dimensional model scorer.
//!
//! Scores a candidate `ModelCapabilityProfile` against task routing preferences and
//! current runtime state. Zero LLM involvement.
//! Constitution Principle 8 (LLMs as Analytical Resource) and
//! Principle 9 (Animus in Charge of Animus).
//!
//! Score is a weighted sum in [0.0, 1.0]. Hard disqualifiers (unavailable, prohibited,
//! budget exceeded) short-circuit to 0.0.

use animus_core::budget::BudgetPressure;
use animus_core::model_capability::{ModelCapabilityProfile, PrefillSpeed, ReasoningSupport};
use animus_core::provider_meta::CostTier;

/// Routing preference weights for one task class.
/// Passed to `ModelScorer::score` at routing time.
/// Task 5 will add these same fields to `model_plan::TaskClass` and provide a conversion.
#[derive(Debug, Clone)]
pub struct TaskWeights {
    /// How much raw model capability (parameter count × recency) matters.
    pub weight_quality: f32,
    /// How much generation speed (tok/s / TTFT) matters.
    pub weight_speed: f32,
    /// How much extended thinking/reasoning capability helps.
    pub weight_reasoning: f32,
    /// How strongly to prefer lower-cost models.
    pub weight_cost: f32,
    /// Hard TTFT cap in milliseconds. Candidates exceeding this are scored 0.
    /// `None` = no hard latency constraint.
    pub latency_budget_ms: Option<u32>,
}

/// Runtime state snapshot for one candidate at the moment of routing.
#[derive(Debug, Clone)]
pub struct ScoringContext {
    /// Fraction of rate limit remaining: 0.0 = exhausted, 1.0 = full.
    pub rate_limit_remaining_pct: f32,
    /// Absolute requests-per-minute ceiling (from profile or observed headers).
    pub rate_limit_rpm_ceiling: Option<u32>,
    /// Current monthly budget pressure level.
    pub budget_pressure: BudgetPressure,
    /// Whether ModelHealthWatcher considers this engine reachable.
    pub engine_available: bool,
    /// Learned quality from per-model RouteStats for this class.
    /// `None` when < 5 turns recorded — not enough data. Defaults to 0.5 (neutral).
    pub learned_quality: Option<f32>,
}

/// Deterministic model scorer.
pub struct ModelScorer;

impl ModelScorer {
    /// Score a candidate for a task class and runtime context.
    ///
    /// Returns 0.0 if any hard disqualifier applies; otherwise a weighted score in (0.0, 1.0].
    pub fn score(
        profile: &ModelCapabilityProfile,
        weights: &TaskWeights,
        context: &ScoringContext,
    ) -> f32 {
        // Hard disqualifiers
        if !context.engine_available                               { return 0.0; }
        if profile.trust_score == 0                               { return 0.0; } // prohibited
        if !Self::passes_budget(profile, context.budget_pressure) { return 0.0; }

        // Latency hard cap — skip slow models if task has strict TTFT budget
        if let Some(budget_ms) = weights.latency_budget_ms {
            if !Self::passes_latency(profile, budget_ms) { return 0.0; }
        }

        let quality   = Self::quality_score(profile);
        let speed     = Self::speed_score(profile);
        let reasoning = Self::reasoning_score(profile);
        let capacity  = Self::capacity_score(context);
        let cost      = Self::cost_score(profile);
        let learned   = context.learned_quality.unwrap_or(0.5); // neutral if no history

        // Task weights govern soft preferences.
        // Capacity (0.15) and learned (0.20) are fixed — operational reality, not preference.
        let raw = weights.weight_quality   * quality
                + weights.weight_speed     * speed
                + weights.weight_reasoning * reasoning
                + weights.weight_cost      * cost
                + 0.15                     * capacity
                + 0.20                     * learned;

        raw.clamp(0.0, 1.0)
    }

    // --- Sub-scorers (pub for tests) ---

    /// Raw model quality: log₂(param_count) × recency decay.
    pub fn quality_score_pub(profile: &ModelCapabilityProfile) -> f32 {
        Self::quality_score(profile)
    }

    /// Reasoning support score (pub for tests).
    pub fn reasoning_score_pub(profile: &ModelCapabilityProfile) -> f32 {
        Self::reasoning_score(profile)
    }

    fn quality_score(profile: &ModelCapabilityProfile) -> f32 {
        let param_score = profile.parameter_count_b
            .map(|b| if b > 0.0 { (b.log2() / 8.0_f32).min(1.0) } else { 0.0 })
            .unwrap_or(0.3); // unknown → conservative below-average

        let recency = profile.release_date
            .map(Self::recency_factor)
            .unwrap_or(0.5); // unknown → neutral

        (param_score * recency).min(1.0)
    }

    fn speed_score(profile: &ModelCapabilityProfile) -> f32 {
        if let Some(tok_s) = profile.generation_tok_per_sec {
            // 3000 tok/s → 1.0 (Cerebras WSE ceiling). Linear below.
            (tok_s / 3000.0_f32).min(1.0)
        } else {
            match profile.prefill_speed {
                PrefillSpeed::Instant  => 1.0,
                PrefillSpeed::Fast     => 0.75,
                PrefillSpeed::Moderate => 0.40,
                PrefillSpeed::Slow     => 0.15,
            }
        }
    }

    fn reasoning_score(profile: &ModelCapabilityProfile) -> f32 {
        match &profile.reasoning_support {
            ReasoningSupport::ExtendedThinking { max_budget_tokens } => {
                // More budget headroom → higher score; 32k = 1.0
                let budget_factor = (*max_budget_tokens as f32 / 32_000.0_f32).min(1.0);
                0.6 + 0.4 * budget_factor
            }
            ReasoningSupport::ChainOfThought => 0.35,
            ReasoningSupport::None           => 0.0,
        }
    }

    fn capacity_score(context: &ScoringContext) -> f32 {
        let remaining = context.rate_limit_remaining_pct.clamp(0.0, 1.0);
        // Ceiling bonus: up to +0.15 for 1000 RPM; normalised log-scale
        let ceiling_bonus = context.rate_limit_rpm_ceiling
            .map(|rpm| ((rpm as f32).ln() / (1000.0_f32).ln() * 0.15_f32).min(0.15))
            .unwrap_or(0.05); // unknown ceiling → small positive assumption
        (remaining + ceiling_bonus).min(1.0)
    }

    fn cost_score(profile: &ModelCapabilityProfile) -> f32 {
        match profile.cost_tier {
            CostTier::Free      => 1.0,
            CostTier::Cheap     => 0.7,
            CostTier::Moderate  => 0.4,
            CostTier::Expensive => 0.1,
        }
    }

    /// Recency decay: full score at release date, half-life 12 months.
    fn recency_factor(release: chrono::NaiveDate) -> f32 {
        let days = (chrono::Utc::now().date_naive() - release)
            .num_days()
            .max(0) as f32;
        let months = days / 30.0;
        (-months / 17.3_f32).exp()
    }

    /// True if budget pressure allows using this model's cost tier.
    pub fn passes_budget(profile: &ModelCapabilityProfile, pressure: BudgetPressure) -> bool {
        match pressure {
            BudgetPressure::Normal  => true,
            BudgetPressure::Careful => profile.cost_tier <= CostTier::Moderate,
            BudgetPressure::Emergency | BudgetPressure::Exceeded => {
                profile.cost_tier == CostTier::Free
            }
        }
    }

    /// True if model's TTFT category is likely within the latency budget.
    fn passes_latency(profile: &ModelCapabilityProfile, budget_ms: u32) -> bool {
        let estimated_ttft_ms: u32 = match profile.prefill_speed {
            PrefillSpeed::Instant  => 150,
            PrefillSpeed::Fast     => 600,
            PrefillSpeed::Moderate => 2_000,
            PrefillSpeed::Slow     => 5_000,
        };
        estimated_ttft_ms <= budget_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::model_capability::{ModelCapabilityProfile, PrefillSpeed, ProfileSource, ReasoningSupport};
    use animus_core::provider_meta::{CostTier, DataPolicy};
    use animus_core::budget::BudgetPressure;
    use chrono::NaiveDate;

    fn analytical_weights() -> TaskWeights {
        TaskWeights {
            weight_quality: 0.8,
            weight_speed: 0.1,
            weight_reasoning: 0.7,
            weight_cost: 0.2,
            latency_budget_ms: None,
        }
    }

    fn realtime_weights() -> TaskWeights {
        TaskWeights {
            weight_quality: 0.2,
            weight_speed: 0.9,
            weight_reasoning: 0.0,
            weight_cost: 0.4,
            latency_budget_ms: Some(2000),
        }
    }

    fn profile(provider: &str, model: &str, params: f32, release_year: i32,
               reasoning: ReasoningSupport, tok_s: f32, cost: CostTier) -> ModelCapabilityProfile {
        ModelCapabilityProfile {
            provider: provider.to_string(), model_id: model.to_string(),
            parameter_count_b: Some(params),
            release_date: Some(NaiveDate::from_ymd_opt(release_year, 6, 1).unwrap()),
            context_window: Some(128_000),
            reasoning_support: reasoning,
            generation_tok_per_sec: Some(tok_s),
            prefill_speed: PrefillSpeed::Fast,
            rate_limit_rpm_ceiling: Some(100),
            rate_limit_tpd_ceiling: None,
            cost_tier: cost,
            cost_per_mtok_input: None, cost_per_mtok_output: None,
            trust_score: 3, data_policy: DataPolicy::NoRetention,
            profile_source: ProfileSource::StaticRegistry,
        }
    }

    fn ok_context() -> ScoringContext {
        ScoringContext {
            rate_limit_remaining_pct: 1.0,
            rate_limit_rpm_ceiling: Some(100),
            budget_pressure: BudgetPressure::Normal,
            engine_available: true,
            learned_quality: None,
        }
    }

    #[test]
    fn unavailable_engine_scores_zero() {
        let p = profile("anthropic", "claude-opus-4-6", 200.0, 2025,
            ReasoningSupport::ExtendedThinking { max_budget_tokens: 32_000 }, 80.0, CostTier::Expensive);
        let ctx = ScoringContext { engine_available: false, ..ok_context() };
        assert_eq!(ModelScorer::score(&p, &analytical_weights(), &ctx), 0.0);
    }

    #[test]
    fn prohibited_trust_scores_zero() {
        let mut p = profile("qwen-api", "qwen-max", 72.0, 2025,
            ReasoningSupport::None, 100.0, CostTier::Free);
        p.trust_score = 0;
        assert_eq!(ModelScorer::score(&p, &analytical_weights(), &ok_context()), 0.0);
    }

    #[test]
    fn expensive_model_scores_zero_on_emergency_budget() {
        let p = profile("anthropic", "claude-opus-4-6", 200.0, 2025,
            ReasoningSupport::ExtendedThinking { max_budget_tokens: 32_000 }, 80.0, CostTier::Expensive);
        let ctx = ScoringContext { budget_pressure: BudgetPressure::Emergency, ..ok_context() };
        assert_eq!(ModelScorer::score(&p, &analytical_weights(), &ctx), 0.0);
    }

    #[test]
    fn free_model_passes_emergency_budget() {
        let p = profile("cerebras", "llama3.1-8b", 8.0, 2024,
            ReasoningSupport::None, 3000.0, CostTier::Free);
        let ctx = ScoringContext { budget_pressure: BudgetPressure::Emergency, ..ok_context() };
        assert!(ModelScorer::score(&p, &analytical_weights(), &ctx) > 0.0);
    }

    #[test]
    fn larger_newer_model_scores_higher_quality() {
        let large = profile("x", "large", 235.0, 2025, ReasoningSupport::None, 100.0, CostTier::Free);
        let small = profile("x", "small", 9.0,   2024, ReasoningSupport::None, 100.0, CostTier::Free);
        let q_large = ModelScorer::quality_score_pub(&large);
        let q_small = ModelScorer::quality_score_pub(&small);
        assert!(q_large > q_small, "235B/2025 should outscore 9B/2024: {q_large} vs {q_small}");
    }

    #[test]
    fn extended_thinking_scores_higher_reasoning_than_none() {
        let thinking = profile("x", "t", 35.0, 2025,
            ReasoningSupport::ExtendedThinking { max_budget_tokens: 16_384 }, 100.0, CostTier::Free);
        let plain = profile("x", "p", 35.0, 2025, ReasoningSupport::None, 100.0, CostTier::Free);
        assert!(ModelScorer::reasoning_score_pub(&thinking) > ModelScorer::reasoning_score_pub(&plain));
    }

    #[test]
    fn realtime_task_prefers_fast_model_over_quality() {
        // Cerebras: fast, small, no thinking — PrefillSpeed::Instant via test helper uses Fast,
        // so we need to explicitly set prefill_speed for a realtime test
        let mut fast = profile("cerebras", "fast", 8.0, 2024, ReasoningSupport::None, 3000.0, CostTier::Free);
        fast.prefill_speed = PrefillSpeed::Instant;
        let quality = profile("anthropic", "quality", 200.0, 2025,
            ReasoningSupport::ExtendedThinking { max_budget_tokens: 32_000 }, 80.0, CostTier::Expensive);
        let ctx = ScoringContext { budget_pressure: BudgetPressure::Normal, ..ok_context() };
        let fast_score = ModelScorer::score(&fast, &realtime_weights(), &ctx);
        let quality_score = ModelScorer::score(&quality, &realtime_weights(), &ctx);
        assert!(fast_score > quality_score,
            "realtime class should prefer fast model: {fast_score:.3} vs {quality_score:.3}");
    }

    #[test]
    fn analytical_task_prefers_thinking_model() {
        let thinking = profile("anthropic", "t", 200.0, 2025,
            ReasoningSupport::ExtendedThinking { max_budget_tokens: 32_000 }, 80.0, CostTier::Expensive);
        let fast_no_think = profile("cerebras", "f", 8.0, 2024, ReasoningSupport::None, 3000.0, CostTier::Free);
        let ctx = ScoringContext { budget_pressure: BudgetPressure::Normal, ..ok_context() };
        let t = ModelScorer::score(&thinking, &analytical_weights(), &ctx);
        let f = ModelScorer::score(&fast_no_think, &analytical_weights(), &ctx);
        assert!(t > f, "analytical class should prefer thinking model: {t:.3} vs {f:.3}");
    }

    #[test]
    fn low_remaining_capacity_reduces_score() {
        let p = profile("x", "m", 35.0, 2025, ReasoningSupport::None, 500.0, CostTier::Free);
        let full_ctx = ScoringContext { rate_limit_remaining_pct: 1.0, ..ok_context() };
        let low_ctx  = ScoringContext { rate_limit_remaining_pct: 0.05, ..ok_context() };
        let s_full = ModelScorer::score(&p, &analytical_weights(), &full_ctx);
        let s_low  = ModelScorer::score(&p, &analytical_weights(), &low_ctx);
        assert!(s_full > s_low, "full capacity should outscore near-exhausted: {s_full} vs {s_low}");
    }

    #[test]
    fn learned_quality_affects_score() {
        let p = profile("x", "m", 35.0, 2025, ReasoningSupport::None, 500.0, CostTier::Free);
        let good_learned = ScoringContext { learned_quality: Some(0.9), ..ok_context() };
        let bad_learned  = ScoringContext { learned_quality: Some(0.1), ..ok_context() };
        let s_good = ModelScorer::score(&p, &analytical_weights(), &good_learned);
        let s_bad  = ModelScorer::score(&p, &analytical_weights(), &bad_learned);
        assert!(s_good > s_bad);
    }
}
