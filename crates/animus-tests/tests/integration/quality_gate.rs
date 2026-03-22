use animus_core::segment::DecayClass;
use animus_core::{Content, EventId, InstanceId, Segment, SegmentId, Source, Tier, TierConfig, ThreadId};
use animus_mnemos::QualityTracker;
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
fn test_segment_bayesian_defaults() {
    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "test");
    assert!((seg.alpha - 1.0).abs() < f32::EPSILON);
    assert!((seg.beta - 1.0).abs() < f32::EPSILON);
    assert!((seg.bayesian_confidence() - 0.5).abs() < 0.01);
    assert_eq!(seg.decay_class, DecayClass::General);
}

#[test]
fn test_positive_feedback_increases_confidence() {
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "good knowledge");
    let before = seg.bayesian_confidence();

    seg.record_positive_feedback();
    seg.record_positive_feedback();
    seg.record_positive_feedback();

    let after = seg.bayesian_confidence();
    assert!(after > before, "positive feedback should increase confidence: {after} > {before}");
    // Beta(4, 1) => 4/5 = 0.8
    assert!((after - 0.8).abs() < 0.01);
}

#[test]
fn test_negative_feedback_decreases_confidence() {
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "bad knowledge");
    let before = seg.bayesian_confidence();

    seg.record_negative_feedback();
    seg.record_negative_feedback();
    seg.record_negative_feedback();

    let after = seg.bayesian_confidence();
    assert!(after < before, "negative feedback should decrease confidence: {after} < {before}");
    // Beta(1, 4) => 1/5 = 0.2
    assert!((after - 0.2).abs() < 0.01);
}

#[test]
fn test_bayesian_confidence_convergence() {
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "converging knowledge");

    // Simulate 70% acceptance rate over 100 observations
    for _ in 0..70 {
        seg.record_positive_feedback();
    }
    for _ in 0..30 {
        seg.record_negative_feedback();
    }

    let conf = seg.bayesian_confidence();
    // Beta(71, 31) => 71/102 ≈ 0.696
    assert!(
        (conf - 0.696).abs() < 0.01,
        "confidence should converge to ~0.696, got {conf}"
    );
}

#[test]
fn test_decay_class_half_lives_ordered() {
    let factual = DecayClass::Factual.half_life_secs();
    let procedural = DecayClass::Procedural.half_life_secs();
    let episodic = DecayClass::Episodic.half_life_secs();
    let opinion = DecayClass::Opinion.half_life_secs();

    assert!(factual > procedural, "factual should decay slower than procedural");
    assert!(procedural > episodic, "procedural should decay slower than episodic");
    assert!(episodic > opinion, "episodic should decay slower than opinion");
}

#[test]
fn test_temporal_decay_new_segment() {
    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "brand new");
    let decay = seg.temporal_decay_factor();
    assert!(decay > 0.99, "brand new segment should have decay ~1.0, got {decay}");
}

#[test]
fn test_temporal_decay_old_segment() {
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "old knowledge");
    seg.created = chrono::Utc::now() - chrono::Duration::days(60);
    seg.decay_class = DecayClass::Opinion; // 7-day half-life

    let decay = seg.temporal_decay_factor();
    // After 60 days with 7-day half-life: ~8.5 half-lives => very small
    assert!(decay < 0.01, "60-day-old opinion should have very low decay, got {decay}");
}

#[test]
fn test_factual_decays_slower_than_opinion() {
    let mut factual = test_segment(vec![1.0, 0.0, 0.0, 0.0], "factual");
    factual.created = chrono::Utc::now() - chrono::Duration::days(30);
    factual.decay_class = DecayClass::Factual;

    let mut opinion = test_segment(vec![1.0, 0.0, 0.0, 0.0], "opinion");
    opinion.created = chrono::Utc::now() - chrono::Duration::days(30);
    opinion.decay_class = DecayClass::Opinion;

    assert!(
        factual.temporal_decay_factor() > opinion.temporal_decay_factor(),
        "factual should decay slower than opinion at same age"
    );
}

#[test]
fn test_health_score_combines_signals() {
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "test");

    // Baseline health
    let baseline = seg.health_score();

    // Improve all signals
    for _ in 0..10 {
        seg.record_positive_feedback();
    }
    seg.relevance_score = 0.9;
    seg.access_count = 50;

    let improved = seg.health_score();
    assert!(
        improved > baseline,
        "improved signals should give higher health: {improved} > {baseline}"
    );
}

#[test]
fn test_quality_tracker_syncs_to_segment() {
    let mut tracker = QualityTracker::new();
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "synced");

    // Record feedback in tracker
    for _ in 0..5 {
        tracker.record_acceptance(seg.id);
    }
    tracker.record_correction(seg.id);

    // Sync to segment
    tracker.sync_to_segment(&mut seg);

    // Beta(6, 2) => 6/8 = 0.75
    assert!((seg.bayesian_confidence() - 0.75).abs() < 0.01);
    assert!((seg.confidence - 0.75).abs() < 0.01);
}

