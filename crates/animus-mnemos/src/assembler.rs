use animus_core::error::Result;
use animus_core::identity::SegmentId;
use animus_core::segment::Segment;
use animus_vectorfs::VectorStore;
use rand::Rng;
use std::collections::HashSet;
use std::sync::Arc;

/// The assembled context ready to be sent to the LLM.
#[derive(Debug)]
pub struct AssembledContext {
    /// Segments included in this context, ordered by relevance.
    pub segments: Vec<Segment>,
    /// Total estimated token count.
    pub total_tokens: usize,
    /// Segment IDs that were evicted to fit the budget.
    pub evicted_summaries: Vec<EvictedSummary>,
}

/// A summary of an evicted segment, kept in context as a retrieval pointer.
#[derive(Debug)]
pub struct EvictedSummary {
    pub segment_id: SegmentId,
    pub summary: String,
    pub relevance_score: f32,
}

/// Assembles optimal LLM context windows from stored segments.
pub struct ContextAssembler<S: VectorStore> {
    store: Arc<S>,
    /// Maximum token budget for assembled context.
    token_budget: usize,
}

impl<S: VectorStore> ContextAssembler<S> {
    pub fn new(store: Arc<S>, token_budget: usize) -> Self {
        Self {
            store,
            token_budget,
        }
    }

    /// Assemble a context window for a reasoning cycle.
    ///
    /// - `query_embedding`: the semantic focus of the current reasoning
    /// - `anchor_ids`: segment IDs that MUST be included (conversation history, etc.)
    /// - `top_k`: max number of additional segments to retrieve by similarity
    pub fn assemble(
        &self,
        query_embedding: &[f32],
        anchor_ids: &[SegmentId],
        top_k: usize,
    ) -> Result<AssembledContext> {
        let mut included: Vec<Segment> = Vec::new();
        let mut seen_ids: HashSet<SegmentId> = HashSet::new();
        let mut total_tokens: usize = 0;

        // Step 1: Include anchor segments (always included, budget permitting)
        // Use get_raw to avoid inflating access counts on anchors
        for id in anchor_ids {
            if let Some(segment) = self.store.get_raw(*id)? {
                let tokens = segment.estimated_tokens();
                if total_tokens + tokens <= self.token_budget {
                    total_tokens += tokens;
                    seen_ids.insert(segment.id);
                    included.push(segment);
                }
            }
        }

        // Step 2: Retrieve candidates with exploration pool (2x top_k)
        // Extra candidates give Thompson Sampling room to surface less-used knowledge.
        let exploration_pool = top_k.saturating_mul(2).max(top_k + 3);
        let candidates = self.store.query(
            query_embedding,
            exploration_pool,
            Some(animus_core::segment::Tier::Warm),
        )?;

        // Step 2.5: Thompson Sampling re-ranking.
        // Combines semantic similarity with a sampled confidence score. Segments with
        // uncertain Beta distributions (few observations) get more variance,
        // enabling exploration of potentially valuable but under-used knowledge.
        let mut scored: Vec<(Segment, f32)> = candidates
            .into_iter()
            .filter(|c| !seen_ids.contains(&c.id))
            .map(|seg| {
                let sampled = thompson_sample(seg.alpha, seg.beta);
                let similarity = crate::evictor::cosine_similarity(&seg.embedding, query_embedding);
                // Similarity dominates (0.7) so relevant segments still rank first.
                // Thompson-sampled confidence (0.3) enables exploration of uncertain
                // but potentially valuable segments.
                let combined = 0.7 * similarity + 0.3 * sampled;
                (seg, combined)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Step 3: Add candidates until budget is exhausted
        let mut evicted: Vec<(Segment, f32)> = Vec::new();

        for (candidate, score) in scored {
            let tokens = candidate.estimated_tokens();
            if total_tokens + tokens <= self.token_budget {
                total_tokens += tokens;
                seen_ids.insert(candidate.id);
                included.push(candidate);
            } else {
                evicted.push((candidate, score));
            }
        }

        // Step 4: Generate summaries for evicted segments
        let evicted_summaries: Vec<EvictedSummary> = evicted
            .into_iter()
            .map(|(seg, score)| {
                let summary = generate_eviction_summary(&seg);
                EvictedSummary {
                    segment_id: seg.id,
                    summary,
                    relevance_score: score,
                }
            })
            .collect();

        Ok(AssembledContext {
            segments: included,
            total_tokens,
            evicted_summaries,
        })
    }

    /// Update the token budget.
    pub fn set_token_budget(&mut self, budget: usize) {
        self.token_budget = budget;
    }
}

/// Approximate Thompson Sampling from a Beta(alpha, beta) distribution.
///
/// Instead of a true Beta sample (which requires the `rand_distr` crate),
/// we use mean + scaled noise proportional to the distribution's standard deviation.
/// For segments with many observations (low variance), the sampled value stays
/// close to the mean. For segments with few observations (high variance),
/// exploration is enabled through larger noise.
fn thompson_sample(alpha: f32, beta: f32) -> f32 {
    let sum = alpha + beta;
    if sum == 0.0 {
        return 0.5;
    }
    let mean = alpha / sum;
    let variance = (alpha * beta) / (sum * sum * (sum + 1.0));
    let stddev = variance.max(0.0).sqrt();

    let noise: f32 = rand::thread_rng().gen_range(-1.0..1.0);
    // Scale noise by 2 stddevs for meaningful exploration
    (mean + noise * stddev * 2.0).clamp(0.0, 1.0)
}

/// Generate a short summary for an evicted segment.
fn generate_eviction_summary(segment: &Segment) -> String {
    match &segment.content {
        animus_core::Content::Text(t) => {
            let preview: String = t.chars().take(80).collect();
            format!("[Recalled: {preview} — retrieve if needed]")
        }
        animus_core::Content::Structured(_) => {
            format!(
                "[Recalled: structured data segment {} — retrieve if needed]",
                segment.id
            )
        }
        animus_core::Content::Binary { mime_type, .. } => {
            format!(
                "[Recalled: binary ({mime_type}) segment {} — retrieve if needed]",
                segment.id
            )
        }
        animus_core::Content::Reference { uri, summary } => {
            format!("[Recalled: ref to {uri}: {summary} — retrieve if needed]")
        }
    }
}
