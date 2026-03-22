use animus_core::Content;
use animus_cortex::llm::{MockEngine, ReasoningEngine, Role, Turn};
use animus_cortex::thread::ReasoningThread;
use animus_embed::SyntheticEmbedding;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_reasoning_thread_processes_turn() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 128).unwrap());
    let embedder = SyntheticEmbedding::new(128);
    let engine = MockEngine::new("Hello! I remember things now.");

    let mut thread = ReasoningThread::new(
        "test".to_string(),
        store.clone(),
        8000,
        128,
    );

    let response = thread
        .process_turn("Hi there", "You are a test AILF.", &engine, &embedder)
        .await
        .unwrap();

    assert_eq!(response, "Hello! I remember things now.");
    assert_eq!(thread.turn_count(), 2); // user + assistant
    assert_eq!(thread.stored_turn_ids().len(), 2);
    assert_eq!(store.count(None), 2); // 2 segments stored
}

#[tokio::test]
async fn test_reasoning_thread_stores_conversation_as_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 128).unwrap());
    let embedder = SyntheticEmbedding::new(128);
    let engine = MockEngine::new("I understand.");

    let mut thread = ReasoningThread::new(
        "test".to_string(),
        store.clone(),
        8000,
        128,
    );

    thread
        .process_turn("Remember that I like Rust", "System", &engine, &embedder)
        .await
        .unwrap();

    // Verify both turns are stored as segments
    let ids = thread.stored_turn_ids();
    assert_eq!(ids.len(), 2);

    let user_seg = store.get_raw(ids[0]).unwrap().unwrap();
    match &user_seg.content {
        Content::Text(t) => assert!(t.contains("Rust")),
        _ => panic!("expected text content"),
    }

    let assistant_seg = store.get_raw(ids[1]).unwrap().unwrap();
    match &assistant_seg.content {
        Content::Text(t) => assert_eq!(t, "I understand."),
        _ => panic!("expected text content"),
    }
}

#[tokio::test]
async fn test_multi_turn_stores_all_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 128).unwrap());
    let embedder = SyntheticEmbedding::new(128);
    let engine = MockEngine::new("Response to turn");

    let mut thread = ReasoningThread::new(
        "multi-turn-test".to_string(),
        store.clone(),
        8000,
        128,
    );

    // First turn
    thread.process_turn("First message", "System", &engine, &embedder).await.unwrap();
    assert_eq!(thread.turn_count(), 2);
    assert_eq!(store.count(None), 2);

    // Second turn — should accumulate
    thread.process_turn("Second message", "System", &engine, &embedder).await.unwrap();
    assert_eq!(thread.turn_count(), 4);
    assert_eq!(store.count(None), 4);
    assert_eq!(thread.stored_turn_ids().len(), 4);
}

#[tokio::test]
async fn test_mock_engine_basic() {
    let engine = MockEngine::new("test response");
    let turns = vec![Turn {
        role: Role::User,
        content: "hello".to_string(),
    }];

    let output = engine.reason("system", &turns).await.unwrap();
    assert_eq!(output.content, "test response");
    assert_eq!(engine.model_name(), "mock-engine");
    assert_eq!(engine.context_limit(), 8192);
}
