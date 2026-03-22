use animus_core::{Content, Segment, SegmentId, Source, Tier, TierConfig};
use animus_mnemos::assembler::ContextAssembler;
use animus_mnemos::consolidator::Consolidator;
use animus_mnemos::quality::QualityTracker;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::tier_manager::TierManager;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn text_segment(embedding: Vec<f32>, text: &str, confidence: f32) -> Segment {
    let mut seg = Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    );
    seg.confidence = confidence;
    seg
}

#[test]
fn test_full_pipeline_store_retrieve_assemble() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let facts = vec![
        (vec![1.0, 0.0, 0.0, 0.0], "The user prefers Rust for systems programming", 0.8),
        (vec![0.9, 0.1, 0.0, 0.0], "The user has experience with NexiBot", 0.7),
        (vec![0.0, 1.0, 0.0, 0.0], "Knowledge Nexus uses PostgreSQL", 0.9),
        (vec![0.0, 0.0, 1.0, 0.0], "The weather today is sunny", 0.3),
        (vec![0.0, 0.0, 0.0, 1.0], "K2K is a federation protocol", 0.9),
    ];

    for (emb, text, conf) in &facts {
        store
            .store(text_segment(emb.clone(), text, *conf))
            .unwrap();
    }

    let assembler = ContextAssembler::new(store.clone(), 10_000);
    let context = assembler
        .assemble(&[0.95, 0.05, 0.0, 0.0], &[], 3)
        .unwrap();

    assert!(!context.segments.is_empty());
    let first_text = match &context.segments[0].content {
        Content::Text(t) => t.clone(),
        _ => panic!("expected text"),
    };
    assert!(
        first_text.contains("Rust") || first_text.contains("NexiBot"),
        "should retrieve programming-related segment, got: {first_text}"
    );
}

#[test]
fn test_full_pipeline_consolidation() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    store
        .store(text_segment(
            vec![1.0, 0.0, 0.0, 0.0],
            "The user likes Rust",
            0.7,
        ))
        .unwrap();
    store
        .store(text_segment(
            vec![0.999, 0.001, 0.0, 0.0],
            "The user prefers Rust",
            0.8,
        ))
        .unwrap();
    store
        .store(text_segment(
            vec![0.0, 1.0, 0.0, 0.0],
            "Unrelated fact",
            0.5,
        ))
        .unwrap();

    assert_eq!(store.count(None), 3);

    let consolidator = Consolidator::new(store.clone(), 0.95);
    let report = consolidator.run_cycle().unwrap();

    assert!(
        report.segments_merged >= 2,
        "should merge the near-duplicate pair"
    );
    assert_eq!(
        store.count(None),
        2,
        "should have consolidated down to 2 segments"
    );
}

#[test]
fn test_full_pipeline_tier_lifecycle() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let mut seg = text_segment(vec![1.0, 0.0, 0.0, 0.0], "fresh knowledge", 0.8);
    seg.relevance_score = 0.05;
    seg.last_accessed = chrono::Utc::now() - chrono::Duration::hours(2);
    let id = seg.id;
    store.store(seg).unwrap();

    // Use get_raw to avoid updating last_accessed
    let s = store.get_raw(id).unwrap().unwrap();
    assert_eq!(s.tier, Tier::Warm);

    let config = TierConfig {
        cold_delay_secs: 1,
        recency_max_age_secs: 3600, // 1 hour — so 2-hour-old segment has 0 recency
        ..Default::default()
    };
    let tier_manager = TierManager::new(store.clone(), config);
    tier_manager.run_cycle();

    let s = store.get_raw(id).unwrap().unwrap();
    assert_eq!(s.tier, Tier::Cold, "stale segment should be Cold");
}

#[test]
fn test_full_pipeline_quality_tracking() {
    let mut tracker = QualityTracker::new();
    let good_id = SegmentId::new();
    let bad_id = SegmentId::new();

    for _ in 0..5 {
        tracker.record_acceptance(good_id);
    }

    tracker.record_acceptance(bad_id);
    for _ in 0..4 {
        tracker.record_correction(bad_id);
    }

    let good_adj = tracker.confidence_adjustment(good_id);
    let bad_adj = tracker.confidence_adjustment(bad_id);

    assert!(
        good_adj > 0.0,
        "well-accepted knowledge should boost confidence"
    );
    assert!(
        bad_adj < 0.0,
        "frequently-corrected knowledge should reduce confidence"
    );
    assert!(
        good_adj > bad_adj,
        "good should have higher adjustment than bad"
    );
}

#[test]
fn test_full_pipeline_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();

    let ids: Vec<SegmentId> = {
        let store = MmapVectorStore::open(&path, 4).unwrap();
        let mut ids = Vec::new();
        for i in 0..5 {
            let seg = text_segment(
                vec![i as f32 * 0.2, 1.0 - i as f32 * 0.2, 0.0, 0.0],
                &format!("persistent fact {i}"),
                0.8,
            );
            ids.push(seg.id);
            store.store(seg).unwrap();
        }
        store.flush().unwrap();
        ids
    };

    let store = MmapVectorStore::open(&path, 4).unwrap();
    assert_eq!(store.count(None), 5);
    for id in &ids {
        assert!(
            store.get(*id).unwrap().is_some(),
            "segment {id} should persist"
        );
    }
}
