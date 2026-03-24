//! NATS channel adapter.
//!
//! Subscribes to configured NATS subjects for inbound messages and publishes
//! outbound responses back to NATS. Supports both direct subject replies and
//! NATS request/reply (reply_to header).

use crate::bus::ChannelBus;
use crate::message::{ChannelMessage, OutboundMessage, SenderIdentity};
use crate::plugin::ChannelPlugin;
use animus_core::config::NatsChannelConfig;
use animus_core::error::{AnimusError, Result};
use async_nats::Client;
use futures::StreamExt;
use std::sync::Arc;

pub const CHANNEL_ID: &str = "nats";

/// NATS channel adapter.
///
/// Subscribes to one or more NATS subjects and publishes inbound messages to
/// the ChannelBus. Outbound responses are published back to the subject stored
/// in `thread_id`, or to `metadata["nats_reply_to"]` if set (request/reply).
pub struct NatsChannel {
    config: NatsChannelConfig,
    client: Client,
}

impl NatsChannel {
    /// Connect to NATS and return a configured adapter.
    pub async fn connect(config: NatsChannelConfig) -> Result<Self> {
        let client = async_nats::connect(&config.url)
            .await
            .map_err(|e| AnimusError::Llm(format!("NATS connect failed ({}): {e}", config.url)))?;
        tracing::info!("NATS channel: connected to {}", config.url);
        Ok(Self { config, client })
    }
}

#[async_trait::async_trait]
impl ChannelPlugin for NatsChannel {
    fn id(&self) -> &str {
        CHANNEL_ID
    }

    fn name(&self) -> &str {
        "NATS"
    }

    fn is_configured(&self) -> bool {
        self.config.enabled && !self.config.url.is_empty()
    }

    async fn start(&self, bus: Arc<ChannelBus>) -> Result<()> {
        if self.config.subjects.is_empty() {
            tracing::warn!("NATS adapter: no subjects configured, skipping subscriptions");
            return Ok(());
        }

        for subject in &self.config.subjects {
            let client = self.client.clone();
            let bus = bus.clone();
            let subject = subject.clone();

            let mut sub = client
                .subscribe(subject.clone())
                .await
                .map_err(|e| AnimusError::Llm(format!("NATS subscribe failed ({subject}): {e}")))?;

            tokio::spawn(async move {
                tracing::info!("NATS adapter: subscribed to '{subject}'");
                while let Some(msg) = sub.next().await {
                    let payload = match std::str::from_utf8(&msg.payload) {
                        Ok(s) => s.to_string(),
                        Err(_) => {
                            tracing::warn!("NATS: non-UTF8 payload on '{}', skipping", msg.subject);
                            continue;
                        }
                    };

                    // Use subject as thread_id so replies route back correctly
                    let thread_id = msg.subject.to_string();

                    let sender = SenderIdentity {
                        name: "nats".to_string(),
                        channel_user_id: msg.subject.to_string(),
                        is_trusted: true,
                    };

                    let mut channel_msg = ChannelMessage::new(
                        CHANNEL_ID,
                        thread_id,
                        sender,
                        Some(payload),
                    );

                    // Preserve reply_to so outbound send can use it for request/reply
                    channel_msg.metadata = serde_json::json!({
                        "nats_subject": msg.subject.as_str(),
                        "nats_reply_to": msg.reply.as_deref().unwrap_or(""),
                    });

                    tracing::debug!("NATS: {}", channel_msg.summary());
                    bus.publish(channel_msg);
                }
                tracing::warn!("NATS subscription ended for '{subject}'");
            });
        }

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<()> {
        // Prefer explicit reply_to (request/reply pattern) over thread_id
        let target = msg
            .metadata
            .get("nats_reply_to")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&msg.thread_id);

        self.client
            .publish(target.to_string(), msg.text.into())
            .await
            .map_err(|e| AnimusError::Llm(format!("NATS publish failed ({target}): {e}")))?;

        Ok(())
    }
}
