//! Telegram send tool — send a message to a Telegram chat.
//!
//! Allows Animus to proactively send messages (reminders, alerts, check-ins)
//! without waiting for the user to message first. Used in Goal-Directed and
//! Full autonomy modes for proactive outreach.

use super::{Tool, ToolContext, ToolResult};
use crate::telos::Autonomy;

pub struct TelegramSendTool;

#[async_trait::async_trait]
impl Tool for TelegramSendTool {
    fn name(&self) -> &str {
        "telegram_send"
    }

    fn description(&self) -> &str {
        "Send a Telegram message to a chat. Use for proactive outreach: reminders, \
        alerts, check-ins, or status updates. Requires ANIMUS_TELEGRAM_TOKEN to be set. \
        Use the stored chat_id from previous conversations with the user."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "integer",
                    "description": "Telegram chat ID to send to. Use the chat_id from the current conversation context."
                },
                "text": {
                    "type": "string",
                    "description": "Message text to send. Supports Markdown formatting."
                },
                "image_path": {
                    "type": "string",
                    "description": "Optional: absolute path to an image file to attach."
                }
            },
            "required": ["chat_id", "text"]
        })
    }

    fn required_autonomy(&self) -> Autonomy {
        Autonomy::Act
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        let chat_id = params["chat_id"]
            .as_i64()
            .ok_or("missing or invalid chat_id")?;
        let text = params["text"].as_str().ok_or("missing text parameter")?;
        let image_path = params["image_path"].as_str();

        let bot_token = std::env::var("ANIMUS_TELEGRAM_TOKEN")
            .map_err(|_| "ANIMUS_TELEGRAM_TOKEN not set — cannot send Telegram message")?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let base_url = format!("https://api.telegram.org/bot{bot_token}");

        if let Some(path) = image_path {
            // Send as photo
            let image_bytes = std::fs::read(path)
                .map_err(|e| format!("failed to read image: {e}"))?;

            let filename = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image.jpg")
                .to_string();

            let part = reqwest::multipart::Part::bytes(image_bytes)
                .file_name(filename)
                .mime_str("image/jpeg")
                .map_err(|e| format!("MIME error: {e}"))?;

            let caption_end = text.char_indices().nth(1024).map(|(i, _)| i).unwrap_or(text.len());
            let caption = &text[..caption_end];

            let form = reqwest::multipart::Form::new()
                .text("chat_id", chat_id.to_string())
                .text("caption", caption.to_string())
                .part("photo", part);

            client
                .post(format!("{base_url}/sendPhoto"))
                .multipart(form)
                .send()
                .await
                .map_err(|e| format!("sendPhoto failed: {e}"))?;
        } else {
            // Send as text, splitting if needed
            let chunks = split_message(text, 4000);
            for chunk in chunks {
                let body = serde_json::json!({
                    "chat_id": chat_id,
                    "text": chunk,
                    "parse_mode": "Markdown"
                });
                let resp = client
                    .post(format!("{base_url}/sendMessage"))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("sendMessage failed: {e}"))?;

                if !resp.status().is_success() {
                    // Retry without Markdown if formatting caused an error
                    let plain = serde_json::json!({
                        "chat_id": chat_id,
                        "text": chunk
                    });
                    let _ = client
                        .post(format!("{base_url}/sendMessage"))
                        .json(&plain)
                        .send()
                        .await;
                }
            }
        }

        Ok(ToolResult {
            content: format!("Message sent to Telegram chat {chat_id}"),
            is_error: false,
        })
    }
}

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
