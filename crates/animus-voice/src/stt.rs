use animus_core::error::{AnimusError, Result};
use async_trait::async_trait;
use std::path::Path;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

#[async_trait]
pub(crate) trait SttProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn transcribe(&self, audio_path: &Path) -> Result<String>;
}

// ---------------------------------------------------------------------------
// Groq Whisper
// ---------------------------------------------------------------------------

pub(crate) struct GroqStt {
    api_key: String,
    http: reqwest::Client,
}

impl GroqStt {
    pub fn new(api_key: String, http: reqwest::Client) -> Self {
        Self { api_key, http }
    }
}

#[async_trait]
impl SttProvider for GroqStt {
    fn name(&self) -> &str {
        "groq-whisper"
    }

    async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let bytes = tokio::fs::read(audio_path)
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/groq: read audio: {e}")))?;

        // Groq identifies file type by extension. Telegram sends .oga which Groq rejects;
        // normalize to voice.ogg (Groq accepts ogg/opus/mp3/wav/etc, not .oga).
        let filename = {
            let ext = audio_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            match ext {
                "ogg" | "oga" | "opus" => "voice.ogg".to_string(),
                "mp3" | "mpeg" | "mpga" => "voice.mp3".to_string(),
                "wav" => "voice.wav".to_string(),
                "m4a" | "mp4" => "voice.m4a".to_string(),
                "flac" => "voice.flac".to_string(),
                "webm" => "voice.webm".to_string(),
                _ => "voice.ogg".to_string(),
            }
        };

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str("audio/ogg; codecs=opus")
            .map_err(|e| AnimusError::Voice(format!("stt/groq: mime: {e}")))?;

        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", "whisper-large-v3-turbo")
            .text("response_format", "json");

        let resp = self
            .http
            .post("https://api.groq.com/openai/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/groq: request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Voice(format!("stt/groq: error {status}: {body}")));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/groq: parse failed: {e}")))?;

        json["text"]
            .as_str()
            .ok_or_else(|| AnimusError::Voice(format!("stt/groq: unexpected response: {json}")))
            .map(|s| s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Deepgram
// ---------------------------------------------------------------------------

pub(crate) struct DeepgramStt {
    api_key: String,
    http: reqwest::Client,
}

impl DeepgramStt {
    pub fn new(api_key: String, http: reqwest::Client) -> Self {
        Self { api_key, http }
    }
}

#[async_trait]
impl SttProvider for DeepgramStt {
    fn name(&self) -> &str {
        "deepgram"
    }

    async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let bytes = tokio::fs::read(audio_path)
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/deepgram: read audio: {e}")))?;

        let content_type = match audio_path.extension().and_then(|e| e.to_str()) {
            Some("ogg") => "audio/ogg",
            Some("mp3") => "audio/mpeg",
            Some("wav") => "audio/wav",
            Some("m4a") => "audio/mp4",
            _ => "audio/ogg",
        };

        let resp = self
            .http
            .post("https://api.deepgram.com/v1/listen?model=nova-2&smart_format=true")
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", content_type)
            .body(bytes)
            .send()
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/deepgram: request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Voice(format!("stt/deepgram: error {status}: {body}")));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/deepgram: parse failed: {e}")))?;

        json["results"]["channels"][0]["alternatives"][0]["transcript"]
            .as_str()
            .ok_or_else(|| AnimusError::Voice(format!("stt/deepgram: unexpected response: {json}")))
            .map(|s| s.to_string())
    }
}

// ---------------------------------------------------------------------------
// macOS STT (existing HTTP wrapper service)
// ---------------------------------------------------------------------------

pub(crate) struct MacosStt {
    stt_url: String,
    stt_key: String,
    http: reqwest::Client,
}

impl MacosStt {
    pub fn new(stt_url: String, stt_key: String, http: reqwest::Client) -> Self {
        Self { stt_url, stt_key, http }
    }
}

#[async_trait]
impl SttProvider for MacosStt {
    fn name(&self) -> &str {
        "macos-stt"
    }

    async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let bytes = tokio::fs::read(audio_path)
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/macos: read audio: {e}")))?;

        let filename = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("voice.ogg")
            .to_string();

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str("application/octet-stream")
            .map_err(|e| AnimusError::Voice(format!("stt/macos: mime: {e}")))?;

        let form = reqwest::multipart::Form::new().part("audio", part);

        let resp = self
            .http
            .post(format!("{}/transcribe", self.stt_url))
            .header("Authorization", format!("Bearer {}", self.stt_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/macos: service unreachable: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Voice(format!("stt/macos: error {status}: {body}")));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AnimusError::Voice(format!("stt/macos: parse failed: {e}")))?;

        json["transcript"]
            .as_str()
            .ok_or_else(|| AnimusError::Voice(format!("stt/macos: unexpected response: {json}")))
            .map(|s| s.to_string())
    }
}
