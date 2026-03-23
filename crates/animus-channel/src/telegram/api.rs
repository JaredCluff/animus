//! Minimal Telegram Bot API client using raw reqwest.
//! Only implements the subset needed: getUpdates, sendMessage, sendPhoto, getFile, download.

use animus_core::error::{AnimusError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

/// A Telegram Bot API client.
#[derive(Clone)]
pub struct TelegramClient {
    bot_token: String,
    http: reqwest::Client,
}

// ---------------------------------------------------------------------------
// Telegram API response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    pub from: Option<User>,
    pub text: Option<String>,
    pub photo: Option<Vec<PhotoSize>>,
    pub document: Option<Document>,
    pub caption: Option<String>,
    pub date: i64,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: String,
    pub last_name: Option<String>,
}

impl User {
    pub fn display_name(&self) -> String {
        let mut name = self.first_name.clone();
        if let Some(last) = &self.last_name {
            name.push(' ');
            name.push_str(last);
        }
        name
    }
}

#[derive(Debug, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: i32,
    pub height: i32,
    pub file_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct Document {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct TgFile {
    pub file_id: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct GetUpdatesParams {
    offset: Option<i64>,
    timeout: u64,
    allowed_updates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageParams<'a> {
    chat_id: i64,
    text: &'a str,
    parse_mode: Option<&'a str>,
    reply_to_message_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Client implementation
// ---------------------------------------------------------------------------

impl TelegramClient {
    pub fn new(bot_token: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| AnimusError::Llm(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            bot_token: bot_token.into(),
            http,
        })
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", TELEGRAM_API_BASE, self.bot_token, method)
    }

    /// Long-poll for new updates. Returns an empty vec on timeout (normal).
    pub async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_secs: u64,
    ) -> Result<Vec<Update>> {
        let params = GetUpdatesParams {
            offset,
            timeout: timeout_secs,
            allowed_updates: vec!["message".to_string()],
        };

        let resp = self
            .http
            .post(self.url("getUpdates"))
            .json(&params)
            .timeout(std::time::Duration::from_secs(timeout_secs + 5))
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("getUpdates request failed: {e}")))?;

        let api_resp: ApiResponse<Vec<Update>> = resp
            .json()
            .await
            .map_err(|e| AnimusError::Llm(format!("getUpdates parse failed: {e}")))?;

        if !api_resp.ok {
            return Err(AnimusError::Llm(format!(
                "getUpdates error: {}",
                api_resp.description.unwrap_or_default()
            )));
        }

        Ok(api_resp.result.unwrap_or_default())
    }

    /// Send a text message to a chat.
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i64>,
    ) -> Result<()> {
        // Telegram has a 4096 character limit; split if needed
        let chunks = split_message(text, 4000);
        for chunk in chunks {
            let params = SendMessageParams {
                chat_id,
                text: &chunk,
                parse_mode: Some("Markdown"),
                reply_to_message_id: reply_to,
            };
            let resp = self
                .http
                .post(self.url("sendMessage"))
                .json(&params)
                .send()
                .await
                .map_err(|e| AnimusError::Llm(format!("sendMessage request failed: {e}")))?;

            let api_resp: ApiResponse<serde_json::Value> = resp
                .json()
                .await
                .map_err(|e| AnimusError::Llm(format!("sendMessage parse failed: {e}")))?;

            if !api_resp.ok {
                // Try plain text if Markdown failed
                let plain = SendMessageParams {
                    chat_id,
                    text: &chunk,
                    parse_mode: None,
                    reply_to_message_id: reply_to,
                };
                let _ = self
                    .http
                    .post(self.url("sendMessage"))
                    .json(&plain)
                    .send()
                    .await;
            }
        }
        Ok(())
    }

    /// Send a photo file to a chat.
    pub async fn send_photo(&self, chat_id: i64, path: &Path, caption: Option<&str>) -> Result<()> {
        let bytes = std::fs::read(path)
            .map_err(|e| AnimusError::Llm(format!("failed to read photo file: {e}")))?;

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.jpg")
            .to_string();

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str("image/jpeg")
            .map_err(|e| AnimusError::Llm(format!("MIME error: {e}")))?;

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("photo", part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        self.http
            .post(self.url("sendPhoto"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("sendPhoto request failed: {e}")))?;

        Ok(())
    }

    /// Get a file's download path from its file_id.
    pub async fn get_file(&self, file_id: &str) -> Result<TgFile> {
        let resp = self
            .http
            .post(self.url("getFile"))
            .json(&serde_json::json!({"file_id": file_id}))
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("getFile request failed: {e}")))?;

        let api_resp: ApiResponse<TgFile> = resp
            .json()
            .await
            .map_err(|e| AnimusError::Llm(format!("getFile parse failed: {e}")))?;

        api_resp
            .result
            .ok_or_else(|| AnimusError::Llm("getFile: no result".to_string()))
    }

    /// Download a Telegram file to a local path.
    pub async fn download_file(&self, file_path: &str, dest: &Path) -> Result<()> {
        let url = format!(
            "{}/file/bot{}/{}",
            TELEGRAM_API_BASE, self.bot_token, file_path
        );

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("download request failed: {e}")))?;

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AnimusError::Llm(format!("download read failed: {e}")))?;

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AnimusError::Llm(format!("failed to create download dir: {e}")))?;
        }

        std::fs::write(dest, &bytes)
            .map_err(|e| AnimusError::Llm(format!("failed to write downloaded file: {e}")))?;

        Ok(())
    }

    /// Download all photos from a message, return local paths.
    pub async fn download_photos(
        &self,
        photos: &[PhotoSize],
        download_dir: &Path,
        message_id: i64,
    ) -> Vec<PathBuf> {
        // Pick the largest photo size
        let largest = photos.iter().max_by_key(|p| p.file_size.unwrap_or(0));
        let Some(photo) = largest else {
            return Vec::new();
        };

        let tg_file = match self.get_file(&photo.file_id).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("Failed to get file info for photo: {e}");
                return Vec::new();
            }
        };

        let Some(file_path) = tg_file.file_path else {
            return Vec::new();
        };

        let ext = file_path.rsplit('.').next().unwrap_or("jpg");
        let dest = download_dir.join(format!("tg_{message_id}.{ext}"));

        match self.download_file(&file_path, &dest).await {
            Ok(()) => {
                tracing::debug!("Downloaded photo to {}", dest.display());
                vec![dest]
            }
            Err(e) => {
                tracing::warn!("Failed to download photo: {e}");
                Vec::new()
            }
        }
    }
}

/// Split a long message into chunks at word boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.len() + word.len() + 1 > max_len && !current.is_empty() {
            chunks.push(current.trim().to_string());
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }

    if !current.is_empty() {
        chunks.push(current.trim().to_string());
    }

    chunks
}
