use thiserror::Error;

#[derive(Error, Debug)]
pub enum AnimusError {
    #[error("segment not found: {0}")]
    SegmentNotFound(uuid::Uuid),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("index error: {0}")]
    Index(String),

    #[error("embedding error: {0}")]
    Embedding(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] bincode::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("context budget exceeded: need {needed} tokens, have {available}")]
    ContextBudgetExceeded { needed: usize, available: usize },

    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("identity error: {0}")]
    Identity(String),

    #[error("interface error: {0}")]
    Interface(String),

    #[error("goal error: {0}")]
    Goal(String),

    #[error("sensorium error: {0}")]
    Sensorium(String),

    #[error("threading error: {0}")]
    Threading(String),

    #[error("federation error: {0}")]
    Federation(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AnimusError>;
