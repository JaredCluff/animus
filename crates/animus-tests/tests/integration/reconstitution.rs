use animus_core::segment::{Content, Segment, Source};
use animus_core::AnimusIdentity;
use animus_core::EmbeddingService;
use animus_cortex::reconstitution::{boot_reconstitution, find_shutdown_segment, shutdown_reflection};
use animus_cortex::telos::GoalManager;
use animus_cortex::MockEngine;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_shutdown_then_boot_reconstitution() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
    let embedder: Arc<dyn EmbeddingService> = Arc::new(animus_embed::SyntheticEmbedding::new(4));
    let identity = AnimusIdentity::generate("mock-model".to_string());
    let goals = GoalManager::new();

    // Simulate a session with some knowledge
    store
        .store(Segment::new(
            Content::Text("Working on auth system".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::Manual {
                description: "test".to_string(),
            },
        ))
        .unwrap();

    // Shutdown
    let shutdown_engine = MockEngine::new("I was helping with the auth system.");
    let shutdown_id = shutdown_reflection(&shutdown_engine, &*store, &*embedder, &goals)
        .await
        .unwrap();
    assert!(shutdown_id.is_some());

    // Verify shutdown segment exists
    let shutdown_seg = find_shutdown_segment(&*store);
    assert!(shutdown_seg.is_some());

    // Boot
    let boot_engine = MockEngine::new("Waking up. I was last working on the auth system.");
    let summary = boot_reconstitution(&boot_engine, &*store, &*embedder, &identity, &goals)
        .await
        .unwrap();
    assert!(summary.is_some());
    assert!(summary.unwrap().contains("auth system"));

    // Should have: 1 knowledge + 1 shutdown + 1 wakeup = 3
    assert_eq!(store.count(None), 3);
}

#[tokio::test]
async fn test_boot_without_shutdown_segment() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
    let embedder: Arc<dyn EmbeddingService> = Arc::new(animus_embed::SyntheticEmbedding::new(4));
    let identity = AnimusIdentity::generate("mock-model".to_string());
    let goals = GoalManager::new();

    // No previous shutdown -- should still work (graceful degradation)
    let engine = MockEngine::new("First boot. No prior context.");
    let summary = boot_reconstitution(&engine, &*store, &*embedder, &identity, &goals)
        .await
        .unwrap();
    assert!(summary.is_some());
}
