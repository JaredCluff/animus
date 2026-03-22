use animus_sensorium::bus::EventBus;
use animus_sensorium::sensors::network_monitor::NetworkMonitor;
use std::sync::Arc;

#[tokio::test]
async fn network_monitor_stops_cleanly() {
    let bus = Arc::new(EventBus::new(100));
    let mut monitor = NetworkMonitor::new(bus.clone(), std::time::Duration::from_millis(100));
    monitor.start();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    monitor.stop();
    // Should not panic or hang
}

#[tokio::test]
async fn network_monitor_emits_no_crash_on_poll() {
    // Verifies the monitor can poll network interfaces without error.
    // Traffic events require >1 MiB delta per interval, so we just verify
    // the monitor runs without panicking on real system data.
    let bus = Arc::new(EventBus::new(100));
    let mut monitor = NetworkMonitor::new(bus.clone(), std::time::Duration::from_millis(200));
    monitor.start();

    // Let it poll at least twice
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    monitor.stop();
}
