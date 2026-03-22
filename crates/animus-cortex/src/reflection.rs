use animus_core::error::Result;
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
}
