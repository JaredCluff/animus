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
            let reply_prefix = self.config.reply_prefix.clone();

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

                    let inbound_subject = msg.subject.to_string();

                    // Compute reply subject: animus.in.X → animus.out.X
                    // Replace the first path segment up to the leaf with reply_prefix.
                    // e.g. "animus.in.claude" with reply_prefix "animus.out" → "animus.out.claude"
                    let reply_subject = if let Some(leaf) = inbound_subject
                        .split('.')
                        .collect::<Vec<_>>()
                        .last()
                        .copied()
                    {
                        format!("{}.{}", reply_prefix, leaf)
                    } else {
                        format!("{}.reply", reply_prefix)
                    };

                    // thread_id is the outbound subject — prevents Animus replying back into
                    // its own inbound subscription and creating a loop.
                    let sender = SenderIdentity {
                        name: "nats".to_string(),
                        channel_user_id: inbound_subject.clone(),
                        is_trusted: true,
                    };

                    let mut channel_msg = ChannelMessage::new(
                        CHANNEL_ID,
                        reply_subject.clone(),
                        sender,
                        Some(payload),
                    );

                    // Preserve original subject and request/reply inbox
                    channel_msg.metadata = serde_json::json!({
                        "nats_subject": inbound_subject,
                        "nats_reply_to": msg.reply.as_deref().unwrap_or(""),
                        "nats_reply_subject": reply_subject,
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
        // Prefer NATS request/reply inbox over thread_id.
        // thread_id is already set to the outbound subject (animus.out.X).
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
