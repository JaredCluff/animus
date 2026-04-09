use animus_core::error::{AnimusError, Result};
use animus_core::identity::{GoalId, SegmentId, ThreadId};
use animus_core::segment::{Content, Segment, Source};
use animus_core::threading::{ThreadStatus, Signal};
use animus_mnemos::assembler::{AssembledContext, ContextAssembler};
use animus_vectorfs::VectorStore;
use std::collections::HashSet;
use std::sync::Arc;

use crate::llm::{ReasoningEngine, Role, Turn, TurnContent};

/// An isolated reasoning context — a single conversation thread.
pub struct ReasoningThread<S: VectorStore> {
    /// Unique thread identifier.
    pub id: ThreadId,
    /// Human-readable thread name.
    pub name: String,
    /// Conversation history as Turn objects (for LLM context).
    conversation: Vec<Turn>,
    /// Segment IDs of stored conversation turns (for Mnemos anchoring).
    stored_turn_ids: Vec<SegmentId>,
    /// Goals bound to this thread.
    pub bound_goals: Vec<GoalId>,
    /// The VectorFS store.
    store: Arc<S>,
    /// Context assembler for building LLM context.
    assembler: ContextAssembler<S>,
    /// Current thread status.
    status: ThreadStatus,
    /// Pending inter-thread signals (inbox).
    pending_signals: Vec<Signal>,
    /// Segment IDs retrieved (not anchors) in the most recent turn.
    /// Used for explicit feedback commands (/accept, /correct).
    last_retrieved_ids: Vec<SegmentId>,
    /// Max estimated tokens to retain in conversation history.
    /// Updated per-turn by run_reasoning_turn based on the active engine's context_limit.
    /// Can be overridden at startup via ANIMUS_CONVERSATION_TOKEN_BUDGET.
    pub conversation_token_budget: usize,
}

impl<S: VectorStore> ReasoningThread<S> {
    pub fn new(
        name: String,
        store: Arc<S>,
        token_budget: usize,
        conversation_token_budget: usize,
    ) -> Self {
        let assembler = ContextAssembler::new(store.clone(), token_budget);
        Self {
            id: ThreadId::new(),
            name,
            conversation: Vec::new(),
            stored_turn_ids: Vec::new(),
            bound_goals: Vec::new(),
            store,
            assembler,
            status: ThreadStatus::Active,
            pending_signals: Vec::new(),
            last_retrieved_ids: Vec::new(),
            conversation_token_budget,
        }
    }

    /// Update the conversation token budget.
    /// Called by run_reasoning_turn after the active engine is selected,
    /// so the budget reflects the real context limit of the model answering.
    pub fn set_conversation_budget(&mut self, budget: usize) {
        self.conversation_token_budget = budget;
    }

    /// Classify whether a user input warrants extended thinking.
    ///
    /// Returns `true` for complex inputs (code, analysis, multi-step questions),
    /// `false` for simple conversational exchanges that can skip the thinking phase.
    fn needs_thinking(input: &str) -> bool {
        let lower = input.to_lowercase();

        // Code blocks always warrant thinking
        if input.contains("```") || (input.contains("    ") && input.contains('\n')) {
            return true;
        }

        // Long messages likely need analysis
        if input.len() > 300 {
            return true;
        }

        // Short messages → no thinking needed
        let word_count = input.split_whitespace().count();
        if word_count <= 5 {
            return false;
        }

        // Deep reasoning signal phrases
        const THINK_SIGNALS: &[&str] = &[
            "why ", "how do", "how does", "how can", "how would",
            "explain", "analyze", "analyse", "design", "debug",
            "implement", "refactor", "compare", "evaluate", "difference",
            "plan", "architecture", "strategy", "help me",
            "figure out", "what if", "should i", "is there a way",
            "walk me through", "step by step", "break down",
            "pros and cons", "trade-off", "tradeoff",
        ];
        if THINK_SIGNALS.iter().any(|kw| lower.contains(kw)) {
            return true;
        }

        // Default: skip thinking for conversational exchanges
        false
    }

