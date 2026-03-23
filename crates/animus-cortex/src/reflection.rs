use animus_core::identity::{GoalId, SegmentId, ThreadId};
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_core::threading::{Signal, SignalPriority};
use animus_core::EmbeddingService;
use animus_vectorfs::VectorStore;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::llm::{ReasoningEngine, Role, Turn};
use crate::telos::GoalManager;

/// Structured output from a Reflection cycle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReflectionOutput {
    /// New synthesized knowledge to store.
    pub syntheses: Vec<Synthesis>,
    /// Contradictions detected between segments.
    pub contradictions: Vec<Contradiction>,
    /// Goal progress updates.
    pub goal_updates: Vec<GoalUpdate>,
    /// Signals to send to Reasoning.
    pub signals: Vec<ReflectionSignal>,
}

/// A synthesized insight from recent knowledge.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Synthesis {
    /// The synthesized insight text.
    pub content: String,
    /// Which segment IDs led to this synthesis (provenance).
    pub source_segment_ids: Vec<SegmentId>,
    /// What kind of knowledge is this?
    pub decay_class: DecayClass,
    /// Why this confidence level?
    pub confidence_rationale: String,
}

/// A contradiction detected between two segments.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Contradiction {
    pub segment_a: SegmentId,
    pub segment_b: SegmentId,
    pub description: String,
    pub suggested_resolution: String,
}

/// A goal progress update from Reflection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoalUpdate {
    pub goal_id: GoalId,
    pub progress_note: String,
    pub suggest_complete: bool,
}

/// A signal Reflection wants to send to Reasoning.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReflectionSignal {
    pub priority: SignalPriority,
    pub insight: String,
    pub relevant_segments: Vec<SegmentId>,
}

const REFLECTION_SYSTEM_PROMPT: &str = r#"You are the Reflection subsystem of an AILF (AI Life Form). Your role is to examine recent knowledge and experiences and produce higher-order understanding.

Consider:
- Do any recent observations form a pattern?
- Do any new segments contradict existing knowledge?
- Has progress been made toward any active goals?
- Is any knowledge decaying that should be reinforced or let go?
- Are there insights the Reasoning thread should know about?

Respond with JSON only:
{
  "syntheses": [
    {
      "content": "Synthesized insight text",
      "source_segment_ids": ["uuid1", "uuid2"],
      "decay_class": "Procedural",
      "confidence_rationale": "Why this confidence level"
    }
  ],
  "contradictions": [
    {
      "segment_a": "uuid1",
      "segment_b": "uuid2",
      "description": "What contradicts",
      "suggested_resolution": "How to resolve"
    }
  ],
  "goal_updates": [
    {
      "goal_id": "uuid",
      "progress_note": "What progress was made",
      "suggest_complete": false
    }
  ],
  "signals": [
    {
      "priority": "Normal",
      "insight": "What Reasoning should know",
      "relevant_segments": ["uuid1"]
    }
  ]
}"#;

/// Format recent segments and goals into a reflection prompt.
fn format_reflection_prompt(segments: &[Segment], goals_summary: &str) -> String {
    let mut msg = format!("Recent knowledge ({} segments):\n\n", segments.len());
    for seg in segments {
        let source_desc = match &seg.source {
            Source::Conversation { thread_id, turn } => {
                format!("conversation(thread={}, turn={turn})",
                    thread_id.0.to_string().get(..8).unwrap_or("?"))
            }
            Source::Observation { event_type, .. } => format!("observation({event_type})"),
            Source::SelfDerived { reasoning_chain } => format!("self-derived({reasoning_chain})"),
            Source::Consolidation { .. } => "consolidation".to_string(),
            Source::Manual { description } => format!("manual({description})"),
            Source::Federation { .. } => "federation".to_string(),
        };
        if let Content::Text(t) = &seg.content {
            msg.push_str(&format!(
                "- [id={}, source={source_desc}, confidence={:.2}, decay={:?}] {}\n",
                seg.id, seg.confidence, seg.decay_class, t
            ));
        }
    }
    if !goals_summary.is_empty() {
        msg.push_str(&format!("\n## Active Goals\n{goals_summary}\n"));
    }
    msg
}

