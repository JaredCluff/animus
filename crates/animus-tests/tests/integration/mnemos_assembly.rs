use animus_core::{Content, Segment, SegmentId, Source};
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
fn test_assemble_retrieves_relevant_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    store
        .store(text_segment(
            vec![1.0, 0.0, 0.0, 0.0],
            "knowledge about cats",
        ))
        .unwrap();
    store
        .store(text_segment(
            vec![0.0, 1.0, 0.0, 0.0],
            "knowledge about dogs",
        ))
        .unwrap();
    store
        .store(text_segment(
            vec![0.0, 0.0, 1.0, 0.0],
            "knowledge about rust",
        ))
        .unwrap();

    let assembler = ContextAssembler::new(store, 10_000);

    let context = assembler
        .assemble(&[1.0, 0.0, 0.0, 0.0], &[], 5)
        .unwrap();

    assert!(!context.segments.is_empty());
    match &context.segments[0].content {
        Content::Text(t) => assert!(
            t.contains("cats"),
            "first result should be about cats, got: {t}"
        ),
        _ => panic!("expected text"),
    }
}

#[test]
fn test_assemble_includes_anchor_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let anchor = text_segment(vec![0.0, 0.0, 0.0, 1.0], "conversation anchor");
    let anchor_id = anchor.id;
    store.store(anchor).unwrap();
    store
        .store(text_segment(vec![1.0, 0.0, 0.0, 0.0], "other segment"))
        .unwrap();

    let assembler = ContextAssembler::new(store, 10_000);

    let context = assembler
        .assemble(&[1.0, 0.0, 0.0, 0.0], &[anchor_id], 5)
        .unwrap();

    let ids: Vec<SegmentId> = context.segments.iter().map(|s| s.id).collect();
    assert!(ids.contains(&anchor_id), "anchor segment must be in context");
}

#[test]
fn test_assemble_respects_token_budget() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    for i in 0..20 {
        let embedding = vec![1.0 - (i as f32 * 0.01), i as f32 * 0.01, 0.0, 0.0];
        let text = format!(
            "segment number {i} with some content to take up tokens in the context window budget"
        );
        store.store(text_segment(embedding, &text)).unwrap();
    }

    let assembler = ContextAssembler::new(store, 50);

    let context = assembler
        .assemble(&[1.0, 0.0, 0.0, 0.0], &[], 20)
        .unwrap();

    assert!(context.segments.len() < 20, "should be limited by token budget");
    assert!(context.total_tokens <= 50, "should not exceed budget");
}
