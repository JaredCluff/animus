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
    pub voice: Option<Voice>,
    pub caption: Option<String>,
    pub date: i64,
}

#[derive(Debug, Deserialize)]
pub struct Voice {
    pub file_id: String,
    pub duration: u32,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
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
    /// Text is preprocessed from Markdown to Telegram HTML before sending.
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i64>,
    ) -> Result<()> {
        let html = md_to_telegram_html(text);
        // Telegram has a 4096 character limit; split if needed
        let chunks = split_message(&html, 4000);
        for chunk in chunks {
            let params = SendMessageParams {
                chat_id,
                text: &chunk,
                parse_mode: Some("HTML"),
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
                // Try plain text if HTML failed
                tracing::warn!(
                    chat_id,
                    reason = ?api_resp.description,
                    "Telegram: HTML send failed, falling back to plain text"
                );
                let plain = SendMessageParams {
                    chat_id,
                    text: &chunk,
                    parse_mode: None,
                    reply_to_message_id: reply_to,
                };
                let plain_resp = self
                    .http
                    .post(self.url("sendMessage"))
                    .json(&plain)
                    .send()
                    .await
                    .map_err(|e| AnimusError::Llm(format!("sendMessage plain fallback request failed: {e}")))?;

                let plain_api_resp: ApiResponse<serde_json::Value> = plain_resp
                    .json()
                    .await
                    .map_err(|e| AnimusError::Llm(format!("sendMessage plain fallback parse failed: {e}")))?;

                if !plain_api_resp.ok {
                    return Err(AnimusError::Llm(format!(
                        "sendMessage failed (plain text): {}",
                        plain_api_resp.description.unwrap_or_default()
                    )));
                }
            }

            tracing::info!(chat_id, chars = chunk.len(), "Telegram: sent message");
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

    /// Send a voice message (OGG Opus or MP3) to a chat.
    pub async fn send_voice(&self, chat_id: i64, path: &Path, reply_to: Option<i64>) -> Result<()> {
        let bytes = std::fs::read(path)
            .map_err(|e| AnimusError::Llm(format!("failed to read voice file: {e}")))?;

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("voice.ogg")
            .to_string();

        let mime = if filename.ends_with(".mp3") { "audio/mpeg" } else { "audio/ogg" };

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| AnimusError::Llm(format!("MIME error: {e}")))?;

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("voice", part);

        if let Some(id) = reply_to {
            form = form.text("reply_to_message_id", id.to_string());
        }

        let resp = self
            .http
            .post(self.url("sendVoice"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("sendVoice request failed: {e}")))?;

        let api_resp: ApiResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| AnimusError::Llm(format!("sendVoice parse failed: {e}")))?;

        if !api_resp.ok {
            return Err(AnimusError::Llm(format!(
                "sendVoice failed: {}",
                api_resp.description.unwrap_or_default()
            )));
        }

        tracing::info!(chat_id, "Telegram: sent voice message");
        Ok(())
    }

    /// Download a voice message to a local file. Returns the local path.
    pub async fn download_voice(
        &self,
        voice: &Voice,
        download_dir: &Path,
        message_id: i64,
    ) -> Option<PathBuf> {
        let tg_file = match self.get_file(&voice.file_id).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("Failed to get voice file info: {e}");
                return None;
            }
        };

        let Some(file_path) = tg_file.file_path else {
            return None;
        };

        // Sanitize extension: only allow short alphanumeric extensions to prevent path traversal
        let raw_ext = file_path.rsplit('.').next().unwrap_or("ogg");
        let ext = if raw_ext.len() <= 6 && raw_ext.chars().all(|c| c.is_alphanumeric()) {
            raw_ext
        } else {
            "ogg"
        };
        let dest = download_dir.join(format!("tg_voice_{message_id}.{ext}"));

        match self.download_file(&file_path, &dest).await {
            Ok(()) => {
                tracing::debug!("Downloaded voice to {}", dest.display());
                Some(dest)
            }
            Err(e) => {
                tracing::warn!("Failed to download voice: {e}");
                None
            }
        }
    }

    /// Send an audio file (non-voice format: AIFF, MP3, etc.) to a chat.
    /// Telegram displays this as a music-player attachment, not an inline voice player.
    /// Use `send_voice` for OGG Opus files that should play inline.
    pub async fn send_audio(&self, chat_id: i64, path: &Path, reply_to: Option<i64>) -> Result<()> {
        let bytes = std::fs::read(path)
            .map_err(|e| AnimusError::Llm(format!("failed to read audio file: {e}")))?;

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.aiff")
            .to_string();

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("aiff");
        let mime = match ext {
            "mp3" => "audio/mpeg",
            "m4a" => "audio/mp4",
            "wav" => "audio/wav",
            "flac" => "audio/flac",
            _ => "audio/x-aiff",
        };

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| AnimusError::Llm(format!("MIME error: {e}")))?;

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("audio", part);

        if let Some(id) = reply_to {
            form = form.text("reply_to_message_id", id.to_string());
        }

        let resp = self
            .http
            .post(self.url("sendAudio"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("sendAudio request failed: {e}")))?;

        let api_resp: ApiResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| AnimusError::Llm(format!("sendAudio parse failed: {e}")))?;

        if !api_resp.ok {
            return Err(AnimusError::Llm(format!(
                "sendAudio failed: {}",
                api_resp.description.unwrap_or_default()
            )));
        }

        tracing::info!(chat_id, "Telegram: sent audio file");
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

