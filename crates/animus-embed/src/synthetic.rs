use animus_core::embedding::EmbeddingService;
use animus_core::error::Result;

/// A deterministic embedding service for testing.
/// Generates embeddings based on simple text hashing — not semantically meaningful,
/// but deterministic and consistent for unit/integration tests that don't need real ML.
pub struct SyntheticEmbedding {
    dimensionality: usize,
}

impl SyntheticEmbedding {
    pub fn new(dimensionality: usize) -> Self {
        Self { dimensionality }
    }
}

#[async_trait::async_trait]
impl EmbeddingService for SyntheticEmbedding {
    async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        // Simple deterministic hash-based embedding
        let mut embedding = vec![0.0f32; self.dimensionality];
        for (i, byte) in text.bytes().enumerate() {
            let idx = i % self.dimensionality;
            embedding[idx] += byte as f32 / 255.0;
        }
        // Normalize to unit vector
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut embedding {
                *v /= norm;
            }
        }
        Ok(embedding)
    }

    fn dimensionality(&self) -> usize {
        self.dimensionality
    }

    fn model_name(&self) -> &str {
        "SyntheticEmbedding"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_deterministic() {
        let embedder = SyntheticEmbedding::new(8);
        let e1 = embedder.embed_text("hello world").await.unwrap();
        let e2 = embedder.embed_text("hello world").await.unwrap();
        assert_eq!(e1, e2);
    }

    #[tokio::test]
    async fn test_correct_dimensionality() {
        let embedder = SyntheticEmbedding::new(16);
        let e = embedder.embed_text("test").await.unwrap();
        assert_eq!(e.len(), 16);
    }

    #[tokio::test]
    async fn test_normalized() {
        let embedder = SyntheticEmbedding::new(8);
        let e = embedder.embed_text("some text").await.unwrap();
        let norm: f32 = e.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "should be unit vector, got norm={norm}");
    }
}
