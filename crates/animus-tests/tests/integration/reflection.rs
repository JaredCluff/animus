use animus_core::identity::ThreadId;
use animus_core::segment::{Content, Segment, Source};
use animus_core::EmbeddingService;
use animus_cortex::reflection::ReflectionLoop;
use animus_cortex::telos::GoalManager;
use animus_cortex::MockEngine;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_reflection_cycle_produces_synthesis() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
    let embedder: Arc<dyn EmbeddingService> = Arc::new(animus_embed::SyntheticEmbedding::new(4));
    let goals = Arc::new(parking_lot::Mutex::new(GoalManager::new()));
    let (signal_tx, _) = tokio::sync::mpsc::channel(100);

    // Store some segments for reflection
    for i in 0..3 {
        let seg = Segment::new(
            Content::Text(format!("knowledge {i}")),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::Conversation {
                thread_id: ThreadId::new(),
                turn: i as u64,
            },
        );
        store.store(seg).unwrap();
    }

    let response = serde_json::json!({
        "syntheses": [{
            "content": "Pattern: consistent usage of Rust",
            "source_segment_ids": [],
            "decay_class": "Procedural",
            "confidence_rationale": "Multiple observations"
        }],
        "contradictions": [],
        "goal_updates": [],
        "signals": []
    });
    let engine = Arc::new(MockEngine::new(&response.to_string()));
    let mut reflection = ReflectionLoop::new(engine, store.clone(), embedder, goals, signal_tx)
        .with_min_new_segments(1)
        // Set last_cycle to the past so gather_recent_segments finds our segments
        .with_last_cycle(chrono::Utc::now() - chrono::Duration::hours(1))
        .with_last_segment_count(0);

    reflection.run_cycle().await;

    // 3 original + 1 synthesis = 4
    assert_eq!(store.count(None), 4);
}

#[test]
fn test_reflection_should_cycle_logic() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
    let embedder: Arc<dyn EmbeddingService> = Arc::new(animus_embed::SyntheticEmbedding::new(4));
    let goals = Arc::new(parking_lot::Mutex::new(GoalManager::new()));
    let (signal_tx, _) = tokio::sync::mpsc::channel(100);

    let engine = Arc::new(MockEngine::new("test"));
    let loop_ = ReflectionLoop::new(engine, store.clone(), embedder, goals, signal_tx)
        .with_min_new_segments(2);

    // Fresh loop, no new segments -- should not cycle
    assert!(!loop_.should_cycle());

    // Store segments (but last_cycle is too recent -- was just set to Utc::now())
    for _ in 0..3 {
        store
            .store(Segment::new(
                Content::Text("x".to_string()),
                vec![1.0, 0.0, 0.0, 0.0],
                Source::Manual {
                    description: "test".to_string(),
                },
            ))
            .unwrap();
    }

    // Still should not cycle -- last_cycle is too recent (< 60s)
    assert!(!loop_.should_cycle());
}