// ---------------------------------------------------------------------------
// Markdown → Telegram HTML conversion
// ---------------------------------------------------------------------------

/// Convert standard Markdown to Telegram-compatible HTML.
///
/// Handles the subset commonly produced by LLMs:
/// - Fenced code blocks (``` → `<pre><code>`)
/// - Inline code (`` `x` `` → `<code>x</code>`)
/// - Bold (`**x**` → `<b>x</b>`)
/// - Italic (`*x*` → `<i>x</i>`)
/// - Strikethrough (`~~x~~` → `<s>x</s>`)
/// - Headers (`# x` → `<b>x</b>`)
/// - Unordered bullets (`- x` / `* x` → `• x`)
/// - Links (`[text](url)` → `<a href="url">text</a>`)
/// - HTML entity escaping in plain text (`&`, `<`, `>`)
pub fn md_to_telegram_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len() * 5 / 4);
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Fenced code block: ``` (possibly with language tag)
        if line.trim_start().starts_with("```") {
            i += 1;
            let mut code_lines: Vec<&str> = Vec::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // skip closing ```
            }
            let code = code_lines.join("\n");
            output.push_str("<pre><code>");
            output.push_str(&html_escape_text(&code));
            output.push_str("</code></pre>\n");
            continue;
        }

        // Header: one or more leading # followed by space
        if line.starts_with('#') {
            let trimmed = line.trim_start_matches('#');
            if trimmed.starts_with(' ') {
                let content = trimmed.trim();
                output.push_str("<b>");
                output.push_str(&process_inline_spans(content));
                output.push_str("</b>\n");
                i += 1;
                continue;
            }
        }

        // Unordered bullet: - , + , or * at the start (but not ** bold)
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("+ ")) {
            output.push_str("• ");
            output.push_str(&process_inline_spans(rest));
            output.push('\n');
            i += 1;
            continue;
        }
        if line.starts_with("* ") && !line.starts_with("** ") {
            let rest = &line[2..];
            output.push_str("• ");
            output.push_str(&process_inline_spans(rest));
            output.push('\n');
            i += 1;
            continue;
        }

        // Regular line
        output.push_str(&process_inline_spans(line));
        output.push('\n');
        i += 1;
    }

    // Trim trailing newline added after the last line
    if output.ends_with('\n') {
        output.pop();
    }

    output
}

/// Process inline Markdown spans within a single line of text.
fn process_inline_spans(text: &str) -> String {
    let mut out = String::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Inline code: `...`
        if remaining.starts_with('`') {
            remaining = &remaining[1..];
            if let Some(end) = remaining.find('`') {
                out.push_str("<code>");
                out.push_str(&html_escape_text(&remaining[..end]));
                out.push_str("</code>");
                remaining = &remaining[end + 1..];
            } else {
                // Unmatched backtick — emit as-is
                out.push('`');
            }
            continue;
        }

        // Bold: **...**  (check before single *)
        if remaining.starts_with("**") {
            remaining = &remaining[2..];
            if let Some(end) = remaining.find("**") {
                out.push_str("<b>");
                out.push_str(&html_escape_text(&remaining[..end]));
                out.push_str("</b>");
                remaining = &remaining[end + 2..];
            } else {
                out.push_str("**");
            }
            continue;
        }

        // Italic: *...*
        if remaining.starts_with('*') {
            remaining = &remaining[1..];
            if let Some(end) = remaining.find('*') {
                out.push_str("<i>");
                out.push_str(&html_escape_text(&remaining[..end]));
                out.push_str("</i>");
                remaining = &remaining[end + 1..];
            } else {
                out.push('*');
            }
            continue;
        }

        // Strikethrough: ~~...~~
        if remaining.starts_with("~~") {
            remaining = &remaining[2..];
            if let Some(end) = remaining.find("~~") {
                out.push_str("<s>");
                out.push_str(&html_escape_text(&remaining[..end]));
                out.push_str("</s>");
                remaining = &remaining[end + 2..];
            } else {
                out.push_str("~~");
            }
            continue;
        }

        // Link: [text](url)
        if remaining.starts_with('[') {
            if let Some((link_text, url, rest)) = try_parse_link(remaining) {
                out.push_str("<a href=\"");
                out.push_str(&html_escape_text(&url));
                out.push_str("\">");
                out.push_str(&html_escape_text(&link_text));
                out.push_str("</a>");
                remaining = rest;
                continue;
            }
        }

        // Plain character with HTML entity escaping
        let c = remaining.chars().next().unwrap();
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
        remaining = &remaining[c.len_utf8()..];
    }

    out
}