    /// Process a user message: store it, assemble context, reason.
    ///
    /// Returns `ReasoningOutput` so the runtime can inspect `stop_reason` and
    /// drive multi-round tool execution. The caller is responsible for pushing
    /// the assistant turn and storing the response segment.
    pub async fn process_turn(
        &mut self,
        user_input: &str,
        system_prompt: &str,
        engine: &dyn ReasoningEngine,
        embedder: &dyn animus_core::EmbeddingService,
        tools: Option<&[crate::llm::ToolDefinition]>,
    ) -> Result<crate::llm::ReasoningOutput> {
        // Store user input as a segment
        let user_embedding = embedder.embed_text(user_input).await?;
        let mut user_segment = Segment::new(
            Content::Text(user_input.to_string()),
            user_embedding.clone(),
            Source::Conversation {
                thread_id: self.id,
                turn: self.conversation.len() as u64,
            },
        );
        user_segment.infer_decay_class();
        let user_seg_id = self.store.store(user_segment)?;
        self.stored_turn_ids.push(user_seg_id);
        // Keep only the most recent anchor IDs to prevent token budget starvation.
        // Older turn segments remain in VectorFS and can still be retrieved via similarity search.
        const MAX_ANCHOR_IDS: usize = 50;
        if self.stored_turn_ids.len() > MAX_ANCHOR_IDS {
            self.stored_turn_ids.drain(..self.stored_turn_ids.len() - MAX_ANCHOR_IDS);
        }

        // Add to conversation history (stores the real user text, not the think-control version)
        self.conversation.push(Turn::text(Role::User, user_input));

        // Assemble context: anchor on stored turns, retrieve similar knowledge
        let context = self.assembler.assemble(
            &user_embedding,
            &self.stored_turn_ids,
            10,
        )?;

        // Inject pending signals into context
        let signals = self.drain_signals();
        let enriched_system = if signals.is_empty() {
            self.build_system_prompt(system_prompt, &context)
        } else {
            let mut sys = self.build_system_prompt(system_prompt, &context);
            sys.push_str("\n\n## Inter-Thread Signals\n");
            for signal in &signals {
                sys.push_str(&format!(
                    "- [{:?}] from thread {}: {}\n",
                    signal.priority,
                    signal.source_thread.0.to_string().get(..8).unwrap_or("?"),
                    signal.summary,
                ));
            }
            sys
        };

        // Build the conversation slice passed to the engine.
        // If the engine supports think-control and the input doesn't warrant
        // extended reasoning, prepend /no_think to the last user turn so the
        // model skips its thinking phase. The stored conversation is unmodified.
        let engine_conversation: std::borrow::Cow<[Turn]> =
            if engine.supports_think_control() && !Self::needs_thinking(user_input) {
                let mut turns = self.conversation.clone();
                if let Some(last) = turns.last_mut() {
                    if last.role == Role::User {
                        let original = last.content.iter()
                            .filter_map(|c| if let crate::llm::TurnContent::Text(t) = c { Some(t.as_str()) } else { None })
                            .collect::<Vec<_>>().join("\n");
                        last.content = vec![crate::llm::TurnContent::Text(
                            format!("/no_think\n{original}")
                        )];
                    }
                }
                tracing::debug!("think-control: skipping thinking phase for short/simple input");
                std::borrow::Cow::Owned(turns)
            } else {
                std::borrow::Cow::Borrowed(&self.conversation)
            };

        // Call the LLM
        let output = engine.reason(&enriched_system, &engine_conversation, tools).await?;

        // Track which knowledge segments were retrieved (not conversation anchors).
        // Used for implicit feedback now and explicit feedback via /accept, /correct.
        let anchor_set: std::collections::HashSet<_> =
            self.stored_turn_ids.iter().copied().collect();
        self.last_retrieved_ids = context
            .segments
            .iter()
            .filter(|s| !anchor_set.contains(&s.id))
            .map(|s| s.id)
            .collect();

        // Implicit Bayesian feedback: retrieved segments get a small positive signal.
        // Alpha is capped at 100.0 to prevent unbounded growth skewing confidence toward 1.0.
        const MAX_IMPLICIT_ALPHA: f32 = 100.0;
        for seg in &context.segments {
            if !anchor_set.contains(&seg.id) && seg.alpha < MAX_IMPLICIT_ALPHA {
                let new_alpha = (seg.alpha + 0.1).min(MAX_IMPLICIT_ALPHA);
                if let Err(e) = self.store.update_meta(
                    seg.id,
                    animus_vectorfs::SegmentUpdate {
                        alpha: Some(new_alpha),
                        ..Default::default()
                    },
                ) {
                    tracing::debug!("implicit feedback update failed for {}: {e}", seg.id);
                }
            }
        }

        tracing::debug!(
            "thread {} turn complete: {} input tokens, {} output tokens",
            self.id,
            output.input_tokens,
            output.output_tokens
        );

        Ok(output)
    }

