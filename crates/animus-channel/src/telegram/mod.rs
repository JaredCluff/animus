//! Telegram channel adapter.
//!
//! Implements long-polling for incoming messages (text, photos, documents)
//! and sends responses back. Uses the raw Telegram Bot API via reqwest.

pub mod api;

use crate::bus::ChannelBus;
use crate::message::{ChannelMessage, OutboundMessage, SenderIdentity};
use crate::plugin::ChannelPlugin;
use animus_core::config::TelegramChannelConfig;
use animus_core::error::{AnimusError, Result};
use api::TelegramClient;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub const CHANNEL_ID: &str = "telegram";

/// Telegram channel adapter.
///
/// Polls Telegram for new messages and publishes them to the ChannelBus.
/// Sends responses back through the Telegram Bot API.
pub struct TelegramChannel {
    config: TelegramChannelConfig,
    client: TelegramClient,
    /// Trusted Telegram user IDs (bypass injection scanner).
    trusted_ids: Vec<i64>,
    /// Last seen update_id for polling offset.
    last_update_id: Arc<Mutex<Option<i64>>>,
}

impl TelegramChannel {
    pub fn new(config: TelegramChannelConfig, trusted_ids: Vec<i64>) -> Result<Self> {
        let client = TelegramClient::new(&config.bot_token)?;
        Ok(Self {
            config,
            client,
            trusted_ids,
            last_update_id: Arc::new(Mutex::new(None)),
        })
    }

    /// Determine if a Telegram user_id is trusted.
    fn is_trusted(&self, user_id: i64) -> bool {
        self.trusted_ids.contains(&user_id)
    }

    /// Convert a Telegram message to a ChannelMessage.
    async fn convert_message(&self, msg: &api::Message) -> Option<ChannelMessage> {
        let user = msg.from.as_ref()?;
        let user_id = user.id;

        let sender = SenderIdentity {
            name: user.display_name(),
            channel_user_id: user_id.to_string(),
            is_trusted: self.is_trusted(user_id),
        };

        // Combine text and caption
        let text = msg
            .text
            .clone()
            .or_else(|| msg.caption.clone());

        let mut channel_msg = ChannelMessage::new(
            CHANNEL_ID,
            msg.chat.id.to_string(),
            sender,
            text,
        );

        // Set message metadata for reply threading
        channel_msg.metadata = serde_json::json!({
            "telegram_message_id": msg.message_id,
            "telegram_chat_id": msg.chat.id,
        });

        // Download photos
        if let Some(photos) = &msg.photo {
            let download_dir = PathBuf::from(&self.config.download_dir);
            let paths = self
                .client
                .download_photos(photos, &download_dir, msg.message_id)
                .await;
            channel_msg.images = paths;
        }

        Some(channel_msg)
    }
}

#[async_trait::async_trait]
impl ChannelPlugin for TelegramChannel {
    fn id(&self) -> &str {
        CHANNEL_ID
    }

    fn name(&self) -> &str {
        "Telegram"
    }

    fn is_configured(&self) -> bool {
        !self.config.bot_token.is_empty()
    }