/// Background reflection loop — periodically synthesizes recent knowledge.
pub struct ReflectionLoop<S: VectorStore> {
    engine: Box<dyn ReasoningEngine>,
    store: Arc<S>,
    embedder: Arc<dyn EmbeddingService>,
    goals: Arc<parking_lot::Mutex<GoalManager>>,
    signal_tx: mpsc::Sender<Signal>,
    cycle_interval: Duration,
    min_new_segments: usize,
    pub(crate) last_cycle: DateTime<Utc>,
    pub(crate) last_segment_count: usize,
    signaled_contradictions: HashSet<(SegmentId, SegmentId)>,
    /// Stable identity for this loop, used as signal source.
    source_id: ThreadId,
}

impl<S: VectorStore> ReflectionLoop<S> {
    pub fn new(
        engine: Box<dyn ReasoningEngine>,
        store: Arc<S>,
        embedder: Arc<dyn EmbeddingService>,
        goals: Arc<parking_lot::Mutex<GoalManager>>,
        signal_tx: mpsc::Sender<Signal>,
    ) -> Self {
        let last_segment_count = store.count(None);
        Self {
            engine,
            store,
            embedder,
            goals,
            signal_tx,
            cycle_interval: Duration::from_secs(600),
            min_new_segments: 3,
            last_cycle: Utc::now(),
            last_segment_count,
            signaled_contradictions: HashSet::new(),
            source_id: ThreadId::new(),
        }
    }

    pub fn with_cycle_interval(mut self, interval: Duration) -> Self {
        self.cycle_interval = interval;
        self
    }

    pub fn with_min_new_segments(mut self, min: usize) -> Self {
        self.min_new_segments = min;
        self
    }

    /// Override the last cycle timestamp (useful for testing).
    pub fn with_last_cycle(mut self, last_cycle: DateTime<Utc>) -> Self {
        self.last_cycle = last_cycle;
        self
    }

    /// Override the last segment count baseline (useful for testing).
    pub fn with_last_segment_count(mut self, count: usize) -> Self {
        self.last_segment_count = count;
        self
    }

    /// Gather segments created or accessed since last cycle.
    fn gather_recent_segments(&self) -> Vec<Segment> {
        let all_ids = self.store.segment_ids(None);
        let mut recent = Vec::new();
        for id in all_ids {
            if let Ok(Some(seg)) = self.store.get_raw(id) {
                if seg.created > self.last_cycle || seg.last_accessed > self.last_cycle {
                    recent.push(seg);
                }
            }
        }
        // Sort newest-first so the most recent activity is prioritised when truncating.
        recent.sort_by(|a, b| b.created.cmp(&a.created));
        // Limit to avoid overwhelming the model
        recent.truncate(50);
        recent
    }