    /// Like `process_turn`, but tries each engine in `engines` in order.
    /// Prep (VectorFS store, context assembly) happens once. On a retryable error
    /// (429, rate limit, 503, overloaded) from engine N, engine N+1 is tried.
    /// Non-retryable errors and exhausted fallbacks propagate as-is.
    pub async fn process_turn_with_engines(
        &mut self,
        user_input: &str,
        system_prompt: &str,
        engines: &[&dyn ReasoningEngine],
        embedder: &dyn animus_core::EmbeddingService,
        tools: Option<&[crate::llm::ToolDefinition]>,
    ) -> Result<crate::llm::ReasoningOutput> {
        // Store user input once regardless of which engine succeeds
        let user_embedding = embedder.embed_text(user_input).await?;
        let mut user_segment = Segment::new(
            Content::Text(user_input.to_string()),
            user_embedding.clone(),
            Source::Conversation { thread_id: self.id, turn: self.conversation.len() as u64 },
        );
        user_segment.infer_decay_class();
        let user_seg_id = self.store.store(user_segment)?;
        self.stored_turn_ids.push(user_seg_id);
        const MAX_ANCHOR_IDS: usize = 50;
        if self.stored_turn_ids.len() > MAX_ANCHOR_IDS {
            self.stored_turn_ids.drain(..self.stored_turn_ids.len() - MAX_ANCHOR_IDS);
        }
        self.conversation.push(Turn::text(Role::User, user_input));

        let context = self.assembler.assemble(&user_embedding, &self.stored_turn_ids, 10)?;
        let signals = self.drain_signals();
        let enriched_system = if signals.is_empty() {
            self.build_system_prompt(system_prompt, &context)
        } else {
            let mut sys = self.build_system_prompt(system_prompt, &context);
            sys.push_str("\n\n## Inter-Thread Signals\n");
            for signal in &signals {
                sys.push_str(&format!(
                    "- [{:?}] from thread {}: {}\n",
                    signal.priority,
                    signal.source_thread.0.to_string().get(..8).unwrap_or("?"),
                    signal.summary,
                ));
            }
            sys
        };

        // Try each engine in order; fall back on retryable errors
        {
            let engine_names: Vec<&str> = engines.iter().map(|e| e.model_name()).collect();
            tracing::info!(
                thread = %self.id,
                engine_count = engines.len(),
                "process_turn_with_engines: trying [{}]",
                engine_names.join(" → ")
            );
        }
        let mut last_err: Option<animus_core::AnimusError> = None;
        let mut failed_engine_names: Vec<String> = Vec::new();
        for (engine_index, engine) in engines.iter().enumerate() {
            let engine = *engine;
            tracing::debug!(
                thread = %self.id,
                engine = engine.model_name(),
                index = engine_index,
                "process_turn_with_engines: attempting engine"
            );
            let think_suppressed = engine.supports_think_control() && !Self::needs_thinking(user_input);
            let engine_conversation: std::borrow::Cow<[Turn]> =
                if think_suppressed {
                    tracing::debug!(engine = engine.model_name(), "Suppressing thinking (/no_think) — input is short/simple");
                    let mut turns = self.conversation.clone();
                    if let Some(last) = turns.last_mut() {
                        if last.role == Role::User {
                            let original = last.content.iter()
                                .filter_map(|c| if let crate::llm::TurnContent::Text(t) = c { Some(t.as_str()) } else { None })
                                .collect::<Vec<_>>().join("\n");
                            last.content = vec![crate::llm::TurnContent::Text(format!("/no_think\n{original}"))];
                        }
                    }
                    std::borrow::Cow::Owned(turns)
                } else {
                    std::borrow::Cow::Borrowed(&self.conversation)
                };

            match engine.reason(&enriched_system, &engine_conversation, tools).await {
                Ok(mut output) => {
                    // Tag output with which engine responded and whether it was a fallback.
                    output.engine_used = engine.model_name().to_string();
                    output.fell_back = engine_index > 0;
                    output.failed_engines = failed_engine_names;
                    // Post-success bookkeeping (same as process_turn)
                    let anchor_set: std::collections::HashSet<_> = self.stored_turn_ids.iter().copied().collect();
                    self.last_retrieved_ids = context.segments.iter()
                        .filter(|s| !anchor_set.contains(&s.id))
                        .map(|s| s.id)
                        .collect();
                    const MAX_IMPLICIT_ALPHA: f32 = 100.0;
                    for seg in &context.segments {
                        if !anchor_set.contains(&seg.id) && seg.alpha < MAX_IMPLICIT_ALPHA {
                            let _ = self.store.update_meta(seg.id, animus_vectorfs::SegmentUpdate {
                                alpha: Some((seg.alpha + 0.1).min(MAX_IMPLICIT_ALPHA)),
                                ..Default::default()
                            });
                        }
                    }
                    tracing::debug!(
                        "thread {} turn complete (engine '{}', fallback={}): {} in, {} out",
                        self.id, output.engine_used, output.fell_back, output.input_tokens, output.output_tokens
                    );
                    return Ok(output);
                }
                Err(e) if is_retryable_error(&e) => {
                    tracing::warn!(
                        "Engine '{}' unavailable (retryable): {e} — trying next fallback",
                        engine.model_name()
                    );
                    // Auth failures cascade but do NOT mark the engine unhealthy — the engine
                    // itself is fine, only the credentials are stale. It will recover once
                    // credentials are refreshed without needing a health probe cycle.
                    if !matches!(e, animus_core::AnimusError::NeedsOAuthReAuth) {
                        failed_engine_names.push(engine.model_name().to_string());
                    }
                    last_err = Some(e);
                }
                Err(e) => {
                    tracing::warn!(
                        engine = engine.model_name(),
                        "Engine failed (non-retryable): {e}"
                    );
                    return Err(e);
                }
            }
        }

        tracing::error!(
            thread = %self.id,
            engines_tried = engines.len(),
            "All engines exhausted — no successful response"
        );
        Err(last_err.unwrap_or_else(|| animus_core::AnimusError::Llm("no engines available".to_string())))
    }

