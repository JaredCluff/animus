use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::bus::EventBus;

/// Watches file system paths for changes and publishes SensorEvents.
pub struct FileWatcher {
    bus: Arc<EventBus>,
    paths: Vec<PathBuf>,
    shutdown: Arc<Notify>,
    _watcher: Option<RecommendedWatcher>,
}

impl FileWatcher {
    pub fn new(bus: Arc<EventBus>, paths: Vec<PathBuf>) -> animus_core::Result<Self> {
        Ok(Self {
            bus,
            paths,
            shutdown: Arc::new(Notify::new()),
            _watcher: None,
        })
    }

    pub fn start(&mut self) {
        let bus = self.bus.clone();
        let shutdown = self.shutdown.clone();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(256);

        let tx_clone = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx_clone.blocking_send(event);
            }
        })
        .expect("failed to create file watcher");

        for path in &self.paths {
            if let Err(e) = watcher.watch(path, RecursiveMode::Recursive) {
                tracing::warn!("Failed to watch {}: {e}", path.display());
            }
        }

        self._watcher = Some(watcher);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        let op = format!("{:?}", event.kind);
                        for path in &event.paths {
                            let sensor_event = SensorEvent {
                                id: EventId::new(),
                                timestamp: chrono::Utc::now(),
                                event_type: EventType::FileChange,
                                source: "file-watcher".to_string(),
                                data: serde_json::json!({
                                    "path": path.to_string_lossy(),
                                    "op": op,
                                }),
                                consent_policy: None,
                            };
                            if let Err(e) = bus.publish(sensor_event).await {
                                tracing::warn!("Failed to publish file event: {e}");
                            }
                        }
                    }
                    _ = shutdown.notified() => {
                        break;
                    }
                }
            }
        });
    }

    pub fn stop(&self) {
        self.shutdown.notify_one();
    }
}
