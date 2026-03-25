//! Voice services for Animus.
//!
//! - **STT**: delegates to the `macos-stt` HTTP service (SFSpeechRecognizer on macOS).
//! - **TTS**: Cartesia neural TTS, returning OGG Opus for Telegram inline voice player.
//!
//! Both operations are handled by [`AnimusVoiceService`], which implements the
//! [`VoiceService`] trait. The runtime holds an `Option<Arc<dyn VoiceService>>`
//! and skips voice processing when `None`.

use animus_core::{config::VoiceConfig, error::{AnimusError, Result}};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstracts speech-to-text and text-to-speech for the Animus runtime.
#[async_trait]
pub trait VoiceService: Send + Sync {
    /// Transcribe an audio file to text by calling the macos-stt service.
    async fn transcribe(&self, audio_path: &Path) -> Result<String>;

    /// Synthesize text to an OGG Opus audio file via Cartesia.
    /// Returns the path to a temporary file. The caller (TelegramChannel) is
    /// responsible for deleting it after the upload completes.
    async fn synthesize(&self, text: &str) -> Result<PathBuf>;
}

// ---------------------------------------------------------------------------
// AnimusVoiceService
// ---------------------------------------------------------------------------

/// Unified voice service: STT via `macos-stt` HTTP + TTS via Cartesia.
pub struct AnimusVoiceService {
    /// Base URL of the macos-stt service, e.g. "http://127.0.0.1:7600".
    stt_url: String,
    /// Bearer key for the macos-stt service.
    stt_key: String,
    /// Cartesia API key.
    cartesia_api_key: String,
    /// Cartesia voice UUID.
    cartesia_voice_id: String,
    /// Cartesia model ID ("sonic-2").
    cartesia_model: String,
    http: reqwest::Client,
}

impl AnimusVoiceService {
    pub fn new(config: &VoiceConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| AnimusError::Llm(format!("voice: failed to build HTTP client: {e}")))?;
        Ok(Self {
            stt_url: config.stt_url.trim_end_matches('/').to_string(),
            stt_key: config.stt_key.clone(),
            cartesia_api_key: config.cartesia_api_key.clone(),
            cartesia_voice_id: config.cartesia_voice_id.clone(),
            cartesia_model: config.cartesia_model.clone(),
            http,
        })
    }
}

#[async_trait]
impl VoiceService for AnimusVoiceService {
    async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let bytes = tokio::fs::read(audio_path)
            .await
            .map_err(|e| AnimusError::Llm(format!("voice: failed to read audio file: {e}")))?;

        let filename = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("voice.ogg")
            .to_string();

        let part = reqwest::multipart::Part::bytes(bytes.to_vec())
            .file_name(filename.clone())
            .mime_str("application/octet-stream")
            .map_err(|e| AnimusError::Llm(format!("voice: MIME error: {e}")))?;

        let form = reqwest::multipart::Form::new().part("audio", part);

        let resp = self
            .http
            .post(format!("{}/transcribe", self.stt_url))
            .header("Authorization", format!("Bearer {}", self.stt_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("voice: STT service unreachable: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Llm(format!(
                "voice: STT service error {status}: {body}"
            )));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AnimusError::Llm(format!("voice: STT response parse failed: {e}")))?;

        json["transcript"]
            .as_str()
            .ok_or_else(|| AnimusError::Llm(format!("voice: unexpected STT response: {json}")))
            .map(|s| s.to_string())
    }

    async fn synthesize(&self, text: &str) -> Result<PathBuf> {
        if self.cartesia_api_key.is_empty() {
            return Err(AnimusError::Llm(
                "voice: Cartesia API key not configured (set ANIMUS_CARTESIA_KEY)".to_string(),
            ));
        }
        if self.cartesia_voice_id.is_empty() {
            return Err(AnimusError::Llm(
                "voice: Cartesia voice ID not configured (set voice.cartesia_voice_id in config)".to_string(),
            ));
        }

        // Cartesia does not support OGG output; request MP3 and convert via ffmpeg.
        let body = serde_json::json!({
            "model_id": self.cartesia_model,
            "transcript": text,
            "voice": {
                "mode": "id",
                "id": self.cartesia_voice_id,
            },
            "output_format": {
                "container": "mp3",
                "bit_rate": 128000,
                "sample_rate": 44100,
            },
        });

        let resp = self
            .http
            .post("https://api.cartesia.ai/tts/bytes")
            .header("X-API-Key", &self.cartesia_api_key)
            .header("Cartesia-Version", "2024-06-10")
            .json(&body)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("voice: Cartesia request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Llm(format!(
                "voice: Cartesia error {status}: {err_body}"
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AnimusError::Llm(format!("voice: Cartesia read failed: {e}")))?;

        let id = uuid::Uuid::new_v4();
        let mp3_path = std::env::temp_dir().join(format!("animus_tts_{id}.mp3"));
        let ogg_path = std::env::temp_dir().join(format!("animus_tts_{id}.ogg"));

        tokio::fs::write(&mp3_path, &bytes)
            .await
            .map_err(|e| AnimusError::Llm(format!("voice: failed to write MP3: {e}")))?;

        // Convert MP3 → OGG Opus so Telegram routes to sendVoice (inline player).
        let status = tokio::process::Command::new("ffmpeg")
            .args([
                "-y", "-i", mp3_path.to_str().unwrap_or(""),
                "-c:a", "libopus", "-b:a", "32k",
                ogg_path.to_str().unwrap_or(""),
            ])
            .output()
            .await
            .map_err(|e| AnimusError::Llm(format!("voice: ffmpeg not found: {e}")))?;

        let _ = tokio::fs::remove_file(&mp3_path).await;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(AnimusError::Llm(format!("voice: ffmpeg conversion failed: {stderr}")));
        }

        tracing::debug!(chars = text.len(), path = %ogg_path.display(), "Cartesia TTS synthesized");
        Ok(ogg_path)
    }
}