    async fn start(&self, bus: Arc<ChannelBus>) -> Result<()> {
        if !self.is_configured() {
            tracing::warn!("Telegram adapter: no bot token configured, skipping");
            return Ok(());
        }

        let client = self.client.clone();
        let last_update_id = self.last_update_id.clone();
        let poll_timeout = self.config.poll_timeout_secs;
        let download_dir = PathBuf::from(&self.config.download_dir);
        let trusted_ids = self.trusted_ids.clone();

        tokio::spawn(async move {
            tracing::info!("Telegram adapter: polling started");
            let mut poll_count: u64 = 0;
            loop {
                poll_count += 1;
                // Log a heartbeat every 20 polls (~10 min at 30s timeout) so silence is detectable
                if poll_count % 20 == 0 {
                    tracing::info!(poll_count, "Telegram adapter: polling heartbeat");
                }

                let offset = {
                    let guard = last_update_id.lock().await;
                    guard.map(|id| id + 1)
                };

                match client.get_updates(offset, poll_timeout).await {
                    Ok(updates) => {
                        for update in updates {
                            // Track the latest update_id
                            {
                                let mut guard = last_update_id.lock().await;
                                *guard = Some(update.update_id);
                            }

                            let Some(tg_msg) = update.message else {
                                continue;
                            };

                            let user_id = tg_msg.from.as_ref().map(|u| u.id).unwrap_or(0);
                            let is_trusted = trusted_ids.contains(&user_id);
                            let user_name = tg_msg
                                .from
                                .as_ref()
                                .map(|u| u.display_name())
                                .unwrap_or_else(|| "unknown".to_string());

                            let text = tg_msg.text.clone().or_else(|| tg_msg.caption.clone());

                            let sender = SenderIdentity {
                                name: user_name,
                                channel_user_id: user_id.to_string(),
                                is_trusted,
                            };

                            let mut channel_msg = ChannelMessage::new(
                                CHANNEL_ID,
                                tg_msg.chat.id.to_string(),
                                sender,
                                text,
                            );

                            channel_msg.metadata = serde_json::json!({
                                "telegram_message_id": tg_msg.message_id,
                                "telegram_chat_id": tg_msg.chat.id,
                            });

                            // Download photos if present
                            if let Some(photos) = &tg_msg.photo {
                                let paths = client
                                    .download_photos(photos, &download_dir, tg_msg.message_id)
                                    .await;
                                channel_msg.images = paths;
                            }

                            // Download voice message if present
                            if let Some(voice) = &tg_msg.voice {
                                if let Some(path) = client
                                    .download_voice(voice, &download_dir, tg_msg.message_id)
                                    .await
                                {
                                    channel_msg.attachments.push(path);
                                    if let Some(obj) = channel_msg.metadata.as_object_mut() {
                                        obj.insert("is_voice".to_string(), serde_json::json!(true));
                                    }
                                }
                            }

                            tracing::info!("Telegram: {}", channel_msg.summary());
                            bus.publish(channel_msg);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Telegram polling error: {e}");
                        // Back off briefly on error before retrying
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<()> {
        // Parse thread_id (which is the Telegram chat_id) as i64
        let chat_id: i64 = msg.thread_id.parse().map_err(|_| {
            AnimusError::Llm(format!("invalid Telegram chat_id: {}", msg.thread_id))
        })?;

        // Extract optional reply_to_message_id from metadata
        let reply_to = msg.metadata["telegram_message_id"].as_i64();

        if let Some(audio_path) = msg.audio {
            // OGG Opus → sendVoice (inline voice player in Telegram).
            // Any other format (AIFF from macOS `say`, etc.) → sendAudio (music player).
            let is_ogg = audio_path.extension().and_then(|e| e.to_str()) == Some("ogg");
            if is_ogg {
                self.client.send_voice(chat_id, &audio_path, reply_to).await?;
            } else {
                self.client.send_audio(chat_id, &audio_path, reply_to).await?;
            }
            // File has been fully read and uploaded; clean up temp TTS file.
            // Cleanup happens here (not in the runtime) to avoid a race where the
            // runtime deletes the file before this task has finished reading it.
            let _ = tokio::fs::remove_file(&audio_path).await;
        } else if let Some(image_path) = msg.image {
            // Send photo with caption (truncated to 1024 chars for Telegram caption limit)
            let caption = if msg.text.len() > 1024 {
                Some(&msg.text[..1024])
            } else if msg.text.is_empty() {
                None
            } else {
                Some(msg.text.as_str())
            };
            self.client.send_photo(chat_id, &image_path, caption).await?;
            // Send remaining text as a separate message if caption was truncated
            if msg.text.len() > 1024 {
                self.client
                    .send_message(chat_id, &msg.text[1024..], reply_to)
                    .await?;
            }
        } else {
            self.client.send_message(chat_id, &msg.text, reply_to).await?;
        }

        Ok(())
    }
}
