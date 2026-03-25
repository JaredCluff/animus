//! Core message types for the channel layer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Identity of the sender of a channel message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderIdentity {
    /// Display name (username, email address, etc.)
    pub name: String,
    /// Channel-specific user identifier (Telegram user_id, email address, etc.)
    pub channel_user_id: String,
    /// Whether this sender is on the trusted list (bypasses heavy injection scanning).
    pub is_trusted: bool,
}

/// Priority assigned to a message by the MessageRouter after triage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MessagePriority {
    /// Background tasks, digests, non-urgent monitoring.
    Low = 0,
    /// Normal conversational messages.
    Normal = 1,
    /// Time-sensitive or explicit urgency markers.
    High = 2,
    /// Calendar alarms, explicit SOS, or messages from trusted sources marked urgent.
    Critical = 3,
}

impl Default for MessagePriority {
    fn default() -> Self {
        MessagePriority::Normal
    }
}

/// A message arriving from any communication channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Unique message ID.
    pub id: Uuid,
    /// Channel identifier (e.g. "telegram", "email", "http_api").
    pub channel_id: String,
    /// Conversation thread identity (channel-specific, e.g. Telegram chat_id as string).
    pub thread_id: String,
    /// Who sent this message.
    pub sender: SenderIdentity,
    /// Text content (may be empty if message is image-only).
    pub text: Option<String>,
    /// Local paths to downloaded images/photos.
    pub images: Vec<PathBuf>,
    /// Local paths to other attachments.
    pub attachments: Vec<PathBuf>,
    /// When the message was received.
    pub timestamp: DateTime<Utc>,
    /// Priority assigned by MessageRouter (default Normal until triage runs).
    pub priority: MessagePriority,
    /// Raw channel-specific metadata (e.g. Telegram message_id for reply threading).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ChannelMessage {
    /// Create a new message with default priority.
    pub fn new(
        channel_id: impl Into<String>,
        thread_id: impl Into<String>,
        sender: SenderIdentity,
        text: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            channel_id: channel_id.into(),
            thread_id: thread_id.into(),
            sender,
            text,
            images: Vec::new(),
            attachments: Vec::new(),
            timestamp: Utc::now(),
            priority: MessagePriority::Normal,
            metadata: serde_json::Value::Null,
        }
    }

    /// Returns true if this message has any visual content.
    pub fn has_images(&self) -> bool {
        !self.images.is_empty()
    }

    /// Produce a plain-text summary for logging.
    pub fn summary(&self) -> String {
        let text_preview = self
            .text
            .as_deref()
            .map(|t| {
                if t.len() > 60 {
                    format!("{}…", &t[..60])
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_else(|| "(no text)".to_string());
        let image_note = if self.has_images() {
            format!(" [+{} image(s)]", self.images.len())
        } else {
            String::new()
        };
        format!(
            "[{}] {} via {}: {}{}",
            self.priority_label(),
            self.sender.name,
            self.channel_id,
            text_preview,
            image_note
        )
    }

    fn priority_label(&self) -> &'static str {
        match self.priority {
            MessagePriority::Low => "LOW",
            MessagePriority::Normal => "NORMAL",
            MessagePriority::High => "HIGH",
            MessagePriority::Critical => "CRITICAL",
        }
    }
}

/// A response to be sent back through a channel.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    /// Which channel to send through.
    pub channel_id: String,
    /// Thread to reply into (channel-specific, e.g. Telegram chat_id).
    pub thread_id: String,
    /// Text content.
    pub text: String,
    /// Optional image to attach.
    pub image: Option<PathBuf>,
    /// Optional audio file to send as a voice message (OGG/MP3).
    pub audio: Option<PathBuf>,
    /// Channel-specific routing metadata (e.g. Telegram reply_to_message_id).
    pub metadata: serde_json::Value,
}

impl OutboundMessage {
    pub fn text(channel_id: impl Into<String>, thread_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            thread_id: thread_id.into(),
            text: text.into(),
            image: None,
            audio: None,
            metadata: serde_json::Value::Null,
        }
    }
}