    /// Build system prompt enriched with assembled context.
    fn build_system_prompt(&self, base_prompt: &str, context: &AssembledContext) -> String {
        let mut prompt = base_prompt.to_string();

        // Add recalled knowledge from VectorFS
        let turn_ids: HashSet<_> = self.stored_turn_ids.iter().copied().collect();
        let knowledge_segments: Vec<&Segment> = context
            .segments
            .iter()
            .filter(|s| !turn_ids.contains(&s.id))
            .collect();

        if !knowledge_segments.is_empty() {
            prompt.push_str("\n\n## Recalled Knowledge\n");
            for seg in knowledge_segments {
                if let Content::Text(t) = &seg.content {
                    prompt.push_str(&format!(
                        "\n- [confidence: {:.1}] {}\n",
                        seg.confidence, t
                    ));
                }
            }
        }

        // Add eviction summaries
        if !context.evicted_summaries.is_empty() {
            prompt.push_str("\n## Additional context (summarized)\n");
            for evicted in &context.evicted_summaries {
                prompt.push_str(&format!("\n{}\n", evicted.summary));
            }
        }

        prompt
    }

    /// Get the conversation history.
    pub fn conversation(&self) -> &[Turn] {
        &self.conversation
    }

    /// Push a turn directly to the conversation (used by runtime tool loop).
    pub fn push_turn(&mut self, turn: Turn) {
        self.conversation.push(turn);
        self.trim_conversation_if_needed();
    }

