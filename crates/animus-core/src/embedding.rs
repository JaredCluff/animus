use crate::error::Result;

/// Trait for generating vector embeddings from content.
/// All layers that need embeddings route through this abstraction.
#[async_trait::async_trait]
pub trait EmbeddingService: Send + Sync {
    /// Generate an embedding vector for the given text.
    async fn embed_text(&self, text: &str) -> Result<Vec<f32>>;

    /// Generate embeddings for multiple texts (batch).
    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed_text(text).await?);
        }
        Ok(results)
    }

    /// The dimensionality of vectors produced by this service.
    fn dimensionality(&self) -> usize;

    /// Human-readable name of the embedding model.
    fn model_name(&self) -> &str;
}
