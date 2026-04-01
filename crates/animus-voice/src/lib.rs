//! Voice services for Animus — multi-provider STT and TTS with automatic fallback.
//!
//! # STT chain (in priority order)
//! 1. **Groq Whisper** (`whisper-large-v3-turbo`) — fastest cloud option; reuses `GROQ_API_KEY`.
//! 2. **Deepgram** (`nova-2`) — high-accuracy cloud STT; requires `ANIMUS_DEEPGRAM_KEY`.
//! 3. **macOS STT** — local HTTP wrapper around SFSpeechRecognizer; requires `ANIMUS_STT_URL`.
//!
//! # TTS chain (in priority order)
//! 1. **Cartesia** (`sonic-2`) — highest-quality neural TTS; requires `ANIMUS_CARTESIA_KEY`.
//! 2. **espeak-ng** — always available in the container; no config required; robotic but reliable.
//!
//! Each provider is tried in order. On failure the next is attempted and a WARN is logged.
//! This means voice always works as long as at least one provider in each chain is reachable.

use animus_core::{config::VoiceConfig, error::{AnimusError, Result}};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

mod stt;
mod tts;

use stt::{DeepgramStt, GroqStt, MacosStt, SttProvider};
use tts::{CartesiaTts, EspeakTts, TtsProvider};

// ---------------------------------------------------------------------------
// Public trait — unchanged; runtime holds Arc<dyn VoiceService>
// ---------------------------------------------------------------------------

#[async_trait]
pub trait VoiceService: Send + Sync {
    /// Transcribe an audio file to text. Tries providers in chain order.
    async fn transcribe(&self, audio_path: &Path) -> Result<String>;

    /// Synthesize text to an OGG Opus file. Tries providers in chain order.
    /// Returns a temp file path; the caller is responsible for deleting it after use.
    async fn synthesize(&self, text: &str) -> Result<PathBuf>;
}

// ---------------------------------------------------------------------------
// AnimusVoiceService
// ---------------------------------------------------------------------------

pub struct AnimusVoiceService {
    stt_chain: Vec<Box<dyn SttProvider>>,
    tts_chain: Vec<Box<dyn TtsProvider>>,
}

impl AnimusVoiceService {
    pub fn new(config: &VoiceConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| AnimusError::Voice(format!("voice: HTTP client: {e}")))?;

        // ── STT chain ────────────────────────────────────────────────────────
        let mut stt_chain: Vec<Box<dyn SttProvider>> = Vec::new();

        if !config.groq_api_key.is_empty() {
            stt_chain.push(Box::new(GroqStt::new(config.groq_api_key.clone(), http.clone())));
        }
        if !config.deepgram_api_key.is_empty() {
            stt_chain.push(Box::new(DeepgramStt::new(config.deepgram_api_key.clone(), http.clone())));
        }
        if !config.stt_url.is_empty() {
            stt_chain.push(Box::new(MacosStt::new(
                config.stt_url.clone(),
                config.stt_key.clone(),
                http.clone(),
            )));
        }

        if stt_chain.is_empty() {
            return Err(AnimusError::Voice(
                "voice: no STT providers configured — set GROQ_API_KEY, ANIMUS_DEEPGRAM_KEY, or ANIMUS_STT_URL".to_string(),
            ));
        }

        // ── TTS chain ────────────────────────────────────────────────────────
        let mut tts_chain: Vec<Box<dyn TtsProvider>> = Vec::new();

        if !config.cartesia_api_key.is_empty() && !config.cartesia_voice_id.is_empty() {
            tts_chain.push(Box::new(CartesiaTts::new(
                config.cartesia_api_key.clone(),
                config.cartesia_voice_id.clone(),
                config.cartesia_model.clone(),
                http.clone(),
            )));
        }
        // espeak-ng is always last — no config required, always in container
        tts_chain.push(Box::new(EspeakTts));

        let stt_names: Vec<&str> = stt_chain.iter().map(|p| p.name()).collect();
        let tts_names: Vec<&str> = tts_chain.iter().map(|p| p.name()).collect();
        tracing::info!(
            stt = %stt_names.join(" → "),
            tts = %tts_names.join(" → "),
            "Voice service initialized"
        );

        Ok(Self { stt_chain, tts_chain })
    }
}

#[async_trait]
impl VoiceService for AnimusVoiceService {
    async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let mut last_err = AnimusError::Voice("no STT providers configured".to_string());
        for provider in &self.stt_chain {
            match provider.transcribe(audio_path).await {
                Ok(text) => {
                    tracing::debug!(provider = provider.name(), chars = text.len(), "STT success");
                    return Ok(text);
                }
                Err(e) => {
                    tracing::warn!(provider = provider.name(), error = %e, "STT provider failed, trying next");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    async fn synthesize(&self, text: &str) -> Result<PathBuf> {
        let mut last_err = AnimusError::Voice("no TTS providers configured".to_string());
        for provider in &self.tts_chain {
            match provider.synthesize(text).await {
                Ok(path) => {
                    tracing::debug!(provider = provider.name(), path = %path.display(), "TTS success");
                    return Ok(path);
                }
                Err(e) => {
                    tracing::warn!(provider = provider.name(), error = %e, "TTS provider failed, trying next");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }
}
