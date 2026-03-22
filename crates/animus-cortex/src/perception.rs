// Imports used by the run logic added in Task 2.
#[allow(unused_imports)]
use animus_core::error::Result;
#[allow(unused_imports)]
use animus_core::identity::{SegmentId, ThreadId};
#[allow(unused_imports)]
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_core::sensorium::SensorEvent;
use animus_core::threading::{Signal, SignalPriority};
use animus_core::EmbeddingService;
use animus_vectorfs::VectorStore;
use std::collections::HashMap;
use std::sync::Arc;
#[allow(unused_imports)]
use std::time::{Duration, Instant};
#[allow(unused_imports)]
use tokio::sync::{broadcast, mpsc};

#[allow(unused_imports)]
use crate::llm::{ReasoningEngine, Role, Turn};

/// Structured output from the Perception model for a batch of events.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PerceptionOutput {
    pub events: Vec<PerceivedEvent>,
}

/// A single event as classified by the Perception model.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PerceivedEvent {
    /// Index into the batch (0-based) identifying which input event this corresponds to.
    pub event_index: usize,
    /// Whether this event should be stored as a segment.
    pub store: bool,
    /// One-sentence summary (becomes segment content if stored).
    pub summary: String,
    /// Decay class assignment.
    pub decay_class: DecayClass,
    /// Tags for categorization.
    #[serde(default)]
    pub tags: HashMap<String, String>,
    /// Signal to send to Reasoning, if any.
    pub signal: Option<PerceptionSignal>,
}

/// A signal the Perception model wants to send to Reasoning.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PerceptionSignal {
    pub priority: SignalPriority,
    pub reason: String,
}

#[allow(dead_code)]
const PERCEPTION_SYSTEM_PROMPT: &str = r#"You are the Perception subsystem of an AILF (AI Life Form). You receive batches of raw sensor events and classify each one.

For each event, decide:
1. Should it be stored? (filter noise like temp files, system processes)
2. If stored, write a one-sentence summary that captures the meaning
3. Assign a decay class: "Factual", "Procedural", "Episodic", "Opinion", or "General"
4. Add relevant tags as key-value pairs
5. Should the Reasoning thread be alerted? If so, at what priority ("Info", "Normal", "Urgent")?

Look for patterns across events in the batch — correlate temporal relationships.

Respond with JSON only:
{
  "events": [
    {
      "event_index": 0,
      "store": true,
      "summary": "One sentence summary",
      "decay_class": "Episodic",
      "tags": {"category": "build"},
      "signal": null
    }
  ]
}"#;

/// Format a batch of sensor events into a user message for the Perception model.
#[allow(dead_code)]
fn format_batch_for_perception(events: &[SensorEvent]) -> String {
    let mut msg = format!("Batch of {} sensor events:\n\n", events.len());
    for (i, event) in events.iter().enumerate() {
        msg.push_str(&format!(
            "Event {i}: type={:?}, source={}, timestamp={}, data={}\n\n",
            event.event_type,
            event.source,
            event.timestamp.format("%H:%M:%S"),
            serde_json::to_string(&event.data).unwrap_or_default(),
        ));
    }
    msg
}

/// Background perception loop — batches sensor events and classifies them via LLM.
#[allow(dead_code)]
pub struct PerceptionLoop<S: VectorStore> {
    engine: Box<dyn ReasoningEngine>,
    store: Arc<S>,
    embedder: Arc<dyn EmbeddingService>,
    signal_tx: mpsc::Sender<Signal>,
    batch_window: Duration,
    max_batch_size: usize,
}

impl<S: VectorStore> PerceptionLoop<S> {
    pub fn new(
        engine: Box<dyn ReasoningEngine>,
        store: Arc<S>,
        embedder: Arc<dyn EmbeddingService>,
        signal_tx: mpsc::Sender<Signal>,
    ) -> Self {
        Self {
            engine,
            store,
            embedder,
            signal_tx,
            batch_window: Duration::from_secs(2),
            max_batch_size: 10,
        }
    }

    pub fn with_batch_window(mut self, window: Duration) -> Self {
        self.batch_window = window;
        self
    }

    pub fn with_max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = size;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perception_output_deserialize() {
        let json = r#"{
            "events": [
                {
                    "event_index": 0,
                    "store": true,
                    "summary": "File main.rs was modified",
                    "decay_class": "Episodic",
                    "tags": {"category": "code"},
                    "signal": null
                },
                {
                    "event_index": 1,
                    "store": false,
                    "summary": "Temp file created",
                    "decay_class": "General",
                    "tags": {},
                    "signal": null
                }
            ]
        }"#;
        let output: PerceptionOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.events.len(), 2);
        assert!(output.events[0].store);
        assert!(!output.events[1].store);
        assert_eq!(output.events[0].decay_class, DecayClass::Episodic);
    }

    #[test]
    fn test_perception_signal_deserialize() {
        let json = r#"{
            "events": [
                {
                    "event_index": 0,
                    "store": true,
                    "summary": "Unknown network connection detected",
                    "decay_class": "Episodic",
                    "tags": {"category": "security"},
                    "signal": {
                        "priority": "Urgent",
                        "reason": "Unusual outbound connection to unknown IP"
                    }
                }
            ]
        }"#;
        let output: PerceptionOutput = serde_json::from_str(json).unwrap();
        let signal = output.events[0].signal.as_ref().unwrap();
        assert_eq!(signal.priority, SignalPriority::Urgent);
        assert!(signal.reason.contains("outbound"));
    }

    #[test]
    fn test_format_batch() {
        use animus_core::identity::EventId;
        use animus_core::sensorium::EventType;

        let event = SensorEvent {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            event_type: EventType::FileChange,
            source: "file_watcher".to_string(),
            data: serde_json::json!({"path": "/tmp/test.rs", "action": "modify"}),
            consent_policy: None,
        };
        let msg = format_batch_for_perception(&[event]);
        assert!(msg.contains("Event 0"));
        assert!(msg.contains("FileChange"));
        assert!(msg.contains("file_watcher"));
    }

    #[test]
    fn test_perception_loop_construction() {
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let (signal_tx, _signal_rx) = mpsc::channel(100);

        let mock_engine = Box::new(crate::MockEngine::new("test"));
        let loop_ = PerceptionLoop::new(mock_engine, store, embedder, signal_tx)
            .with_batch_window(Duration::from_millis(500))
            .with_max_batch_size(5);

        assert_eq!(loop_.batch_window, Duration::from_millis(500));
        assert_eq!(loop_.max_batch_size, 5);
    }
}