/// Try to parse a Markdown link `[text](url)` from the start of `s`.
/// Returns `(link_text, url, remaining_str)` on success.
fn try_parse_link<'a>(s: &'a str) -> Option<(String, String, &'a str)> {
    let inner = s.strip_prefix('[')?;
    let close = inner.find(']')?;
    let link_text = inner[..close].to_string();
    let after_bracket = inner[close + 1..].strip_prefix('(')?;
    let close_paren = after_bracket.find(')')?;
    let url = after_bracket[..close_paren].to_string();
    Some((link_text, url, &after_bracket[close_paren + 1..]))
}

/// Escape `&`, `<`, and `>` for use in Telegram HTML text nodes.
fn html_escape_text(s: &str) -> String {
    // Most strings have no entities; avoid allocation in the common case.
    if !s.contains(['&', '<', '>']) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(md_to_telegram_html("Hello world"), "Hello world");
    }

    #[test]
    fn html_entities_escaped() {
        assert_eq!(
            md_to_telegram_html("a & b < c > d"),
            "a &amp; b &lt; c &gt; d"
        );
    }

    #[test]
    fn bold_converted() {
        assert_eq!(md_to_telegram_html("**bold**"), "<b>bold</b>");
    }

    #[test]
    fn italic_converted() {
        assert_eq!(md_to_telegram_html("*italic*"), "<i>italic</i>");
    }

    #[test]
    fn inline_code_converted() {
        assert_eq!(md_to_telegram_html("`code`"), "<code>code</code>");
    }

    #[test]
    fn inline_code_escapes_html_entities() {
        assert_eq!(
            md_to_telegram_html("`a < b`"),
            "<code>a &lt; b</code>"
        );
    }

    #[test]
    fn fenced_code_block_converted() {
        let input = "```\nfn main() {}\n```";
        let output = md_to_telegram_html(input);
        assert_eq!(output, "<pre><code>fn main() {}</code></pre>");
    }

    #[test]
    fn fenced_code_block_with_lang() {
        let input = "```rust\nlet x = 1;\n```";
        let output = md_to_telegram_html(input);
        assert_eq!(output, "<pre><code>let x = 1;</code></pre>");
    }

    #[test]
    fn header_converted() {
        assert_eq!(md_to_telegram_html("# Title"), "<b>Title</b>");
        assert_eq!(md_to_telegram_html("## Section"), "<b>Section</b>");
    }

    #[test]
    fn bullet_converted() {
        assert_eq!(md_to_telegram_html("- item"), "• item");
        assert_eq!(md_to_telegram_html("* item"), "• item");
    }

    #[test]
    fn strikethrough_converted() {
        assert_eq!(md_to_telegram_html("~~old~~"), "<s>old</s>");
    }

    #[test]
    fn link_converted() {
        assert_eq!(
            md_to_telegram_html("[click here](https://example.com)"),
            "<a href=\"https://example.com\">click here</a>"
        );
    }

    #[test]
    fn mixed_inline_in_paragraph() {
        let input = "Use **bold** and *italic* and `code` here.";
        let output = md_to_telegram_html(input);
        assert_eq!(output, "Use <b>bold</b> and <i>italic</i> and <code>code</code> here.");
    }

    #[test]
    fn multiline_message() {
        let input = "# Heading\n\n- item one\n- item two\n\nSome `code` here.";
        let output = md_to_telegram_html(input);
        assert!(output.contains("<b>Heading</b>"));
        assert!(output.contains("• item one"));
        assert!(output.contains("• item two"));
        assert!(output.contains("<code>code</code>"));
    }

    #[test]
    fn unmatched_marker_emitted_as_is() {
        // Unmatched bold marker should be emitted literally
        assert_eq!(md_to_telegram_html("**unclosed"), "**unclosed");
    }
}
