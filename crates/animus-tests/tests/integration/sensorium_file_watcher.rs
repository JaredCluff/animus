use animus_core::sensorium::EventType;
use animus_sensorium::bus::EventBus;
use animus_sensorium::sensors::file_watcher::FileWatcher;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn detects_file_creation() {
    let dir = TempDir::new().unwrap();
    let bus = Arc::new(EventBus::new(100));
    let mut rx = bus.subscribe();

    let mut watcher = FileWatcher::new(bus.clone(), vec![dir.path().to_path_buf()]).unwrap();
    watcher.start();

    // Give watcher time to initialize
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Create a file
    std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

    // Wait for event (with timeout)
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        rx.recv(),
    )
    .await
    .expect("timeout waiting for event")
    .expect("channel closed");

    assert_eq!(event.event_type, EventType::FileChange);
    assert_eq!(event.source, "file-watcher");
    let path = event.data.get("path").unwrap().as_str().unwrap();
    assert!(path.contains("test.txt"));

    watcher.stop();
}

#[tokio::test]
async fn detects_file_modification() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("existing.txt");
    std::fs::write(&test_file, "original").unwrap();

    let bus = Arc::new(EventBus::new(100));
    let mut rx = bus.subscribe();

    let mut watcher = FileWatcher::new(bus.clone(), vec![dir.path().to_path_buf()]).unwrap();
    watcher.start();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Modify the file
    std::fs::write(&test_file, "modified").unwrap();

    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        rx.recv(),
    )
    .await
    .expect("timeout waiting for event")
    .expect("channel closed");

    assert_eq!(event.event_type, EventType::FileChange);

    watcher.stop();
}