    /// Store a response as a VectorFS segment (called by runtime after final response).
    pub async fn store_response_segment(
        &mut self,
        response: &str,
        embedder: &dyn animus_core::EmbeddingService,
    ) -> Result<()> {
        let embedding = embedder.embed_text(response).await?;
        let mut segment = Segment::new(
            Content::Text(response.to_string()),
            embedding,
            Source::Conversation {
                thread_id: self.id,
                turn: self.conversation.len() as u64,
            },
        );
        segment.infer_decay_class();
        let id = self.store.store(segment)?;
        self.stored_turn_ids.push(id);
        const MAX_ANCHOR_IDS: usize = 50;
        if self.stored_turn_ids.len() > MAX_ANCHOR_IDS {
            self.stored_turn_ids.drain(..self.stored_turn_ids.len() - MAX_ANCHOR_IDS);
        }
        Ok(())
    }

    /// Trim conversation history to MAX_CONVERSATION_TOKENS if needed.
    ///
    /// Removes turns from the front until the estimated token count is within budget,
    /// then injects a single summary turn capturing what was dropped. Always trims in
    /// whole "exchange" units (minimum 2 turns) to avoid leaving orphaned tool-result turns.
    fn trim_conversation_if_needed(&mut self) {
        if estimate_conversation_tokens(&self.conversation) <= self.conversation_token_budget {
            return;
        }

        // Find how many turns from the front we must drop.
        // Drop at least 4 at a time (2 full exchanges) so we don't spin trimming one turn per call.
        let mut drop_count = 0usize;
        let mut dropped_tokens = 0usize;
        let target_drop = estimate_conversation_tokens(&self.conversation)
            .saturating_sub(self.conversation_token_budget / 2); // trim to 50% to amortise cost

        for turn in &self.conversation {
            if dropped_tokens >= target_drop && drop_count >= 4 {
                break;
            }
            dropped_tokens += estimate_turn_tokens(turn);
            drop_count += 1;
        }

        // Round up to even to keep user/assistant pairs together.
        if drop_count % 2 != 0 {
            drop_count += 1;
        }
        let drop_count = drop_count.min(self.conversation.len().saturating_sub(4));
        if drop_count == 0 {
            return;
        }

        let dropped: Vec<Turn> = self.conversation.drain(..drop_count).collect();
        let summary = summarize_turns(&dropped);

        tracing::info!(
            thread = %self.id,
            dropped = drop_count,
            summary_len = summary.len(),
            "Conversation trimmed: {drop_count} turns dropped to stay within token budget"
        );

        // Inject summary as a System turn at the front so context isn't lost entirely.
        self.conversation.insert(0, Turn {
            role: Role::System,
            content: vec![TurnContent::Text(format!(
                "[Earlier conversation ({drop_count} turns) condensed: {summary}]"
            ))],
        });
    }

    /// Get the number of turns.
    pub fn turn_count(&self) -> usize {
        self.conversation.len()
    }

    /// Get stored turn segment IDs.
    pub fn stored_turn_ids(&self) -> &[SegmentId] {
        &self.stored_turn_ids
    }

    /// Get segment IDs that were retrieved (not conversation anchors) in the last turn.
    /// Empty if no turn has been processed yet.
    pub fn last_retrieved_ids(&self) -> &[SegmentId] {
        &self.last_retrieved_ids
    }

    /// Get the current thread status.
    pub fn status(&self) -> ThreadStatus {
        self.status
    }

    /// Set thread status, validating the transition.
    pub fn set_status(&mut self, status: ThreadStatus) -> Result<()> {
        if !self.status.can_transition_to(status) {
            return Err(AnimusError::Threading(format!(
                "invalid status transition from {:?} to {:?}",
                self.status, status
            )));
        }
        self.status = status;
        Ok(())
    }

    /// Deliver a signal to this thread's inbox.
    pub fn deliver_signal(&mut self, signal: Signal) {
        self.pending_signals.push(signal);
    }

    /// Get pending signals.
    pub fn pending_signals(&self) -> &[Signal] {
        &self.pending_signals
    }

    /// Drain all pending signals, sorted by priority (Urgent first).
    pub fn drain_signals(&mut self) -> Vec<Signal> {
        let mut signals: Vec<Signal> = self.pending_signals.drain(..).collect();
        signals.sort_by(|a, b| b.priority.cmp(&a.priority));
        signals
    }
}

