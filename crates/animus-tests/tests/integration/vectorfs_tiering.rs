use animus_core::{Content, Segment, Source, Tier, TierConfig};
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::tier_manager::TierManager;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn test_segment(embedding: Vec<f32>, text: &str) -> Segment {
    Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    )
}

#[test]
fn test_tier_manager_demotes_stale_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "stale segment");
    seg.relevance_score = 0.1;
    seg.confidence = 0.1;
    seg.last_accessed = chrono::Utc::now() - chrono::Duration::hours(2);
    seg.created = chrono::Utc::now() - chrono::Duration::hours(2);
    let id = seg.id;
    store.store(seg).unwrap();

    let config = TierConfig {
        cold_delay_secs: 60,
        recency_max_age_secs: 3600, // 1 hour — so 2-hour-old segment has 0 recency
        ..Default::default()
    };

    let manager = TierManager::new(store.clone(), config);
    manager.run_cycle();

    // Use get_raw to avoid updating last_accessed
    let updated = store.get_raw(id).unwrap().unwrap();
    assert_eq!(
        updated.tier,
        Tier::Cold,
        "stale low-score segment should be demoted to Cold"
    );
}

#[test]
fn test_tier_manager_promotes_accessed_cold_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "accessed cold segment");
    seg.relevance_score = 0.8;
    seg.confidence = 0.9;
    seg.access_count = 100;
    let id = seg.id;
    store.store(seg).unwrap();
    store.set_tier(id, Tier::Cold).unwrap();

    let config = TierConfig::default();
    let manager = TierManager::new(store.clone(), config);
    manager.run_cycle();

    let updated = store.get_raw(id).unwrap().unwrap();
    assert_eq!(
        updated.tier,
        Tier::Warm,
        "frequently accessed high-score Cold segment should be promoted to Warm"
    );
}

#[test]
fn test_tier_manager_ignores_hot_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "hot segment");
    seg.relevance_score = 0.01; // very low score
    let id = seg.id;
    store.store(seg).unwrap();
    store.set_tier(id, Tier::Hot).unwrap();

    let config = TierConfig {
        cold_delay_secs: 0,
        ..Default::default()
    };

    let manager = TierManager::new(store.clone(), config);
    manager.run_cycle();

    let updated = store.get(id).unwrap().unwrap();
    assert_eq!(
        updated.tier,
        Tier::Hot,
        "Hot segments should not be touched by TierManager"
    );
}
