use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::Networks;
use tokio::sync::Notify;

use crate::bus::EventBus;

/// Polls network interface statistics and publishes events on significant changes.
pub struct NetworkMonitor {
    bus: Arc<EventBus>,
    poll_interval: Duration,
    shutdown: Arc<Notify>,
}

impl NetworkMonitor {
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
            let mut networks = Networks::new_with_refreshed_list();
            let mut prev_stats: HashMap<String, (u64, u64)> = snapshot(&networks);

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        networks.refresh(true);
                        let current_stats = snapshot(&networks);

                        // Detect new interfaces
                        for (name, &(rx, tx)) in &current_stats {
                            if !prev_stats.contains_key(name) {
                                let event = SensorEvent {
                                    id: EventId::new(),
                                    timestamp: chrono::Utc::now(),
                                    event_type: EventType::Network,
                                    source: "network-monitor".to_string(),
                                    data: serde_json::json!({
                                        "interface": name,
                                        "op": "new",
                                        "rx_bytes": rx,
                                        "tx_bytes": tx,
                                    }),
                                    consent_policy: None,
                                };
                                if let Err(e) = bus.publish(event).await {
                                    tracing::warn!("Failed to publish network event: {e}");
                                }
                            }
                        }

                        // Detect removed interfaces
                        for name in prev_stats.keys() {
                            if !current_stats.contains_key(name) {
                                let event = SensorEvent {
                                    id: EventId::new(),
                                    timestamp: chrono::Utc::now(),
                                    event_type: EventType::Network,
                                    source: "network-monitor".to_string(),
                                    data: serde_json::json!({
                                        "interface": name,
                                        "op": "removed",
                                    }),
                                    consent_policy: None,
                                };
                                if let Err(e) = bus.publish(event).await {
                                    tracing::warn!("Failed to publish network event: {e}");
                                }
                            }
                        }

                        // Report significant traffic changes (>1 MiB delta)
                        for (name, &(rx, tx)) in &current_stats {
                            if let Some(&(prev_rx, prev_tx)) = prev_stats.get(name) {
                                let rx_delta = rx.saturating_sub(prev_rx);
                                let tx_delta = tx.saturating_sub(prev_tx);
                                if rx_delta > 1_048_576 || tx_delta > 1_048_576 {
                                    let event = SensorEvent {
                                        id: EventId::new(),
                                        timestamp: chrono::Utc::now(),
                                        event_type: EventType::Network,
                                        source: "network-monitor".to_string(),
                                        data: serde_json::json!({
                                            "interface": name,
                                            "op": "traffic",
                                            "rx_delta_bytes": rx_delta,
                                            "tx_delta_bytes": tx_delta,
                                            "rx_total_bytes": rx,
                                            "tx_total_bytes": tx,
                                        }),
                                        consent_policy: None,
                                    };
                                    if let Err(e) = bus.publish(event).await {
                                        tracing::warn!("Failed to publish network event: {e}");
                                    }
                                }
                            }
                        }

                        prev_stats = current_stats;
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

fn snapshot(networks: &Networks) -> HashMap<String, (u64, u64)> {
    networks
        .iter()
        .map(|(name, data)| {
            (
                name.to_string(),
                (data.total_received(), data.total_transmitted()),
            )
        })
        .collect()
}
