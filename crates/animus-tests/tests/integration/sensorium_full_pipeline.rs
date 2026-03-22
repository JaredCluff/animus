use animus_core::sensorium::*;
use animus_core::PolicyId;
use animus_sensorium::bus::EventBus;
use animus_sensorium::orchestrator::SensoriumOrchestrator;
use animus_sensorium::sensors::file_watcher::FileWatcher;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn full_pipeline_file_change_to_segment() {
    let dir = TempDir::new().unwrap();
    let watch_dir = dir.path().join("watched");
    std::fs::create_dir_all(&watch_dir).unwrap();

    let vectorfs_dir = dir.path().join("vectorfs");
    let dim = 128;
    let store = Arc::new(MmapVectorStore::open(&vectorfs_dir, dim).unwrap());

    let bus = Arc::new(EventBus::new(100));

    // Consent: allow everything
    let policies = vec![ConsentPolicy {
        id: PolicyId::new(),
        name: "test-allow".to_string(),
        rules: vec![ConsentRule {
            event_types: vec![EventType::FileChange],
            scope: Scope::All,
            permission: Permission::Allow,
            audit_level: AuditLevel::Full,
        }],
        active: true,
        created: chrono::Utc::now(),
    }];

    let audit_path = dir.path().join("audit.jsonl");
    let orchestrator = Arc::new(
        SensoriumOrchestrator::new(policies, vec![], audit_path.clone(), 0.5).unwrap(),
    );

    // Start background processing
    let orch_clone = orchestrator.clone();
    let store_clone = store.clone();
    let mut rx = bus.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match orch_clone.process_event(event.clone()).await {
                Ok(outcome) if outcome.passed_attention => {
                    let embedding = vec![0.0f32; dim];
                    let segment = animus_core::Segment::new(
                        animus_core::segment::Content::Structured(event.data.clone()),
                        embedding,
                        animus_core::segment::Source::Observation {
                            event_type: format!("{:?}", event.event_type),
                            raw_event_id: event.id,
                        },
                    );
                    let _ = store_clone.store(segment);
                }
                _ => {}
            }
        }
    });

    // Start file watcher
    let mut watcher = FileWatcher::new(bus.clone(), vec![watch_dir.clone()]).unwrap();
    watcher.start().unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Create a file — this should trigger the full pipeline
    std::fs::write(watch_dir.join("important.rs"), "fn main() {}").unwrap();

    // Wait for the pipeline to process
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    watcher.stop();

    // Verify: audit trail has entries
    let entries = animus_sensorium::audit::AuditTrail::read_all(&audit_path).unwrap();
    assert!(!entries.is_empty(), "audit trail should have entries");

    // Verify: VectorFS has observation segments
    let segment_count = store.count(None);
    assert!(segment_count > 0, "VectorFS should have observation segments");
}
