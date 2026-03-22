use animus_core::{Content, Segment, Source};
use animus_mnemos::assembler::ContextAssembler;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn text_segment(embedding: Vec<f32>, text: &str) -> Segment {
    Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    )
}

#[test]
fn test_eviction_produces_summaries() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    for i in 0..10 {
        let embedding = vec![1.0 - (i as f32 * 0.05), i as f32 * 0.05, 0.0, 0.0];
        let text = format!(
            "important knowledge chunk number {i} that contains valuable information for context"
        );
        store.store(text_segment(embedding, &text)).unwrap();
    }

    let assembler = ContextAssembler::new(store, 100);
    let context = assembler
        .assemble(&[1.0, 0.0, 0.0, 0.0], &[], 10)
        .unwrap();

    assert!(!context.segments.is_empty(), "should include some segments");
    assert!(
        !context.evicted_summaries.is_empty(),
        "should have evicted some segments"
    );

    for evicted in &context.evicted_summaries {
        assert!(!evicted.summary.is_empty());
        assert!(evicted.summary.contains("[Recalled:"));
    }
}

#[test]
fn test_eviction_keeps_highest_relevance() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let mut close = text_segment(vec![0.99, 0.01, 0.0, 0.0], "very relevant");
    close.relevance_score = 0.9;
    let close_id = close.id;
    store.store(close).unwrap();

    let mut far = text_segment(vec![0.0, 0.0, 1.0, 0.0], "not relevant");
    far.relevance_score = 0.1;
    store.store(far).unwrap();

    let assembler = ContextAssembler::new(store, 20);
    let context = assembler
        .assemble(&[1.0, 0.0, 0.0, 0.0], &[], 2)
        .unwrap();

    assert_eq!(context.segments.len(), 1);
    assert_eq!(context.segments[0].id, close_id);
}