// ---------------------------------------------------------------------------
// Conversation windowing
// ---------------------------------------------------------------------------

/// Default conversation token budget used when no engine context limit is known yet
/// and ANIMUS_CONVERSATION_TOKEN_BUDGET is not set.
pub const DEFAULT_CONVERSATION_TOKEN_BUDGET: usize = 16_000;

/// Estimate tokens in a single turn (4 chars ≈ 1 token, plus per-message overhead).
fn estimate_turn_tokens(turn: &Turn) -> usize {
    let content_chars: usize = turn.content.iter().map(|c| match c {
        TurnContent::Text(s) => s.len(),
        TurnContent::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        TurnContent::ToolResult { content, .. } => content.len(),
    }).sum();
    (content_chars / 4).max(1) + 8 // 8 tokens per-message overhead
}

/// Estimate total tokens for the full conversation slice.
fn estimate_conversation_tokens(conversation: &[Turn]) -> usize {
    conversation.iter().map(estimate_turn_tokens).sum()
}

/// Build a terse plain-text summary of a slice of turns.
/// Used as the injected summary when old turns are trimmed.
fn summarize_turns(turns: &[Turn]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for turn in turns {
        let text: String = turn.content.iter().filter_map(|c| match c {
            TurnContent::Text(t) => {
                let preview: String = t.chars().take(120).collect();
                if !preview.trim().is_empty() {
                    Some(preview)
                } else {
                    None
                }
            }
            TurnContent::ToolUse { name, .. } => Some(format!("[called {}]", name)),
            TurnContent::ToolResult { content, is_error, .. } => {
                if *is_error {
                    Some(format!("[tool error: {}]", &content[..content.len().min(60)]))
                } else {
                    let preview: String = content.chars().take(80).collect();
                    Some(format!("[result: {preview}]"))
                }
            }
        }).collect::<Vec<_>>().join(" ");

        if !text.trim().is_empty() {
            let role_label = match turn.role {
                Role::User => "User",
                Role::Assistant => "Animus",
                Role::System => "System",
            };
            parts.push(format!("{role_label}: {text}"));
        }
    }
    parts.join(" | ")
}

/// Returns true if the error is transient and a fallback engine should be tried.
///
/// Includes rate limits (429), service unavailable (503), AND model-not-found /
/// auth errors — because the cascade should keep trying other engines rather
/// than returning a hard failure to the user when healthy engines remain.
pub fn is_retryable_error(err: &animus_core::AnimusError) -> bool {
    match err {
        animus_core::AnimusError::LlmRateLimited(_)
        | animus_core::AnimusError::LlmServiceUnavailable(_)
        // Dead OAuth refresh token — cascade to next engine, notifier already fired
        | animus_core::AnimusError::NeedsOAuthReAuth => true,
        animus_core::AnimusError::Llm(msg) => {
            // Model not found, auth failures, and other provider-specific errors
            // should cascade to the next engine rather than killing the whole turn.
            msg.contains("404")
                || msg.contains("not_found")
                || msg.contains("does not exist")
                || msg.contains("401")
                || msg.contains("403")
                || msg.contains("Unauthorized")
                || msg.contains("invalid_api_key")
                // Network/connection failures — endpoint unreachable, cascade to next engine
                || msg.contains("error sending request")
                || msg.contains("connection refused")
                || msg.contains("connect error")
                || msg.contains("dns error")
                || msg.contains("connection reset")
                // 422 = unsupported parameter for this model (e.g. tools sent to a non-chat model)
                || msg.contains("422")
                || msg.contains("extra_forbidden")
                || msg.contains("Unknown parameter")
                // Context length exceeded — model can't handle our input size, try next
                || msg.contains("context_length_exceeded")
                || msg.contains("context window")
                || msg.contains("reduce the length")
                // 400: tool choice not supported without special server flags (vLLM/Ollama-served models)
                || msg.contains("tool choice requires")
                || msg.contains("enable-auto-tool-choice")
                || msg.contains("tool-call-parser")
                // 400: billing/quota exhausted — treat as provider unavailable, cascade to next
                || msg.contains("credit balance")
                || msg.contains("too low")
                || msg.contains("quota")
                || msg.contains("billing")
        }
        _ => false,
    }
}
