// Imports used by the run logic added in Task 2.
use animus_core::identity::ThreadId;
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_core::sensorium::SensorEvent;
use animus_core::threading::{Signal, SignalPriority};
use animus_core::EmbeddingService;
use animus_vectorfs::VectorStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

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
pub struct PerceptionLoop<S: VectorStore> {
    engine: Box<dyn ReasoningEngine>,
    store: Arc<S>,
    embedder: Arc<dyn EmbeddingService>,
    signal_tx: mpsc::Sender<Signal>,
    batch_window: Duration,
    max_batch_size: usize,
    /// Stable identity for this loop, used as signal source.
    source_id: ThreadId,
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
            source_id: ThreadId::new(),
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

    /// Process a batch of events through the Perception model.
    /// Public for testing; called internally by run().
    pub async fn process_batch(&self, events: Vec<SensorEvent>) {
        if events.is_empty() {
            return;
        }

        let user_msg = format_batch_for_perception(&events);
        let messages = vec![Turn::text(Role::User, &user_msg)];

        match self.engine.reason(PERCEPTION_SYSTEM_PROMPT, &messages, None).await {
            Ok(output) => {
                match serde_json::from_str::<PerceptionOutput>(&output.content) {
                    Ok(perception) => {
                        self.handle_perception_output(perception, &events).await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse perception output: {e}");
                        self.fallback_store(&events).await;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Perception engine error: {e}");
                self.fallback_store(&events).await;
            }
        }
    }

    /// Handle parsed perception output — store segments and send signals.
    async fn handle_perception_output(
        &self,
        output: PerceptionOutput,
        events: &[SensorEvent],
    ) {
        for perceived in output.events {
            if perceived.store {
                // Determine the source event (for provenance)
                let source = if perceived.event_index < events.len() {
                    let ev = &events[perceived.event_index];
                    Source::Observation {
                        event_type: format!("{:?}", ev.event_type),
                        raw_event_id: ev.id,
                    }
                } else {
                    Source::Observation {
                        event_type: "unknown".to_string(),
                        raw_event_id: animus_core::identity::EventId::new(),
                    }
                };

                match self.embedder.embed_text(&perceived.summary).await {
                    Ok(embedding) => {
                        let mut segment = Segment::new(
                            Content::Text(perceived.summary.clone()),
                            embedding,
                            source,
                        );
                        segment.decay_class = perceived.decay_class;
                        segment.tags = perceived.tags.clone();
                        if let Err(e) = self.store.store(segment) {
                            tracing::warn!("Failed to store perception segment: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Embedding failed for perception: {e}");
                    }
                }
            }

            if let Some(signal) = &perceived.signal {
                let sig = Signal {
                    source_thread: self.source_id,
                    target_thread: ThreadId::default(),
                    priority: signal.priority,
                    summary: signal.reason.clone(),
                    segment_refs: vec![],
                    created: chrono::Utc::now(),
                };
                if self.signal_tx.send(sig).await.is_err() {
                    tracing::warn!("Signal channel closed — Reasoning may not receive perception signal");
                }
            }
        }
    }

    /// Fallback: store events mechanically when the Perception model is unavailable.
    async fn fallback_store(&self, events: &[SensorEvent]) {
        for event in events {
            let text = serde_json::to_string(&event.data).unwrap_or_default();
            match self.embedder.embed_text(&text).await {
                Ok(embedding) => {
                    let mut segment = Segment::new(
                        Content::Structured(event.data.clone()),
                        embedding,
                        Source::Observation {
                            event_type: format!("{:?}", event.event_type),
                            raw_event_id: event.id,
                        },
                    );
                    segment.infer_decay_class();
                    if let Err(e) = self.store.store(segment) {
                        tracing::warn!("Failed to store fallback observation: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("Fallback embedding failed: {e}");
                }
            }
        }
    }

    /// Run the perception loop, consuming events from the broadcast channel.
    /// This method runs indefinitely — spawn it with `tokio::spawn`.
    pub async fn run(self, mut event_rx: broadcast::Receiver<SensorEvent>) {
        let mut batch: Vec<SensorEvent> = Vec::new();
        let mut batch_start: Option<Instant> = None;

        loop {
            let timeout = batch_start
                .map(|s| self.batch_window.saturating_sub(s.elapsed()))
                .unwrap_or(self.batch_window);

            tokio::select! {
                result = event_rx.recv() => {
                    match result {
                        Ok(event) => {
                            if batch.is_empty() {
                                batch_start = Some(Instant::now());
                            }
                            batch.push(event);
                            if batch.len() >= self.max_batch_size {
                                self.process_batch(std::mem::take(&mut batch)).await;
                                batch_start = None;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Perception lagged, dropped {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = tokio::time::sleep(timeout), if !batch.is_empty() => {
                    self.process_batch(std::mem::take(&mut batch)).await;
                    batch_start = None;
                }
            }
        }

        // Flush remaining events on shutdown
        if !batch.is_empty() {
            self.process_batch(batch).await;
        }
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

    #[tokio::test]
    async fn test_perception_process_batch_stores_segments() {
        use animus_core::identity::EventId;
        use animus_core::sensorium::EventType;
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let (signal_tx, mut signal_rx) = mpsc::channel(100);

        // MockEngine that returns valid perception JSON
        let response = serde_json::json!({
            "events": [
                {
                    "event_index": 0,
                    "store": true,
                    "summary": "Source file main.rs was modified",
                    "decay_class": "Episodic",
                    "tags": {"category": "code"},
                    "signal": null
                },
                {
                    "event_index": 1,
                    "store": false,
                    "summary": "Temp file noise",
                    "decay_class": "General",
                    "tags": {},
                    "signal": null
                }
            ]
        });
        let mock_engine = Box::new(crate::MockEngine::new(&response.to_string()));
        let perception = PerceptionLoop::new(mock_engine, store.clone(), embedder, signal_tx);

        let events = vec![
            SensorEvent {
                id: EventId::new(),
                timestamp: chrono::Utc::now(),
                event_type: EventType::FileChange,
                source: "file_watcher".to_string(),
                data: serde_json::json!({"path": "main.rs"}),
                consent_policy: None,
            },
            SensorEvent {
                id: EventId::new(),
                timestamp: chrono::Utc::now(),
                event_type: EventType::FileChange,
                source: "file_watcher".to_string(),
                data: serde_json::json!({"path": "/tmp/cache.tmp"}),
                consent_policy: None,
            },
        ];

        perception.process_batch(events).await;

        // Only the first event (store=true) should be stored
        assert_eq!(store.count(None), 1);
        // No signals sent
        assert!(signal_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_perception_process_batch_sends_signal() {
        use animus_core::identity::EventId;
        use animus_core::sensorium::EventType;
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let (signal_tx, mut signal_rx) = mpsc::channel(100);

        let response = serde_json::json!({
            "events": [{
                "event_index": 0,
                "store": true,
                "summary": "Unknown outbound connection",
                "decay_class": "Episodic",
                "tags": {"category": "security"},
                "signal": {"priority": "Urgent", "reason": "Suspicious network activity"}
            }]
        });
        let mock_engine = Box::new(crate::MockEngine::new(&response.to_string()));
        let perception = PerceptionLoop::new(mock_engine, store, embedder, signal_tx);

        let events = vec![SensorEvent {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            event_type: EventType::Network,
            source: "network_monitor".to_string(),
            data: serde_json::json!({"ip": "192.168.1.99"}),
            consent_policy: None,
        }];

        perception.process_batch(events).await;

        let signal = signal_rx.try_recv().unwrap();
        assert_eq!(signal.priority, SignalPriority::Urgent);
        assert!(signal.summary.contains("Suspicious"));
    }

    #[tokio::test]
    async fn test_perception_fallback_on_parse_failure() {
        use animus_core::identity::EventId;
        use animus_core::sensorium::EventType;
        use animus_vectorfs::store::MmapVectorStore;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let (signal_tx, _signal_rx) = mpsc::channel(100);

        // MockEngine returns unparseable text — should trigger fallback
        let mock_engine = Box::new(crate::MockEngine::new("I don't understand the events"));
        let perception = PerceptionLoop::new(mock_engine, store.clone(), embedder, signal_tx);

        let events = vec![SensorEvent {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            event_type: EventType::FileChange,
            source: "file_watcher".to_string(),
            data: serde_json::json!({"path": "important.rs"}),
            consent_policy: None,
        }];

        perception.process_batch(events).await;

        // Fallback should store the event mechanically
        assert_eq!(store.count(None), 1);
    }
}
