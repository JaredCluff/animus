use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use animus_sensorium::bus::EventBus;
use std::sync::Arc;

#[tokio::test]
async fn event_bus_send_and_receive() {
    let bus = EventBus::new(100);
    let mut rx = bus.subscribe();

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::FileChange,
        source: "test".to_string(),
        data: serde_json::json!({"path": "/tmp/test.txt"}),
        consent_policy: None,
    };

    bus.publish(event.clone()).await.unwrap();
    let received = rx.recv().await.unwrap();
    assert_eq!(received.id, event.id);
    assert_eq!(received.event_type, EventType::FileChange);
}

#[tokio::test]
async fn event_bus_multiple_subscribers() {
    let bus = EventBus::new(100);
    let mut rx1 = bus.subscribe();
    let mut rx2 = bus.subscribe();

    let event = SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type: EventType::ProcessLifecycle,
        source: "test".to_string(),
        data: serde_json::json!({"pid": 123}),
        consent_policy: None,
    };

    bus.publish(event.clone()).await.unwrap();

    let r1 = rx1.recv().await.unwrap();
    let r2 = rx2.recv().await.unwrap();
    assert_eq!(r1.id, event.id);
    assert_eq!(r2.id, event.id);
}

#[tokio::test]
async fn event_bus_shutdown() {
    let bus = Arc::new(EventBus::new(100));
    let mut rx = bus.subscribe();
    drop(bus); // dropping the sender closes the channel
    // After shutdown, recv should return an error (closed)
    assert!(rx.recv().await.is_err());
}
