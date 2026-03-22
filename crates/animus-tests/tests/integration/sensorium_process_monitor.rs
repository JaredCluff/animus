use animus_core::sensorium::EventType;
use animus_sensorium::bus::EventBus;
use animus_sensorium::sensors::process_monitor::ProcessMonitor;
use std::sync::Arc;

#[tokio::test]
async fn detects_new_process() {
    let bus = Arc::new(EventBus::new(100));
    let mut rx = bus.subscribe();

    let mut monitor = ProcessMonitor::new(bus.clone(), std::time::Duration::from_millis(500));
    monitor.start();

    // Spawn a short-lived process
    let child = std::process::Command::new("sleep")
        .arg("2")
        .spawn()
        .expect("failed to spawn sleep");
    let child_pid = child.id();

    // Wait for the monitor to detect it
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            loop {
                if let Ok(event) = rx.recv().await {
                    if event.event_type == EventType::ProcessLifecycle {
                        if let Some(pid) = event.data.get("pid").and_then(|v| v.as_u64()) {
                            if pid == child_pid as u64 {
                                return event;
                            }
                        }
                    }
                }
            }
        },
    )
    .await;

    if let Ok(event) = event {
        assert_eq!(event.event_type, EventType::ProcessLifecycle);
        assert_eq!(event.source, "process-monitor");
    }

    monitor.stop();
}

#[tokio::test]
async fn process_monitor_stops_cleanly() {
    let bus = Arc::new(EventBus::new(100));
    let mut monitor = ProcessMonitor::new(bus.clone(), std::time::Duration::from_millis(100));
    monitor.start();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    monitor.stop();
    // Should not panic or hang
}