#[test]
fn test_bayesian_params_persist_in_vectorstore() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();

    let id = {
        let store = MmapVectorStore::open(&path, 4).unwrap();
        let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "persistent bayesian");
        seg.record_positive_feedback();
        seg.record_positive_feedback();
        seg.decay_class = DecayClass::Factual;
        let id = seg.id;
        store.store(seg).unwrap();
        store.flush().unwrap();
        id
    };

    // Reopen and verify Bayesian params survived
    let store = MmapVectorStore::open(&path, 4).unwrap();
    let loaded = store.get(id).unwrap().expect("should persist");
    assert!((loaded.alpha - 3.0).abs() < f32::EPSILON);
    assert!((loaded.beta - 1.0).abs() < f32::EPSILON);
    assert_eq!(loaded.decay_class, DecayClass::Factual);
}

#[test]
fn test_tier_manager_uses_bayesian_confidence() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Create a segment with many corrections (low Bayesian confidence)
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "corrected often");
    seg.relevance_score = 0.1;
    for _ in 0..10 {
        seg.record_negative_feedback();
    }
    seg.last_accessed = chrono::Utc::now() - chrono::Duration::hours(2);
    seg.created = chrono::Utc::now() - chrono::Duration::hours(2);
    let id = seg.id;
    store.store(seg).unwrap();

    let config = TierConfig {
        cold_delay_secs: 60,
        recency_max_age_secs: 3600,
        ..Default::default()
    };

    let manager = TierManager::new(store.clone(), config);
    manager.run_cycle();

    let updated = store.get_raw(id).unwrap().unwrap();
    assert_eq!(
        updated.tier,
        Tier::Cold,
        "segment with low Bayesian confidence should be demoted"
    );
}

#[test]
fn test_tier_manager_respects_high_bayesian_confidence() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Create a segment with many acceptances (high Bayesian confidence)
    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "accepted often");
    seg.relevance_score = 0.8;
    seg.access_count = 50;
    for _ in 0..20 {
        seg.record_positive_feedback();
    }
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
        "segment with high Bayesian confidence should be promoted"
    );
}

#[test]
fn test_update_meta_sets_decay_class() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "decay test");
    let id = seg.id;
    store.store(seg).unwrap();

    store
        .update_meta(
            id,
            animus_vectorfs::SegmentUpdate {
                decay_class: Some(DecayClass::Factual),
                alpha: Some(5.0),
                beta: Some(1.0),
                ..Default::default()
            },
        )
        .unwrap();

    let updated = store.get(id).unwrap().unwrap();
    assert_eq!(updated.decay_class, DecayClass::Factual);
    assert!((updated.alpha - 5.0).abs() < f32::EPSILON);
    assert!((updated.beta - 1.0).abs() < f32::EPSILON);
}

#[test]
fn test_auto_classify_observation() {
    let mut seg = Segment::new(
        Content::Text("file changed".to_string()),
        vec![1.0, 0.0, 0.0, 0.0],
        Source::Observation {
            event_type: "file_change".to_string(),
            raw_event_id: EventId::new(),
        },
    );
    seg.infer_decay_class();
    assert_eq!(seg.decay_class, DecayClass::Episodic);
}

#[test]
fn test_auto_classify_conversation() {
    let mut seg = Segment::new(
        Content::Text("hello".to_string()),
        vec![1.0, 0.0, 0.0, 0.0],
        Source::Conversation {
            thread_id: ThreadId::new(),
            turn: 0,
        },
    );
    seg.infer_decay_class();
    assert_eq!(seg.decay_class, DecayClass::General);
}

#[test]
fn test_auto_classify_consolidation() {
    let mut seg = Segment::new(
        Content::Text("merged fact".to_string()),
        vec![1.0, 0.0, 0.0, 0.0],
        Source::Consolidation {
            merged_from: vec![SegmentId::new(), SegmentId::new()],
        },
    );
    seg.infer_decay_class();
    assert_eq!(seg.decay_class, DecayClass::Factual);
}

#[test]
fn test_auto_classify_federation() {
    let mut seg = Segment::new(
        Content::Text("from peer".to_string()),
        vec![1.0, 0.0, 0.0, 0.0],
        Source::Federation {
            source_ailf: InstanceId::new(),
            original_id: SegmentId::new(),
        },
    );
    seg.infer_decay_class();
    assert_eq!(seg.decay_class, DecayClass::General);
}

#[test]
fn test_auto_classify_self_derived() {
    let mut seg = Segment::new(
        Content::Text("I reasoned this".to_string()),
        vec![1.0, 0.0, 0.0, 0.0],
        Source::SelfDerived {
            reasoning_chain: "because X".to_string(),
        },
    );
    seg.infer_decay_class();
    assert_eq!(seg.decay_class, DecayClass::Procedural);
}