    /// Run one reflection cycle.
    pub async fn run_cycle(&mut self) {
        let recent = self.gather_recent_segments();
        if recent.is_empty() {
            self.last_segment_count = self.store.count(None);
            return;
        }

        let goals_summary = self.goals.lock().goals_summary();
        let user_msg = format_reflection_prompt(&recent, &goals_summary);
        let messages = vec![Turn::text(Role::User, &user_msg)];

        match self.engine.reason(REFLECTION_SYSTEM_PROMPT, &messages, None).await {
            Ok(output) => {
                let json = strip_json_fence(&output.content);
                match serde_json::from_str::<ReflectionOutput>(json) {
                    Ok(reflection) => {
                        self.handle_reflection_output(reflection).await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse reflection output: {e}\nRaw: {}", &output.content[..output.content.len().min(200)]);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Reflection engine error: {e}");
            }
        }

        self.last_cycle = Utc::now();
        self.last_segment_count = self.store.count(None);
    }

    /// Process the structured reflection output.
    async fn handle_reflection_output(&mut self, output: ReflectionOutput) {
        // Store syntheses as SelfDerived segments
        for synthesis in output.syntheses {
            match self.embedder.embed_text(&synthesis.content).await {
                Ok(embedding) => {
                    let mut segment = Segment::new(
                        Content::Text(synthesis.content),
                        embedding,
                        Source::SelfDerived {
                            reasoning_chain: "reflection-synthesis".to_string(),
                        },
                    );
                    segment.decay_class = synthesis.decay_class;
                    segment.lineage = synthesis.source_segment_ids;
                    // Uniform prior — must earn trust through feedback
                    segment.alpha = 1.0;
                    segment.beta = 1.0;
                    segment.confidence = 0.5;
                    if let Err(e) = self.store.store(segment) {
                        tracing::warn!("Failed to store synthesis: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("Synthesis embedding failed: {e}");
                }
            }
        }

        // Handle contradictions with deduplication
        for contradiction in output.contradictions {
            let pair = if contradiction.segment_a < contradiction.segment_b {
                (contradiction.segment_a, contradiction.segment_b)
            } else {
                (contradiction.segment_b, contradiction.segment_a)
            };
            if self.signaled_contradictions.contains(&pair) {
                continue;
            }
            // Cap the deduplication set to prevent unbounded growth in long sessions.
            // When full, evict ~10% of entries so we don't evict on every insert.
            const MAX_SIGNALED_CONTRADICTIONS: usize = 10_000;
            if self.signaled_contradictions.len() >= MAX_SIGNALED_CONTRADICTIONS {
                let to_remove: Vec<_> = self.signaled_contradictions
                    .iter()
                    .take(MAX_SIGNALED_CONTRADICTIONS / 10)
                    .cloned()
                    .collect();
                for k in to_remove {
                    self.signaled_contradictions.remove(&k);
                }
            }
            self.signaled_contradictions.insert(pair);

            let sig = Signal {
                source_thread: self.source_id,
                target_thread: ThreadId::default(),
                priority: SignalPriority::Normal,
                summary: format!(
                    "Contradiction detected: {}. Suggested resolution: {}",
                    contradiction.description, contradiction.suggested_resolution,
                ),
                segment_refs: vec![contradiction.segment_a, contradiction.segment_b],
                created: Utc::now(),
            };
            if self.signal_tx.send(sig).await.is_err() {
                tracing::warn!("Signal channel closed");
            }
        }

        // Handle goal updates
        for update in output.goal_updates {
            if update.suggest_complete {
                let sig = Signal {
                    source_thread: self.source_id,
                    target_thread: ThreadId::default(),
                    priority: SignalPriority::Normal,
                    summary: format!(
                        "Goal {} may be complete: {}",
                        update.goal_id, update.progress_note
                    ),
                    segment_refs: vec![],
                    created: Utc::now(),
                };
                if self.signal_tx.send(sig).await.is_err() {
                    tracing::warn!("Signal channel closed");
                }
            }
        }

        // Handle signals from reflection
        for signal in output.signals {
            let sig = Signal {
                source_thread: self.source_id,
                target_thread: ThreadId::default(),
                priority: signal.priority,
                summary: signal.insight,
                segment_refs: signal.relevant_segments,
                created: Utc::now(),
            };
            if self.signal_tx.send(sig).await.is_err() {
                tracing::warn!("Signal channel closed");
            }
        }
    }

    /// Run the reflection loop. Checks every 60 seconds if a cycle is needed.
    /// This method runs indefinitely — spawn it with `tokio::spawn`.
    pub async fn run(mut self) {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            if self.should_cycle() {
                self.run_cycle().await;
            }
        }
    }

    /// Check if a reflection cycle should run based on segment count and time.
    pub fn should_cycle(&self) -> bool {
        let now = Utc::now();
        let since_last = (now - self.last_cycle).num_seconds().max(0) as u64;
        let current_count = self.store.count(None);
        let new_segments = current_count.saturating_sub(self.last_segment_count);

        // Trigger: enough new segments AND at least 60s since last cycle
        let triggered = new_segments >= self.min_new_segments && since_last >= 60;
        // Maximum interval: cycle_interval elapsed AND at least 1 new segment
        let max_interval = since_last >= self.cycle_interval.as_secs() && new_segments >= 1;

        triggered || max_interval
    }
}

/// Strip markdown code fences (` ```json ... ``` ` or ` ``` ... ``` `) from LLM output.
fn strip_json_fence(s: &str) -> &str {
    let s = s.trim();
    let inner = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    // Strip trailing fence if present
    inner
        .trim()
        .trim_end_matches("```")
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reflection_output_deserialize() {
        let json = r#"{
            "syntheses": [{
                "content": "The user prefers Rust for systems work",
                "source_segment_ids": [],
                "decay_class": "Procedural",
                "confidence_rationale": "Observed across multiple conversations"
            }],
            "contradictions": [],
            "goal_updates": [],
            "signals": []
        }"#;
        let output: ReflectionOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.syntheses.len(), 1);
        assert_eq!(output.syntheses[0].decay_class, DecayClass::Procedural);
    }

