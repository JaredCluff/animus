use animus_core::sensorium::{EventType, SensorEvent};
use animus_core::EventId;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

use crate::bus::EventBus;

/// Polls the system clipboard for changes and publishes SensorEvents.
///
/// Uses `arboard` for cross-platform clipboard access. On macOS this reads
/// NSPasteboard, on Linux it reads X11/Wayland clipboard.
///
/// Privacy: emits content type, text length, and a preview (first 200 chars).
/// The consent engine handles anonymization/filtering.
pub struct ClipboardMonitor {
    bus: Arc<EventBus>,
    poll_interval: Duration,
    shutdown: Arc<Notify>,
}

impl ClipboardMonitor {
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
            // arboard::Clipboard must be created on each poll (not Send/Sync on some platforms)
            let mut last_hash: u64 = 0;

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        if let Some(event_data) = Self::poll_clipboard(&mut last_hash) {
                            let event = SensorEvent {
                                id: EventId::new(),
                                timestamp: chrono::Utc::now(),
                                event_type: EventType::Clipboard,
                                source: "clipboard-monitor".to_string(),
                                data: event_data,
                                consent_policy: None,
                            };
                            if let Err(e) = bus.publish(event).await {
                                tracing::warn!("Failed to publish clipboard event: {e}");
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

    /// Check clipboard for changes. Returns event data if changed.
    fn poll_clipboard(last_hash: &mut u64) -> Option<serde_json::Value> {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Clipboard access failed: {e}");
                return None;
            }
        };

        // Try to get text content
        match clipboard.get_text() {
            Ok(text) if !text.is_empty() => {
                let hash = Self::hash_content(&text);
                if hash == *last_hash {
                    return None; // no change
                }
                *last_hash = hash;

                let preview: String = text.chars().take(200).collect();
                let truncated = text.len() > 200;

                Some(serde_json::json!({
                    "content_type": "text",
                    "length": text.len(),
                    "preview": preview,
                    "truncated": truncated,
                }))
            }
            Ok(_) => None, // empty clipboard
            Err(_) => {
                // Might be non-text content (image, etc.)
                // Just note that clipboard has non-text content
                let hash = Self::hash_content("__non_text__");
                if hash == *last_hash {
                    return None;
                }
                *last_hash = hash;

                Some(serde_json::json!({
                    "content_type": "non-text",
                    "length": 0,
                }))
            }
        }
    }

    fn hash_content(content: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }

    pub fn stop(&self) {
        self.shutdown.notify_one();
    }
}
