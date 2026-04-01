use animus_core::error::{AnimusError, Result};
use async_trait::async_trait;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

#[async_trait]
pub(crate) trait TtsProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn synthesize(&self, text: &str) -> Result<PathBuf>;
}

// ---------------------------------------------------------------------------
// Cartesia
// ---------------------------------------------------------------------------

pub(crate) struct CartesiaTts {
    api_key: String,
    voice_id: String,
    model: String,
    http: reqwest::Client,
}

impl CartesiaTts {
    pub fn new(api_key: String, voice_id: String, model: String, http: reqwest::Client) -> Self {
        Self { api_key, voice_id, model, http }
    }
}

#[async_trait]
impl TtsProvider for CartesiaTts {
    fn name(&self) -> &str {
        "cartesia"
    }

    async fn synthesize(&self, text: &str) -> Result<PathBuf> {
        let body = serde_json::json!({
            "model_id": self.model,
            "transcript": text,
            "voice": { "mode": "id", "id": self.voice_id },
            "output_format": {
                "container": "mp3",
                "bit_rate": 128000,
                "sample_rate": 44100,
            },
        });

        let resp = self
            .http
            .post("https://api.cartesia.ai/tts/bytes")
            .header("X-API-Key", &self.api_key)
            .header("Cartesia-Version", "2024-06-10")
            .json(&body)
            .send()
            .await
            .map_err(|e| AnimusError::Voice(format!("tts/cartesia: request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Voice(format!("tts/cartesia: error {status}: {body}")));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AnimusError::Voice(format!("tts/cartesia: read failed: {e}")))?;

        let id = uuid::Uuid::new_v4();
        let mp3_path = std::env::temp_dir().join(format!("animus_tts_{id}.mp3"));
        let ogg_path = std::env::temp_dir().join(format!("animus_tts_{id}.ogg"));

        tokio::fs::write(&mp3_path, &bytes)
            .await
            .map_err(|e| AnimusError::Voice(format!("tts/cartesia: write MP3: {e}")))?;

        let result = tokio::process::Command::new("ffmpeg")
            .args([
                "-y", "-i", mp3_path.to_str().unwrap_or(""),
                "-c:a", "libopus", "-b:a", "32k",
                ogg_path.to_str().unwrap_or(""),
            ])
            .output()
            .await
            .map_err(|e| AnimusError::Voice(format!("tts/cartesia: ffmpeg not found: {e}")))?;

        let _ = tokio::fs::remove_file(&mp3_path).await;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(AnimusError::Voice(format!("tts/cartesia: ffmpeg failed: {stderr}")));
        }

        tracing::debug!(chars = text.len(), "Cartesia TTS synthesized");
        Ok(ogg_path)
    }
}

// ---------------------------------------------------------------------------
// espeak-ng fallback (always available in container)
// ---------------------------------------------------------------------------

pub(crate) struct EspeakTts;

#[async_trait]
impl TtsProvider for EspeakTts {
    fn name(&self) -> &str {
        "espeak-ng"
    }

    async fn synthesize(&self, text: &str) -> Result<PathBuf> {
        let id = uuid::Uuid::new_v4();
        let wav_path = std::env::temp_dir().join(format!("animus_tts_{id}.wav"));
        let ogg_path = std::env::temp_dir().join(format!("animus_tts_{id}.ogg"));

        let result = tokio::process::Command::new("espeak-ng")
            .args([
                "-w", wav_path.to_str().unwrap_or(""),
                // Slightly slower rate (default 175) for better clarity
                "-s", "155",
                text,
            ])
            .output()
            .await
            .map_err(|e| AnimusError::Voice(format!("tts/espeak: not found: {e}")))?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(AnimusError::Voice(format!("tts/espeak: failed: {stderr}")));
        }

        let ffmpeg = tokio::process::Command::new("ffmpeg")
            .args([
                "-y", "-i", wav_path.to_str().unwrap_or(""),
                "-c:a", "libopus", "-b:a", "32k",
                ogg_path.to_str().unwrap_or(""),
            ])
            .output()
            .await
            .map_err(|e| AnimusError::Voice(format!("tts/espeak: ffmpeg not found: {e}")))?;

        let _ = tokio::fs::remove_file(&wav_path).await;

        if !ffmpeg.status.success() {
            let stderr = String::from_utf8_lossy(&ffmpeg.stderr);
            return Err(AnimusError::Voice(format!("tts/espeak: ffmpeg failed: {stderr}")));
        }

        tracing::debug!(chars = text.len(), "espeak-ng TTS synthesized (fallback)");
        Ok(ogg_path)
    }
}
