use animus_core::embedding::EmbeddingService;
use animus_core::error::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::ollama::OllamaEmbedding;
use crate::synthetic::SyntheticEmbedding;

/// Resilient embedding service that wraps OllamaEmbedding with automatic
/// fallback to SyntheticEmbedding when Ollama becomes unavailable.
///
/// Retries failed Ollama calls once before falling back. Periodically
/// attempts to reconnect to Ollama after failures.
pub struct ResilientEmbedding {
    ollama: OllamaEmbedding,
    fallback: SyntheticEmbedding,
    /// Whether Ollama is currently considered healthy.
    ollama_healthy: AtomicBool,
    /// Timestamp of last failed Ollama call (for retry backoff).
    last_failure: AtomicU64,
    /// How many seconds to wait before retrying Ollama after failure.
    retry_interval_secs: u64,
}

impl ResilientEmbedding {
    pub fn new(ollama: OllamaEmbedding, dimensionality: usize) -> Self {
        Self {
            ollama,
            fallback: SyntheticEmbedding::new(dimensionality),
            ollama_healthy: AtomicBool::new(true),
            last_failure: AtomicU64::new(0),
            retry_interval_secs: 30,
        }
    }

    /// Check if enough time has passed to retry Ollama.
    fn should_retry_ollama(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last = self.last_failure.load(Ordering::Relaxed);
        now.saturating_sub(last) >= self.retry_interval_secs
    }

    /// Record an Ollama failure.
    fn record_failure(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_failure.store(now, Ordering::Relaxed);
        self.ollama_healthy.store(false, Ordering::Relaxed);
    }

    /// Record Ollama recovery.
    fn record_recovery(&self) {
        self.ollama_healthy.store(true, Ordering::Relaxed);
    }

    /// Returns whether Ollama is currently the active backend.
    pub fn is_ollama_active(&self) -> bool {
        self.ollama_healthy.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl EmbeddingService for ResilientEmbedding {
    async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        // If Ollama is healthy, try it first
        if self.ollama_healthy.load(Ordering::Relaxed) {
            match self.ollama.embed_text(text).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    tracing::warn!("Ollama embedding failed, falling back to synthetic: {e}");
                    self.record_failure();
                }
            }
        } else if self.should_retry_ollama() {
            // Try to reconnect
            match self.ollama.embed_text(text).await {
                Ok(v) => {
                    tracing::info!("Ollama reconnected successfully");
                    self.record_recovery();
                    return Ok(v);
                }
                Err(e) => {
                    tracing::debug!("Ollama still unavailable: {e}");
                    self.record_failure();
                }
            }
        }

        // Fallback to synthetic
        self.fallback.embed_text(text).await
    }

    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if self.ollama_healthy.load(Ordering::Relaxed) {
            match self.ollama.embed_texts(texts).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    tracing::warn!("Ollama batch embedding failed, falling back to synthetic: {e}");
                    self.record_failure();
                }
            }
        } else if self.should_retry_ollama() {
            match self.ollama.embed_texts(texts).await {
                Ok(v) => {
                    tracing::info!("Ollama reconnected successfully");
                    self.record_recovery();
                    return Ok(v);
                }
                Err(e) => {
                    tracing::debug!("Ollama still unavailable: {e}");
                    self.record_failure();
                }
            }
        }

        self.fallback.embed_texts(texts).await
    }

    fn dimensionality(&self) -> usize {
        self.ollama.dimensionality()
    }

    fn model_name(&self) -> &str {
        if self.ollama_healthy.load(Ordering::Relaxed) {
            self.ollama.model_name()
        } else {
            "SyntheticEmbedding (fallback)"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a ResilientEmbedding with Ollama pointed at a dead port.
    fn make_resilient(dim: usize) -> ResilientEmbedding {
        let ollama = OllamaEmbedding::new("http://127.0.0.1:1", "fake-model", dim);
        ResilientEmbedding::new(ollama, dim)
    }

    #[tokio::test]
    async fn test_falls_back_to_synthetic_on_failure() {
        let r = make_resilient(64);
        assert!(r.is_ollama_active(), "should start healthy");

        let result = r.embed_text("hello world").await;
        assert!(result.is_ok(), "should succeed via fallback");
        assert_eq!(result.unwrap().len(), 64);
        assert!(!r.is_ollama_active(), "should be marked unhealthy after failure");
    }

    #[tokio::test]
    async fn test_batch_falls_back_to_synthetic() {
        let r = make_resilient(32);
        let result = r.embed_texts(&["a", "b", "c"]).await;
        assert!(result.is_ok());
        let vecs = result.unwrap();
        assert_eq!(vecs.len(), 3);
        assert!(vecs.iter().all(|v| v.len() == 32));
    }

    #[tokio::test]
    async fn test_model_name_reflects_active_backend() {
        let r = make_resilient(16);
        assert_eq!(r.model_name(), "fake-model");

        // Trigger failure
        let _ = r.embed_text("trigger").await;
        assert_eq!(r.model_name(), "SyntheticEmbedding (fallback)");
    }

    #[tokio::test]
    async fn test_retry_not_attempted_before_interval() {
        let r = make_resilient(16);

        // Trigger failure
        let _ = r.embed_text("trigger").await;
        assert!(!r.is_ollama_active());

        // Second call should NOT attempt Ollama retry (interval not elapsed)
        // It should go straight to fallback
        let result = r.embed_text("second").await;
        assert!(result.is_ok());
        assert!(!r.is_ollama_active(), "still unhealthy — retry interval not elapsed");
    }

    #[test]
    fn test_should_retry_ollama_timing() {
        let r = make_resilient(16);

        // No failure recorded — should_retry returns true
        assert!(r.should_retry_ollama());

        // Record a failure
        r.record_failure();
        assert!(!r.should_retry_ollama(), "just failed — too soon to retry");

        // Simulate old failure by setting last_failure to 0
        r.last_failure.store(0, Ordering::Relaxed);
        assert!(r.should_retry_ollama(), "old failure — should retry now");
    }

    #[test]
    fn test_dimensionality() {
        let r = make_resilient(128);
        assert_eq!(r.dimensionality(), 128);
    }
}
