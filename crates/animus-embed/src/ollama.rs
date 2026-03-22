use animus_core::embedding::EmbeddingService;
use animus_core::error::{AnimusError, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Embedding service backed by a local Ollama instance.
pub struct OllamaEmbedding {
    client: Client,
    base_url: String,
    model: String,
    dimensionality: usize,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: EmbedInput<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum EmbedInput<'a> {
    Single(&'a str),
    Batch(Vec<&'a str>),
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

impl OllamaEmbedding {
    pub fn new(base_url: &str, model: &str, dimensionality: usize) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            dimensionality,
        }
    }

    /// Probe the Ollama server to verify the model is available and detect dimensionality.
    pub async fn probe(base_url: &str, model: &str) -> Result<usize> {
        let client = Client::new();
        let url = format!("{}/api/embed", base_url.trim_end_matches('/'));
        let resp = client
            .post(&url)
            .json(&serde_json::json!({
                "model": model,
                "input": "dimensionality probe"
            }))
            .send()
            .await
            .map_err(|e| AnimusError::Embedding(format!("ollama connection failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Embedding(format!(
                "ollama probe failed ({status}): {body}"
            )));
        }

        let embed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| AnimusError::Embedding(format!("ollama response parse error: {e}")))?;

        embed
            .embeddings
            .first()
            .map(|v| v.len())
            .ok_or_else(|| AnimusError::Embedding("ollama returned no embeddings".into()))
    }
}

#[async_trait::async_trait]
impl EmbeddingService for OllamaEmbedding {
    async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/api/embed", self.base_url);
        let request = EmbedRequest {
            model: &self.model,
            input: EmbedInput::Single(text),
        };

        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| AnimusError::Embedding(format!("ollama request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Embedding(format!(
                "ollama embed failed ({status}): {body}"
            )));
        }

        let embed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| AnimusError::Embedding(format!("ollama response parse error: {e}")))?;

        embed
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| AnimusError::Embedding("ollama returned no embeddings".into()))
    }

    async fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/api/embed", self.base_url);
        let request = EmbedRequest {
            model: &self.model,
            input: EmbedInput::Batch(texts.to_vec()),
        };

        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| AnimusError::Embedding(format!("ollama batch request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Embedding(format!(
                "ollama batch embed failed ({status}): {body}"
            )));
        }

        let embed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| AnimusError::Embedding(format!("ollama response parse error: {e}")))?;

        Ok(embed.embeddings)
    }

    fn dimensionality(&self) -> usize {
        self.dimensionality
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embed_request_single_serialization() {
        let req = EmbedRequest {
            model: "mxbai-embed-large",
            input: EmbedInput::Single("hello"),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "mxbai-embed-large");
        assert_eq!(json["input"], "hello");
    }

    #[test]
    fn test_embed_request_batch_serialization() {
        let req = EmbedRequest {
            model: "mxbai-embed-large",
            input: EmbedInput::Batch(vec!["hello", "world"]),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "mxbai-embed-large");
        assert_eq!(json["input"], serde_json::json!(["hello", "world"]));
    }

    #[test]
    fn test_embed_response_deserialization() {
        let json = r#"{"model":"mxbai-embed-large","embeddings":[[0.1,0.2,0.3]]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.embeddings.len(), 1);
        assert_eq!(resp.embeddings[0], vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn test_constructor() {
        let embed = OllamaEmbedding::new("http://localhost:11434/", "mxbai-embed-large", 1024);
        assert_eq!(embed.base_url, "http://localhost:11434");
        assert_eq!(embed.model, "mxbai-embed-large");
        assert_eq!(embed.dimensionality(), 1024);
        assert_eq!(embed.model_name(), "mxbai-embed-large");
    }
}
