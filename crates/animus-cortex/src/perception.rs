// Imports used by the run logic added in Task 2.
use animus_core::identity::ThreadId;
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_core::sensorium::SensorEvent;
use animus_core::threading::{Signal, SignalPriority};
use animus_core::{ApiTracker, EmbeddingService};
use animus_vectorfs::VectorStore;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, RwLock};

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
    /// One-sentence summary (becomes segment content if stored). May be null when store=false.
    #[serde(default)]
    pub summary: Option<String>,
    /// Decay class assignment. May be null when store=false.
    #[serde(default)]
    pub decay_class: Option<DecayClass>,
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

const BURST_SUMMARY_PROMPT: &str = r#"You are the Perception subsystem of an AILF (AI Life Form). A burst of sensor events has been detected — too many individual events to classify one by one.

You will receive a statistical summary of the burst. Analyze it and produce a single meaningful summary that captures what happened during this burst.

Decide:
1. Should this burst be stored as a single memory? (usually yes unless it's pure noise)
2. Write a 1-2 sentence summary that captures the essence of what happened
3. Assign a decay class: "Factual", "Procedural", "Episodic", "Opinion", or "General"
4. Add relevant tags as key-value pairs
5. Should the Reasoning thread be alerted? If so, at what priority ("Info", "Normal", "Urgent")?

Respond with JSON only:
{
  "events": [
    {
      "event_index": 0,
      "store": true,
      "summary": "Summary of the burst",
      "decay_class": "Episodic",
      "tags": {"category": "bulk_operation"},
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

/// Tracks recently-modified paths to filter out self-generated filesystem events.
/// Tools register paths they modify; the perception loop skips events for those paths
/// within a configurable TTL window.
pub struct SelfEventFilter {
    /// Map of path → timestamp when it was registered.
    entries: RwLock<HashMap<String, Instant>>,
    /// How long a registered path stays in the filter.
    ttl: Duration,
}

impl SelfEventFilter {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// Register a path as recently modified by the AILF itself.
    pub async fn register(&self, path: String) {
        let mut entries = self.entries.write().await;
        entries.insert(path, Instant::now());
    }

    /// Check if an event path should be filtered (is self-generated and not expired).
    pub async fn should_filter(&self, path: &str) -> bool {
        let entries = self.entries.read().await;
        if let Some(timestamp) = entries.get(path) {
            return timestamp.elapsed() < self.ttl;
        }
        false
    }

    /// Clean up expired entries. Call periodically.
    pub async fn cleanup(&self) {
        let mut entries = self.entries.write().await;
        entries.retain(|_, timestamp| timestamp.elapsed() < self.ttl);
    }
}

/// Burst detection state — tracks event rate in a sliding window.
struct BurstState {
    /// Timestamps of recently received events (sliding window).
    event_timestamps: VecDeque<Instant>,
    /// How long the sliding window is.
    window: Duration,
    /// Events-per-second threshold to enter burst mode.
    rate_threshold: f64,
    /// Accumulated events during a burst.
    burst_buffer: Vec<SensorEvent>,
    /// Whether we're currently in burst mode.
    in_burst: bool,
    /// When the current burst started.
    burst_start: Option<Instant>,
    /// How long to wait after events slow down before ending the burst.
    burst_cooldown: Duration,
}

impl BurstState {
    fn new(window: Duration, rate_threshold: f64, burst_cooldown: Duration) -> Self {
        Self {
            event_timestamps: VecDeque::new(),
            window,
            rate_threshold,
            burst_buffer: Vec::new(),
            in_burst: false,
            burst_start: None,
            burst_cooldown,
        }
    }

    /// Record an event and return whether we should enter/remain in burst mode.
    fn record(&mut self) -> bool {
        let now = Instant::now();
        self.event_timestamps.push_back(now);

        // Evict timestamps outside the window
        while let Some(front) = self.event_timestamps.front() {
            if now.duration_since(*front) > self.window {
                self.event_timestamps.pop_front();
            } else {
                break;
            }
        }

        let rate = self.event_timestamps.len() as f64 / self.window.as_secs_f64();
        if rate >= self.rate_threshold {
            if !self.in_burst {
                self.in_burst = true;
                self.burst_start = Some(now);
            }
            true
        } else {
            false
        }
    }

    /// Check if burst mode should end (rate dropped and cooldown elapsed).
    fn should_end_burst(&self) -> bool {
        if !self.in_burst {
            return false;
        }
        let now = Instant::now();
        // Evict expired timestamps to get current rate
        let current_count = self.event_timestamps.iter()
            .filter(|t| now.duration_since(**t) <= self.window)
            .count();
        let rate = current_count as f64 / self.window.as_secs_f64();
        let burst_duration = self.burst_start.map(|s| s.elapsed()).unwrap_or(Duration::ZERO);
        rate < self.rate_threshold && burst_duration >= self.burst_cooldown
    }

    fn reset(&mut self) {
        self.in_burst = false;
        self.burst_start = None;
        self.burst_buffer.clear();
        self.event_timestamps.clear();
    }
}

/// Format a statistical summary of a burst for the LLM.
fn format_burst_summary(events: &[SensorEvent]) -> String {
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    let mut path_counts: HashMap<String, usize> = HashMap::new();
    let mut unique_paths: Vec<String> = Vec::new();

    for event in events {
        let type_key = format!("{:?}", event.event_type);
        *type_counts.entry(type_key).or_insert(0) += 1;

        if let Some(path) = event.data.get("path").and_then(|v| v.as_str()) {
            *path_counts.entry(path.to_string()).or_insert(0) += 1;
            if !unique_paths.contains(&path.to_string()) {
                unique_paths.push(path.to_string());
            }
        }
    }

    let duration = if events.len() >= 2 {
        let first = events.first().unwrap().timestamp;
        let last = events.last().unwrap().timestamp;
        (last - first).num_milliseconds()
    } else {
        0
    };

    let mut msg = format!(
        "BURST DETECTED: {} events over {}ms\n\n",
        events.len(),
        duration
    );

    msg.push_str("Event type breakdown:\n");
    for (event_type, count) in &type_counts {
        msg.push_str(&format!("  {event_type}: {count}\n"));
    }

    // Show top 10 most-changed paths
    let mut sorted_paths: Vec<_> = path_counts.iter().collect();
    sorted_paths.sort_by(|a, b| b.1.cmp(a.1));
    let top_paths: Vec<_> = sorted_paths.iter().take(10).collect();

    if !top_paths.is_empty() {
        msg.push_str("\nMost frequently changed paths:\n");
        for (path, count) in &top_paths {
            msg.push_str(&format!("  {path}: {count} changes\n"));
        }
        if unique_paths.len() > 10 {
            msg.push_str(&format!("  ... and {} more paths\n", unique_paths.len() - 10));
        }
    }

    msg.push_str(&format!(
        "\nThis burst likely represents a bulk filesystem operation (git checkout, build, npm install, etc). \
         The individual events have been aggregated to avoid excessive API calls."
    ));

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
    /// Burst detection configuration.
    burst_window: Duration,
    burst_rate_threshold: f64,
    burst_cooldown: Duration,
    /// Self-event filter — skip events generated by the AILF's own tools.
    self_filter: Option<Arc<SelfEventFilter>>,
    /// API usage tracker for self-awareness.
    api_tracker: Option<Arc<ApiTracker>>,
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
            burst_window: Duration::from_secs(5),
            burst_rate_threshold: 20.0, // 20 events/sec triggers burst mode
            burst_cooldown: Duration::from_secs(3),
            self_filter: None,
            api_tracker: None,
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

    pub fn with_burst_detection(mut self, window: Duration, rate_threshold: f64, cooldown: Duration) -> Self {
        self.burst_window = window;
        self.burst_rate_threshold = rate_threshold;
        self.burst_cooldown = cooldown;
        self
    }

    pub fn with_self_filter(mut self, filter: Arc<SelfEventFilter>) -> Self {
        self.self_filter = Some(filter);
        self
    }

    pub fn with_api_tracker(mut self, tracker: Arc<ApiTracker>) -> Self {
        self.api_tracker = Some(tracker);
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

        // Track the API call
        if let Some(tracker) = &self.api_tracker {
            tracker.record_call_simple();
        }

        match self.engine.reason(PERCEPTION_SYSTEM_PROMPT, &messages, None).await {
            Ok(output) => {
                let json = strip_json_fence(&output.content);
                match serde_json::from_str::<PerceptionOutput>(json) {
                    Ok(perception) => {
                        self.handle_perception_output(perception, &events).await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse perception output: {e}\nRaw: {}", &output.content[..output.content.len().min(200)]);
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
                let summary = match &perceived.summary {
                    Some(s) if !s.is_empty() => s.clone(),
                    _ => continue, // skip if no summary when store=true
                };

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

                match self.embedder.embed_text(&summary).await {
                    Ok(embedding) => {
                        let mut segment = Segment::new(
                            Content::Text(summary),
                            embedding,
                            source,
                        );
                        segment.decay_class = perceived.decay_class.unwrap_or(DecayClass::General);
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

    /// Process a burst of events — aggregates into a single intelligent summary call.
    async fn process_burst(&self, events: Vec<SensorEvent>) {
        if events.is_empty() {
            return;
        }

        tracing::info!(
            "Processing burst of {} events — aggregating into single summary",
            events.len()
        );

        let summary_msg = format_burst_summary(&events);
        let messages = vec![Turn::text(Role::User, &summary_msg)];

        // Track the API call
        if let Some(tracker) = &self.api_tracker {
            tracker.record_call_simple();
        }

        match self.engine.reason(BURST_SUMMARY_PROMPT, &messages, None).await {
            Ok(output) => {
                let json = strip_json_fence(&output.content);
                match serde_json::from_str::<PerceptionOutput>(json) {
                    Ok(perception) => {
                        // For burst summaries, use the first event as provenance
                        self.handle_perception_output(perception, &events).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to parse burst perception output: {e}\nRaw: {}",
                            &output.content[..output.content.len().min(200)]
                        );
                        self.fallback_store_burst(&events).await;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Burst perception engine error: {e}");
                self.fallback_store_burst(&events).await;
            }
        }
    }

    /// Fallback for burst: store a single summary segment instead of individual events.
    async fn fallback_store_burst(&self, events: &[SensorEvent]) {
        if events.is_empty() {
            return;
        }

        // Count event types
        let mut type_counts: HashMap<String, usize> = HashMap::new();
        for event in events {
            *type_counts.entry(format!("{:?}", event.event_type)).or_insert(0) += 1;
        }
        let type_summary: Vec<String> = type_counts.iter()
            .map(|(t, c)| format!("{c} {t}"))
            .collect();

        let summary = format!(
            "Burst of {} events: {}",
            events.len(),
            type_summary.join(", ")
        );

        match self.embedder.embed_text(&summary).await {
            Ok(embedding) => {
                let segment = Segment::new(
                    Content::Text(summary),
                    embedding,
                    Source::Observation {
                        event_type: "Burst".to_string(),
                        raw_event_id: events[0].id,
                    },
                );
                if let Err(e) = self.store.store(segment) {
                    tracing::warn!("Failed to store burst fallback segment: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("Burst fallback embedding failed: {e}");
            }
        }
    }

    /// Check if an event should be filtered as self-generated.
    async fn should_filter_event(&self, event: &SensorEvent) -> bool {
        if let Some(filter) = &self.self_filter {
            if let Some(path) = event.data.get("path").and_then(|v| v.as_str()) {
                return filter.should_filter(path).await;
            }
        }
        false
    }

    /// Run the perception loop, consuming events from the broadcast channel.
    /// This method runs indefinitely — spawn it with `tokio::spawn`.
    pub async fn run(self, mut event_rx: broadcast::Receiver<SensorEvent>) {
        let mut batch: Vec<SensorEvent> = Vec::new();
        let mut batch_start: Option<Instant> = None;
        let mut burst_state = BurstState::new(
            self.burst_window,
            self.burst_rate_threshold,
            self.burst_cooldown,
        );
        let mut cleanup_counter: u64 = 0;

        loop {
            let timeout = batch_start
                .map(|s| self.batch_window.saturating_sub(s.elapsed()))
                .unwrap_or(self.batch_window);

            tokio::select! {
                result = event_rx.recv() => {
                    match result {
                        Ok(event) => {
                            // Self-event filter: skip events generated by the AILF's own tools
                            if self.should_filter_event(&event).await {
                                continue;
                            }

                            let is_burst = burst_state.record();

                            if is_burst {
                                // In burst mode — accumulate, don't process yet
                                burst_state.burst_buffer.push(event);

                                // If burst buffer is very large, force process
                                if burst_state.burst_buffer.len() >= 500 {
                                    self.process_burst(std::mem::take(&mut burst_state.burst_buffer)).await;
                                    burst_state.reset();
                                }
                            } else {
                                // Normal mode — batch as usual
                                if batch.is_empty() {
                                    batch_start = Some(Instant::now());
                                }
                                batch.push(event);
                                if batch.len() >= self.max_batch_size {
                                    self.process_batch(std::mem::take(&mut batch)).await;
                                    batch_start = None;
                                }
                            }

                            // Periodic self-filter cleanup
                            cleanup_counter += 1;
                            if cleanup_counter % 100 == 0 {
                                if let Some(filter) = &self.self_filter {
                                    filter.cleanup().await;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Perception lagged, dropped {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = tokio::time::sleep(timeout), if !batch.is_empty() && !burst_state.in_burst => {
                    self.process_batch(std::mem::take(&mut batch)).await;
                    batch_start = None;
                }
                _ = tokio::time::sleep(Duration::from_secs(1)), if burst_state.should_end_burst() => {
                    // Burst has subsided — process the accumulated burst
                    tracing::info!(
                        "Burst ended — processing {} accumulated events",
                        burst_state.burst_buffer.len()
                    );
                    self.process_burst(std::mem::take(&mut burst_state.burst_buffer)).await;
                    burst_state.reset();
                }
            }
        }

        // Flush remaining events on shutdown
        if !burst_state.burst_buffer.is_empty() {
            self.process_burst(burst_state.burst_buffer).await;
        }
        if !batch.is_empty() {
            self.process_batch(batch).await;
        }
    }
}

/// Strip markdown code fences (` ```json ... ``` ` or ` ``` ... ``` `) from LLM output.
fn strip_json_fence(s: &str) -> &str {
    let s = s.trim();
    let inner = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    inner
        .trim()
        .trim_end_matches("```")
        .trim()
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
        assert_eq!(output.events[0].decay_class, Some(DecayClass::Episodic));
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
        assert_eq!(loop_.burst_rate_threshold, 20.0);
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
