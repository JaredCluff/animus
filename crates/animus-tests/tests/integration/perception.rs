use animus_core::identity::EventId;
use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EmbeddingService;
use animus_cortex::perception::PerceptionLoop;
use animus_cortex::MockEngine;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_perception_pipeline_store_and_filter() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
    let embedder: Arc<dyn EmbeddingService> = Arc::new(animus_embed::SyntheticEmbedding::new(4));
    let (signal_tx, _signal_rx) = tokio::sync::mpsc::channel(100);

    let response = serde_json::json!({
        "events": [
            {"event_index": 0, "store": true, "summary": "Important file changed", "decay_class": "Episodic", "tags": {}, "signal": null},
            {"event_index": 1, "store": false, "summary": "Noise", "decay_class": "General", "tags": {}, "signal": null},
            {"event_index": 2, "store": true, "summary": "Build completed", "decay_class": "Episodic", "tags": {"category": "build"}, "signal": null}
        ]
    });
    let engine = Box::new(MockEngine::new(&response.to_string()));
    let perception = PerceptionLoop::new(engine, store.clone(), embedder, signal_tx);

    let events: Vec<SensorEvent> = (0..3)
        .map(|_| SensorEvent {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            event_type: EventType::FileChange,
            source: "test".to_string(),
            data: serde_json::json!({"path": "test.rs"}),
            consent_policy: None,
        })
        .collect();

    perception.process_batch(events).await;

    // 2 stored (indices 0 and 2), 1 filtered
    assert_eq!(store.count(None), 2);
}

#[tokio::test]
async fn test_perception_fallback_stores_all_on_engine_failure() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
    let embedder: Arc<dyn EmbeddingService> = Arc::new(animus_embed::SyntheticEmbedding::new(4));
    let (signal_tx, _signal_rx) = tokio::sync::mpsc::channel(100);

    // Invalid JSON response triggers fallback (parse failure path)
    let engine = Box::new(MockEngine::new("not valid json"));
    let perception = PerceptionLoop::new(engine, store.clone(), embedder, signal_tx);

    let events: Vec<SensorEvent> = (0..3)
        .map(|_| SensorEvent {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            event_type: EventType::FileChange,
            source: "test".to_string(),
            data: serde_json::json!({"path": "test.rs"}),
            consent_policy: None,
        })
        .collect();

    perception.process_batch(events).await;

    // Fallback stores all events mechanically
    assert_eq!(store.count(None), 3);
}
