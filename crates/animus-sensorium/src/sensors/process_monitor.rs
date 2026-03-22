use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::System;
use tokio::sync::Notify;

use crate::bus::EventBus;

/// Polls for process lifecycle changes and publishes SensorEvents.
pub struct ProcessMonitor {
    bus: Arc<EventBus>,
    poll_interval: Duration,
    shutdown: Arc<Notify>,
}

impl ProcessMonitor {
    pub fn new(bus: Arc<EventBus>, poll_interval: Duration) -> Self {
        Self {
            bus,
            poll_interval,
            shutdown: Arc::new(Notify::new()),
        }
    }

    pub fn start(&mut self) {
        let bus = self.bus.clone();
        let interval = self.poll_interval;
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            let mut sys = System::new();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            let mut known_pids: HashSet<sysinfo::Pid> = sys.processes().keys().copied().collect();

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                        let current_pids: HashSet<sysinfo::Pid> = sys.processes().keys().copied().collect();

                        // Detect new processes
                        for &pid in current_pids.difference(&known_pids) {
                            if let Some(process) = sys.process(pid) {
                                let event = SensorEvent {
                                    id: EventId::new(),
                                    timestamp: chrono::Utc::now(),
                                    event_type: EventType::ProcessLifecycle,
                                    source: "process-monitor".to_string(),
                                    data: serde_json::json!({
                                        "pid": pid.as_u32(),
                                        "name": process.name().to_string_lossy(),
                                        "op": "start",
                                    }),
                                    consent_policy: None,
                                };
                                if let Err(e) = bus.publish(event).await {
                                    tracing::warn!("Failed to publish process event: {e}");
                                }
                            }
                        }

                        // Detect terminated processes
                        for &pid in known_pids.difference(&current_pids) {
                            let event = SensorEvent {
                                id: EventId::new(),
                                timestamp: chrono::Utc::now(),
                                event_type: EventType::ProcessLifecycle,
                                source: "process-monitor".to_string(),
                                data: serde_json::json!({
                                    "pid": pid.as_u32(),
                                    "op": "stop",
                                }),
                                consent_policy: None,
                            };
                            if let Err(e) = bus.publish(event).await {
                                tracing::warn!("Failed to publish process stop event: {e}");
                            }
                        }

                        known_pids = current_pids;
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