    #[test]
    fn test_reflection_output_with_contradiction() {
        let id_a = SegmentId::new();
        let id_b = SegmentId::new();
        let json = serde_json::json!({
            "syntheses": [],
            "contradictions": [{
                "segment_a": id_a,
                "segment_b": id_b,
                "description": "Conflicting build status",
                "suggested_resolution": "Check latest CI run"
            }],
            "goal_updates": [],
            "signals": []
        });
        let output: ReflectionOutput = serde_json::from_str(&json.to_string()).unwrap();
        assert_eq!(output.contradictions.len(), 1);
        assert_eq!(output.contradictions[0].segment_a, id_a);
    }

    #[test]
    fn test_should_cycle_false_initially() {
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let goals = Arc::new(parking_lot::Mutex::new(GoalManager::new()));
        let (signal_tx, _) = mpsc::channel(100);

        let mock_engine = Box::new(crate::MockEngine::new("test"));
        let loop_ = ReflectionLoop::new(mock_engine, store, embedder, goals, signal_tx);

        // No new segments, should not cycle
        assert!(!loop_.should_cycle());
    }

    #[test]
    fn test_should_cycle_true_with_new_segments() {
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let goals = Arc::new(parking_lot::Mutex::new(GoalManager::new()));
        let (signal_tx, _) = mpsc::channel(100);

        let mock_engine = Box::new(crate::MockEngine::new("test"));
        let mut loop_ = ReflectionLoop::new(mock_engine, store.clone(), embedder, goals, signal_tx)
            .with_min_new_segments(2);

        // Set last_cycle to 2 minutes ago so the 60s cooldown is satisfied
        loop_.last_cycle = Utc::now() - chrono::Duration::seconds(120);
        loop_.last_segment_count = 0;

        // Store 3 segments (above min_new_segments threshold)
        for i in 0..3 {
            let seg = Segment::new(
                Content::Text(format!("segment {i}")),
                vec![1.0, 0.0, 0.0, 0.0],
                Source::Manual { description: "test".to_string() },
            );
            store.store(seg).unwrap();
        }

        assert!(loop_.should_cycle());
    }

