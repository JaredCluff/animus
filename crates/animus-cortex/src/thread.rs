use animus_core::error::{AnimusError, Result};
use animus_core::identity::{GoalId, SegmentId, ThreadId};
use animus_core::segment::{Content, Segment, Source};
use animus_core::threading::{ThreadStatus, Signal};
use animus_mnemos::assembler::{AssembledContext, ContextAssembler};
use animus_vectorfs::VectorStore;
use std::collections::HashSet;
use std::sync::Arc;

use crate::llm::{ReasoningEngine, Role, Turn};

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
    /// Embedding dimensionality (for creating placeholder embeddings).
    #[allow(dead_code)]
    embedding_dim: usize,
    /// Current thread status.
    status: ThreadStatus,
    /// Pending inter-thread signals (inbox).
    pending_signals: Vec<Signal>,
}

impl<S: VectorStore> ReasoningThread<S> {
    pub fn new(
        name: String,
        store: Arc<S>,
        token_budget: usize,
        embedding_dim: usize,
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
            embedding_dim,
            status: ThreadStatus::Active,
            pending_signals: Vec::new(),
        }
    }

    /// Process a user message: store it, assemble context, reason, store response.
    pub async fn process_turn(
        &mut self,
        user_input: &str,
        system_prompt: &str,
        engine: &dyn ReasoningEngine,
        embedder: &dyn animus_core::EmbeddingService,
    ) -> Result<String> {
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

        // Add to conversation history
        self.conversation.push(Turn {
            role: Role::User,
            content: user_input.to_string(),
        });

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

        // Call the LLM
        let output = engine.reason(&enriched_system, &self.conversation).await?;

        // Implicit Bayesian feedback: retrieved segments that made it into context
        // get a small positive signal. This makes frequently-used knowledge
        // gradually accumulate confidence over time.
        let anchor_set: std::collections::HashSet<_> =
            self.stored_turn_ids.iter().copied().collect();
        for seg in &context.segments {
            if !anchor_set.contains(&seg.id) {
                // Small implicit boost (0.1) vs explicit feedback (1.0)
                let new_alpha = seg.alpha + 0.1;
                if let Err(e) = self.store.update_meta(
                    seg.id,
                    animus_vectorfs::SegmentUpdate {
                        alpha: Some(new_alpha),
                        confidence: Some(new_alpha / (new_alpha + seg.beta)),
                        ..Default::default()
                    },
                ) {
                    tracing::debug!("implicit feedback update failed for {}: {e}", seg.id);
                }
            }
        }

        // Store assistant response as a segment
        let response_embedding = embedder.embed_text(&output.content).await?;
        let mut response_segment = Segment::new(
            Content::Text(output.content.clone()),
            response_embedding,
            Source::Conversation {
                thread_id: self.id,
                turn: self.conversation.len() as u64,
            },
        );
        response_segment.infer_decay_class();
        let response_seg_id = self.store.store(response_segment)?;
        self.stored_turn_ids.push(response_seg_id);

        // Add to conversation history
        self.conversation.push(Turn {
            role: Role::Assistant,
            content: output.content.clone(),
        });

        tracing::debug!(
            "thread {} turn complete: {} input tokens, {} output tokens",
            self.id,
            output.input_tokens,
            output.output_tokens
        );

        Ok(output.content)
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

    /// Get the number of turns.
    pub fn turn_count(&self) -> usize {
        self.conversation.len()
    }

    /// Get stored turn segment IDs.
    pub fn stored_turn_ids(&self) -> &[SegmentId] {
        &self.stored_turn_ids
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
