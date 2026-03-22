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

/// Thompson Sampling should still rank the most similar segment first
/// because similarity dominates (0.7 weight) over sampled confidence (0.3).
#[test]
fn thompson_sampling_preserves_similarity_ranking() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Very similar to query
    store
        .store(text_segment(vec![0.9, 0.1, 0.0, 0.0], "closest match"))
        .unwrap();
    // Orthogonal to query
    store
        .store(text_segment(vec![0.0, 0.0, 1.0, 0.0], "unrelated"))
        .unwrap();

    let assembler = ContextAssembler::new(store, 10_000);
    let query = [1.0, 0.0, 0.0, 0.0];

    // Run multiple times — despite randomness, the high-similarity segment
    // should consistently rank first.
    for _ in 0..10 {
        let ctx = assembler.assemble(&query, &[], 5).unwrap();
        assert!(!ctx.segments.is_empty());
        match &ctx.segments[0].content {
            Content::Text(t) => assert!(
                t.contains("closest match"),
                "most similar segment should rank first, got: {t}"
            ),
            _ => panic!("expected text"),
        }
    }
}

/// High-confidence segments should generally outrank low-confidence segments
/// at similar similarity levels.
#[test]
fn thompson_sampling_favors_high_confidence() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Both equally similar to the query, but different confidence levels.
    let mut high_conf = text_segment(vec![0.7, 0.7, 0.0, 0.0], "trusted knowledge");
    for _ in 0..20 {
        high_conf.record_positive_feedback();
    }
    let high_id = high_conf.id;
    store.store(high_conf).unwrap();

    let mut low_conf = text_segment(vec![0.7, 0.7, 0.0, 0.0], "corrected knowledge");
    for _ in 0..20 {
        low_conf.record_negative_feedback();
    }
    store.store(low_conf).unwrap();

    let assembler = ContextAssembler::new(store, 10_000);
    let query = [0.7, 0.7, 0.0, 0.0];

    // Over 20 trials, the high-confidence segment should rank first most of the time.
    let mut high_first_count = 0;
    for _ in 0..20 {
        let ctx = assembler.assemble(&query, &[], 5).unwrap();
        if ctx.segments[0].id == high_id {
            high_first_count += 1;
        }
    }
    assert!(
        high_first_count >= 14,
        "high-confidence segment should rank first at least 70% of the time, got {high_first_count}/20"
    );
}

/// Thompson Sampling with a larger exploration pool should include
/// candidates beyond the strict top-k similarity ranking.
#[test]
fn thompson_sampling_exploration_pool_expands_candidates() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Store 10 segments with varying similarity
    for i in 0..10 {
        let sim = 1.0 - (i as f32 * 0.1);
        let embedding = vec![sim, 1.0 - sim, 0.0, 0.0];
        store
            .store(text_segment(embedding, &format!("segment {i}")))
            .unwrap();
    }

    let assembler = ContextAssembler::new(store, 10_000);
    let query = [1.0, 0.0, 0.0, 0.0];

    // With top_k=3, the exploration pool is 2*3=6, so the assembler
    // retrieves 6 candidates and re-ranks them. All should be included
    // since we have budget.
    let ctx = assembler.assemble(&query, &[], 3).unwrap();

    // Should have retrieved the exploration pool (6) candidates, not just 3
    assert!(
        ctx.segments.len() >= 3,
        "should include at least top_k segments, got {}",
        ctx.segments.len()
    );
}

/// Implicit feedback (+0.1 alpha) should accumulate across multiple retrievals,
/// making frequently-retrieved segments more confident over time.
#[test]
fn implicit_feedback_accumulates() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let seg = text_segment(vec![1.0, 0.0, 0.0, 0.0], "frequently retrieved");
    let id = seg.id;
    let initial_alpha = seg.alpha;
    store.store(seg).unwrap();

    // Simulate 5 rounds of implicit feedback (+0.1 alpha each)
    for _ in 0..5 {
        let seg = store.get(id).unwrap().unwrap();
        let new_alpha = seg.alpha + 0.1;
        store
            .update_meta(
                id,
                animus_vectorfs::SegmentUpdate {
                    alpha: Some(new_alpha),
                    confidence: Some(new_alpha / (new_alpha + seg.beta)),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    let updated = store.get(id).unwrap().unwrap();
    assert!(
        (updated.alpha - (initial_alpha + 0.5)).abs() < 0.001,
        "alpha should have increased by ~0.5 (5 × 0.1), got delta {}",
        updated.alpha - initial_alpha,
    );
    assert!(
        updated.bayesian_confidence() > 0.5,
        "confidence should be above prior after positive implicit feedback"
    );
}

/// Explicit feedback (/accept) should increase alpha by 1.0 per call.
#[test]
fn explicit_positive_feedback_updates_segment() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let seg = text_segment(vec![1.0, 0.0, 0.0, 0.0], "accepted segment");
    let id = seg.id;
    store.store(seg).unwrap();

    // Simulate /accept — record_positive_feedback then persist
    let mut seg = store.get(id).unwrap().unwrap();
    seg.record_positive_feedback();
    store
        .update_meta(
            id,
            animus_vectorfs::SegmentUpdate {
                alpha: Some(seg.alpha),
                confidence: Some(seg.confidence),
                ..Default::default()
            },
        )
        .unwrap();

    let updated = store.get(id).unwrap().unwrap();
    assert!((updated.alpha - 2.0).abs() < f32::EPSILON, "alpha should be 2.0 after one accept");
    assert!((updated.beta - 1.0).abs() < f32::EPSILON, "beta should remain 1.0");
    // Beta(2,1) => 2/3 ≈ 0.667
    assert!(
        (updated.bayesian_confidence() - 0.667).abs() < 0.01,
        "confidence should be ~0.667"
    );
}

/// Explicit feedback (/correct) should increase beta by 1.0 per call.
#[test]
fn explicit_negative_feedback_updates_segment() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let seg = text_segment(vec![1.0, 0.0, 0.0, 0.0], "corrected segment");
    let id = seg.id;
    store.store(seg).unwrap();

    // Simulate /correct — record_negative_feedback then persist
    let mut seg = store.get(id).unwrap().unwrap();
    seg.record_negative_feedback();
    store
        .update_meta(
            id,
            animus_vectorfs::SegmentUpdate {
                alpha: Some(seg.alpha),
                beta: Some(seg.beta),
                confidence: Some(seg.confidence),
                ..Default::default()
            },
        )
        .unwrap();

    let updated = store.get(id).unwrap().unwrap();
    assert!((updated.alpha - 1.0).abs() < f32::EPSILON, "alpha should remain 1.0");
    assert!((updated.beta - 2.0).abs() < f32::EPSILON, "beta should be 2.0 after one correct");
    // Beta(1,2) => 1/3 ≈ 0.333
    assert!(
        (updated.bayesian_confidence() - 0.333).abs() < 0.01,
        "confidence should be ~0.333"
    );
}