    #[test]
    fn test_format_reflection_prompt_includes_segments() {
        let seg = Segment::new(
            Content::Text("test knowledge".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::Manual { description: "test".to_string() },
        );
        let prompt = format_reflection_prompt(&[seg], "- Build the thing\n");
        assert!(prompt.contains("test knowledge"));
        assert!(prompt.contains("Active Goals"));
        assert!(prompt.contains("Build the thing"));
    }

    #[tokio::test]
    async fn test_reflection_run_cycle_stores_synthesis() {
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let goals = Arc::new(parking_lot::Mutex::new(GoalManager::new()));
        let (signal_tx, _signal_rx) = mpsc::channel(100);

        // Store some recent segments for reflection to find
        let seg1 = Segment::new(
            Content::Text("User prefers Rust".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::Conversation { thread_id: ThreadId::new(), turn: 0 },
        );
        let id1 = seg1.id;
        store.store(seg1).unwrap();

        let response = serde_json::json!({
            "syntheses": [{
                "content": "User is a Rust developer who values performance",
                "source_segment_ids": [id1.to_string()],
                "decay_class": "Procedural",
                "confidence_rationale": "Direct observation"
            }],
            "contradictions": [],
            "goal_updates": [],
            "signals": []
        });
        let mock_engine = Box::new(crate::MockEngine::new(&response.to_string()));
        let mut loop_ = ReflectionLoop::new(mock_engine, store.clone(), embedder, goals, signal_tx);
        loop_.last_cycle = Utc::now() - chrono::Duration::hours(1);
        loop_.last_segment_count = 0;

        loop_.run_cycle().await;

        // Should have stored the synthesis segment (1 original + 1 synthesis = 2)
        assert_eq!(store.count(None), 2);
    }

    #[tokio::test]
    async fn test_reflection_deduplicates_contradictions() {
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let goals = Arc::new(parking_lot::Mutex::new(GoalManager::new()));
        let (signal_tx, mut signal_rx) = mpsc::channel(100);

        // Store segments for reflection to find
        let seg = Segment::new(
            Content::Text("test".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::Manual { description: "test".to_string() },
        );
        store.store(seg).unwrap();

        let id_a = SegmentId::new();
        let id_b = SegmentId::new();
        let response = serde_json::json!({
            "syntheses": [],
            "contradictions": [{
                "segment_a": id_a,
                "segment_b": id_b,
                "description": "Conflicting info",
                "suggested_resolution": "Check"
            }],
            "goal_updates": [],
            "signals": []
        });
        let mock_engine = Box::new(crate::MockEngine::new(&response.to_string()));
        let mut loop_ = ReflectionLoop::new(mock_engine, store, embedder, goals, signal_tx);
        loop_.last_cycle = Utc::now() - chrono::Duration::hours(1);
        loop_.last_segment_count = 0;

        // First cycle — should signal
        loop_.run_cycle().await;
        assert!(signal_rx.try_recv().is_ok());

        // Second cycle with same contradiction — should NOT signal again
        // Reset last_cycle so it gathers segments again
        loop_.last_cycle = Utc::now() - chrono::Duration::hours(1);
        loop_.run_cycle().await;
        // No new signal for the same contradiction pair
        let mut found_duplicate = false;
        while let Ok(sig) = signal_rx.try_recv() {
            if sig.summary.contains("Conflicting info") {
                found_duplicate = true;
            }
        }
        assert!(!found_duplicate, "should not re-signal same contradiction");
    }

    #[tokio::test]
    async fn test_reflection_sends_insight_signal() {
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let goals = Arc::new(parking_lot::Mutex::new(GoalManager::new()));
        let (signal_tx, mut signal_rx) = mpsc::channel(100);

        let seg = Segment::new(
            Content::Text("test".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::Manual { description: "test".to_string() },
        );
        store.store(seg).unwrap();

        let response = serde_json::json!({
            "syntheses": [],
            "contradictions": [],
            "goal_updates": [],
            "signals": [{
                "priority": "Normal",
                "insight": "Build failures correlate with auth changes",
                "relevant_segments": []
            }]
        });
        let mock_engine = Box::new(crate::MockEngine::new(&response.to_string()));
        let mut loop_ = ReflectionLoop::new(mock_engine, store, embedder, goals, signal_tx);
        loop_.last_cycle = Utc::now() - chrono::Duration::hours(1);
        loop_.last_segment_count = 0;

        loop_.run_cycle().await;

        let signal = signal_rx.try_recv().unwrap();
        assert_eq!(signal.priority, SignalPriority::Normal);
        assert!(signal.summary.contains("Build failures"));
    }
}
