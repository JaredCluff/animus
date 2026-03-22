use crate::tier::TierConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level configuration for an Animus instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimusConfig {
    /// Directory where VectorFS stores data.
    pub data_dir: PathBuf,

    /// Embedding model configuration.
    pub embedding: EmbeddingConfig,

    /// VectorFS configuration.
    pub vectorfs: VectorFSConfig,

    /// Mnemos configuration.
    pub mnemos: MnemosConfig,

    /// Tier management configuration.
    pub tier: TierConfig,

    /// Cortex reasoning layer configuration.
    pub cortex: CortexConfig,

    /// Terminal interface configuration.
    pub interface: InterfaceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Path to the embedding model directory.
    pub model_dir: PathBuf,
    /// Which tier of embedding model to use.
    pub tier: EmbeddingTier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmbeddingTier {
    /// EmbeddingGemma 300M — text only, constrained devices.
    Tier1Gemma,
    /// Nomic Embed Multimodal 3B — text + images.
    Tier2Nomic,
    /// Gemini Embedding 2 API — full multimodal (cloud).
    Tier3GeminiApi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorFSConfig {
    /// Vector dimensionality (must match embedding model).
    pub dimensionality: usize,
    /// Maximum number of segments (hint for HNSW pre-allocation).
    pub max_segments: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MnemosConfig {
    /// Maximum token budget for context assembly.
    pub context_token_budget: usize,
    /// Number of segments to retrieve per query.
    pub retrieval_top_k: usize,
    /// Cosine similarity threshold for consolidation.
    pub consolidation_similarity_threshold: f32,
}

/// Configuration for the Cortex reasoning layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexConfig {
    /// LLM provider name (e.g., "anthropic").
    pub llm_provider: String,
    /// Model identifier (e.g., "claude-sonnet-4-20250514").
    pub model_id: String,
    /// API key for the LLM provider. Always read from env at runtime; never serialized.
    #[serde(skip)]
    pub api_key: Option<String>,
    /// Maximum tokens for LLM response.
    pub max_response_tokens: usize,
    /// System prompt prepended to every reasoning call.
    pub system_prompt: String,
}

impl Default for CortexConfig {
    fn default() -> Self {
        Self {
            llm_provider: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
            api_key: None,
            max_response_tokens: 4096,
            system_prompt: String::new(),
        }
    }
}

/// Configuration for the terminal interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceConfig {
    /// Prompt string shown to the user.
    pub prompt: String,
    /// Whether to display system status on startup.
    pub show_status_on_start: bool,
}

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self {
            prompt: ">> ".to_string(),
            show_status_on_start: true,
        }
    }
}

impl Default for AnimusConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./animus-data"),
            embedding: EmbeddingConfig {
                model_dir: PathBuf::from("./models/embeddinggemma-300m"),
                tier: EmbeddingTier::Tier1Gemma,
            },
            vectorfs: VectorFSConfig {
                dimensionality: 768,
                max_segments: 100_000,
            },
            mnemos: MnemosConfig {
                context_token_budget: 100_000,
                retrieval_top_k: 20,
                consolidation_similarity_threshold: 0.95,
            },
            tier: TierConfig::default(),
            cortex: CortexConfig::default(),
            interface: InterfaceConfig::default(),
        }
    }
}
