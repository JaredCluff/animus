use animus_core::{Content, Segment, SegmentId, Source, Tier};
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
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
fn test_store_and_get() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "hello world");
    let id = seg.id;
    store.store(seg).unwrap();

    let retrieved = store.get(id).unwrap().expect("segment should exist");
    assert_eq!(retrieved.id, id);
    match &retrieved.content {
        Content::Text(t) => assert_eq!(t, "hello world"),
        _ => panic!("expected text content"),
    }
}

#[test]
fn test_query_by_similarity() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let s1 = test_segment(vec![1.0, 0.0, 0.0, 0.0], "north");
    let s2 = test_segment(vec![0.0, 1.0, 0.0, 0.0], "east");
    let s3 = test_segment(vec![0.9, 0.1, 0.0, 0.0], "mostly north");
    let id1 = s1.id;
    let id3 = s3.id;

    store.store(s1).unwrap();
    store.store(s2).unwrap();
    store.store(s3).unwrap();

    let results = store.query(&[1.0, 0.0, 0.0, 0.0], 2, None).unwrap();
    assert_eq!(results.len(), 2);
    let result_ids: Vec<SegmentId> = results.iter().map(|s| s.id).collect();
    assert!(result_ids.contains(&id1));
    assert!(result_ids.contains(&id3));
}

#[test]
fn test_query_with_tier_filter() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let s1 = test_segment(vec![1.0, 0.0, 0.0, 0.0], "warm segment");
    let s2 = test_segment(vec![0.9, 0.1, 0.0, 0.0], "will be cold");
    let id2 = s2.id;

    store.store(s1).unwrap();
    store.store(s2).unwrap();
    store.set_tier(id2, Tier::Cold).unwrap();

    let results = store
        .query(&[1.0, 0.0, 0.0, 0.0], 2, Some(Tier::Warm))
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(&results[0].content, Content::Text(t) if t == "warm segment"));
}

#[test]
fn test_delete() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "to be deleted");
    let id = seg.id;
    store.store(seg).unwrap();
    assert_eq!(store.count(None), 1);

    store.delete(id).unwrap();
    assert_eq!(store.count(None), 0);
    assert!(store.get(id).unwrap().is_none());
}

#[test]
fn test_persistence_across_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();

    let id = {
        let store = MmapVectorStore::open(&path, 4).unwrap();
        let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "persistent");
        let id = seg.id;
        store.store(seg).unwrap();
        store.flush().unwrap();
        id
    };

    let store = MmapVectorStore::open(&path, 4).unwrap();
    let retrieved = store.get(id).unwrap().expect("should persist across reopen");
    match &retrieved.content {
        Content::Text(t) => assert_eq!(t, "persistent"),
        _ => panic!("expected text"),
    }
}

#[test]
fn test_merge() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let s1 = test_segment(vec![1.0, 0.0, 0.0, 0.0], "fact A");
    let s2 = test_segment(vec![0.9, 0.1, 0.0, 0.0], "fact B");
    let id1 = s1.id;
    let id2 = s2.id;
    store.store(s1).unwrap();
    store.store(s2).unwrap();

    let merged = test_segment(vec![0.95, 0.05, 0.0, 0.0], "consolidated fact AB");
    let merged_id = store.merge(vec![id1, id2], merged).unwrap();

    assert!(store.get(id1).unwrap().is_none());
    assert!(store.get(id2).unwrap().is_none());

    let m = store.get(merged_id).unwrap().expect("merged should exist");
    match &m.content {
        Content::Text(t) => assert_eq!(t, "consolidated fact AB"),
        _ => panic!("expected text"),
    }
}

#[test]
fn test_dimension_mismatch_rejected() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let seg = test_segment(vec![1.0, 0.0], "wrong dimensions");
    let result = store.store(seg);
    assert!(result.is_err());
}

#[test]
fn test_update_meta() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "meta test");
    let id = seg.id;
    store.store(seg).unwrap();

    store
        .update_meta(
            id,
            animus_vectorfs::SegmentUpdate {
                relevance_score: Some(0.9),
                confidence: Some(0.95),
                ..Default::default()
            },
        )
        .unwrap();

    let updated = store.get(id).unwrap().unwrap();
    assert!((updated.relevance_score - 0.9).abs() < 1e-6);
    assert!((updated.confidence - 0.95).abs() < 1e-6);
}
