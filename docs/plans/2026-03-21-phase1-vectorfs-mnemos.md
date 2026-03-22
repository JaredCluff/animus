# Phase 1: VectorFS + Mnemos Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build AI-native persistent storage (VectorFS) and memory management (Mnemos) — the foundation that makes Animus fundamentally different from any other LLM wrapper.

**Architecture:** Rust workspace with three crates: `animus-core` (shared types), `animus-vectorfs` (segment storage + HNSW index + tiering), `animus-mnemos` (context assembly + eviction + consolidation). VectorFS stores Segments indexed by embedding vectors. Mnemos assembles optimal LLM context windows from stored Segments. EmbeddingGemma 300M via ONNX (`ort` crate) provides embeddings locally.

**Tech Stack:** Rust (2021 edition), `ort` 2.0 (ONNX runtime), `hnsw_rs` (HNSW index), `memmap2` (memory-mapped storage), `serde`/`bincode` (serialization), `uuid`, `chrono`, `tokio` (async runtime), `tracing` (logging)

**Spec:** `docs/specs/2026-03-21-animus-design.md`
**Genesis doc:** `docs/00-genesis-conversation.md`

---

## File Structure

```
animus/
  Cargo.toml                           # workspace manifest
  crates/
    animus-core/
      Cargo.toml
      src/
        lib.rs                         # re-exports all public types
        segment.rs                     # Segment struct, Content, Source, Tier enums
        identity.rs                    # AnimusIdentity, SegmentId, InstanceId
        embedding.rs                   # EmbeddingService trait
        tier.rs                        # Tier enum, TierScore, scoring weights/thresholds
        config.rs                      # AnimusConfig, VectorFSConfig, MnemosConfig
        error.rs                       # AnimusError enum, Result alias
    animus-vectorfs/
      Cargo.toml
      src/
        lib.rs                         # re-exports, VectorStore trait
        store.rs                       # MmapVectorStore implementation
        index.rs                       # HnswIndex wrapper around hnsw_rs
        tier_manager.rs                # background tier promotion/demotion
        snapshot.rs                    # snapshot/restore for fork/backup
    animus-mnemos/
      Cargo.toml
      src/
        lib.rs                         # re-exports, Mnemos struct
        assembler.rs                   # ContextAssembler — builds LLM context
        evictor.rs                     # intelligent eviction with summaries
        consolidator.rs                # background segment merging/dedup
        quality.rs                     # quality gate heuristics (confidence tracking)
    animus-embed/
      Cargo.toml
      src/
        lib.rs                         # re-exports
        gemma.rs                       # EmbeddingGemma 300M ONNX implementation
        nomic.rs                       # Nomic Embed Multimodal 3B (stub, Tier 2)
  tests/
    integration/
      vectorfs_basic.rs               # store, query, get, delete segments
      vectorfs_tiering.rs             # tier promotion/demotion
      mnemos_assembly.rs              # context assembly from stored segments
      mnemos_eviction.rs              # eviction under context budget
      embedding_gemma.rs              # real EmbeddingGemma inference
```

---

## Task 1: Workspace Scaffold + Core Types

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/animus-core/Cargo.toml`
- Create: `crates/animus-core/src/lib.rs`
- Create: `crates/animus-core/src/error.rs`
- Create: `crates/animus-core/src/identity.rs`
- Create: `crates/animus-core/src/segment.rs`

### Steps

- [ ] **Step 1: Create workspace Cargo.toml**

```toml
[workspace]
resolver = "2"
members = [
    "crates/animus-core",
    "crates/animus-vectorfs",
    "crates/animus-mnemos",
    "crates/animus-embed",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
repository = "https://github.com/JaredCluff/animus"

[workspace.dependencies]
animus-core = { path = "crates/animus-core" }
animus-vectorfs = { path = "crates/animus-vectorfs" }
animus-mnemos = { path = "crates/animus-mnemos" }
animus-embed = { path = "crates/animus-embed" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
bincode = "1"
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
thiserror = "2"
```

- [ ] **Step 2: Create animus-core Cargo.toml**

```toml
[package]
name = "animus-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
uuid = { workspace = true }
chrono = { workspace = true }
thiserror = { workspace = true }
bincode = { workspace = true }
```

- [ ] **Step 3: Create error.rs**

```rust
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

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AnimusError>;
```

- [ ] **Step 4: Create identity.rs**

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a Segment in VectorFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SegmentId(pub Uuid);

impl SegmentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SegmentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SegmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for an AILF instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub Uuid);

impl InstanceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for InstanceId {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique identifier for a reasoning thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(pub Uuid);

impl ThreadId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ThreadId {
    fn default() -> Self {
        Self::new()
    }
}

/// Unique identifier for a Sensorium event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

/// Unique identifier for a consent policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyId(pub Uuid);

/// Unique identifier for a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoalId(pub Uuid);

/// Unique identifier for a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub Uuid);

impl SnapshotId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
```

- [ ] **Step 5: Create segment.rs**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{EventId, InstanceId, PolicyId, SegmentId, ThreadId};

/// The atomic unit of VectorFS storage. A unit of meaning with context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Unique, immutable identifier.
    pub id: SegmentId,

    /// Vector representation of content.
    pub embedding: Vec<f32>,

    /// The actual knowledge stored.
    pub content: Content,

    /// Where this segment came from.
    pub source: Source,

    /// Validation level (0.0 - 1.0). Higher = more trusted.
    pub confidence: f32,

    /// Parent segments (for consolidation tracking).
    pub lineage: Vec<SegmentId>,

    /// Current storage tier.
    pub tier: Tier,

    /// Current computed relevance score.
    pub relevance_score: f32,

    /// How many times this segment has been retrieved.
    pub access_count: u64,

    /// Last time this segment was accessed.
    pub last_accessed: DateTime<Utc>,

    /// When this segment was created.
    pub created: DateTime<Utc>,

    /// Weighted links to related segments.
    pub associations: Vec<(SegmentId, f32)>,

    /// Which consent rule permitted creation.
    pub consent_policy: Option<PolicyId>,

    /// Who can see this segment.
    pub observable_by: Vec<Principal>,
}

impl Segment {
    /// Create a new segment with the given content and embedding.
    pub fn new(content: Content, embedding: Vec<f32>, source: Source) -> Self {
        let now = Utc::now();
        Self {
            id: SegmentId::new(),
            embedding,
            content,
            source,
            confidence: 0.5,
            lineage: Vec::new(),
            tier: Tier::Warm,
            relevance_score: 0.5,
            access_count: 0,
            last_accessed: now,
            created: now,
            associations: Vec::new(),
            consent_policy: None,
            observable_by: Vec::new(),
        }
    }

    /// Record an access, updating count and timestamp.
    pub fn record_access(&mut self) {
        self.access_count += 1;
        self.last_accessed = Utc::now();
    }

    /// Estimated token count for context budgeting.
    pub fn estimated_tokens(&self) -> usize {
        match &self.content {
            Content::Text(t) => t.len() / 4, // rough estimate: 4 chars per token
            Content::Structured(v) => v.to_string().len() / 4,
            Content::Binary { .. } => 0, // binary content doesn't go into LLM context
            Content::Reference { summary, .. } => summary.len() / 4,
        }
    }
}

/// The content stored in a segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    Structured(serde_json::Value),
    Binary { mime_type: String, data: Vec<u8> },
    Reference { uri: String, summary: String },
}

/// Where a segment originated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Source {
    Conversation {
        thread_id: ThreadId,
        turn: u64,
    },
    Observation {
        event_type: String,
        raw_event_id: EventId,
    },
    Consolidation {
        merged_from: Vec<SegmentId>,
    },
    Federation {
        source_ailf: InstanceId,
        original_id: SegmentId,
    },
    SelfDerived {
        reasoning_chain: String,
    },
    /// Bootstrap or manually injected knowledge.
    Manual {
        description: String,
    },
}

/// Storage tier for a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// Currently loaded in reasoning context.
    Hot,
    /// Vector-indexed, retrievable in <10ms.
    Warm,
    /// Compressed, archived, retrievable but not instant.
    Cold,
}

/// An entity that can observe or be observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Principal {
    Ailf(InstanceId),
    Human(String),
}
```

- [ ] **Step 6: Create lib.rs for animus-core**

```rust
pub mod error;
pub mod identity;
pub mod segment;

pub use error::{AnimusError, Result};
pub use identity::{EventId, GoalId, InstanceId, PolicyId, SegmentId, SnapshotId, ThreadId};
pub use segment::{Content, Principal, Segment, Source, Tier};
```

- [ ] **Step 7: Verify it compiles**

Run: `cargo check -p animus-core`
Expected: successful compilation with possible warnings

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/animus-core/
git commit -m "feat: scaffold workspace and animus-core types

Segment, Content, Source, Tier, identity types, error types.
Foundation for VectorFS and Mnemos."
```

---

## Task 2: EmbeddingService Trait + EmbeddingGemma Provider

**Files:**
- Create: `crates/animus-core/src/embedding.rs` (trait)
- Create: `crates/animus-embed/Cargo.toml`
- Create: `crates/animus-embed/src/lib.rs`
- Create: `crates/animus-embed/src/gemma.rs`
- Test: `tests/integration/embedding_gemma.rs`

### Steps

- [ ] **Step 1: Add embedding trait to animus-core**

Create `crates/animus-core/src/embedding.rs`:

```rust
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
```

- [ ] **Step 2: Add async-trait dependency to animus-core**

Add to `crates/animus-core/Cargo.toml` dependencies:
```toml
async-trait = "0.1"
```

Add to `crates/animus-core/src/lib.rs`:
```rust
pub mod embedding;
pub use embedding::EmbeddingService;
```

- [ ] **Step 3: Create animus-embed Cargo.toml**

```toml
[package]
name = "animus-embed"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
animus-core = { workspace = true }
ort = "2.0.0-rc.12"
tokenizers = "0.21"
async-trait = "0.1"
tokio = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 4: Create animus-embed/src/gemma.rs**

```rust
use animus_core::embedding::EmbeddingService;
use animus_core::error::{AnimusError, Result};
use ort::session::Session;
use std::path::Path;
use std::sync::Arc;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;

/// EmbeddingGemma 300M implementation via ONNX runtime.
/// Tier 1 embedding: text-only, <200MB, runs on constrained devices.
pub struct GemmaEmbedding {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
    dimensionality: usize,
}

impl GemmaEmbedding {
    /// Load the EmbeddingGemma model from the given directory.
    /// The directory should contain `model.onnx` and `tokenizer.json`.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        let session = Session::builder()
            .map_err(|e| AnimusError::Embedding(format!("failed to create session builder: {e}")))?
            .with_intra_threads(4)
            .map_err(|e| AnimusError::Embedding(format!("failed to set threads: {e}")))?
            .commit_from_file(&model_path)
            .map_err(|e| AnimusError::Embedding(format!("failed to load model: {e}")))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| AnimusError::Embedding(format!("failed to load tokenizer: {e}")))?;

        // EmbeddingGemma default output: 768 dimensions
        let dimensionality = 768;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
            dimensionality,
        })
    }
}

#[async_trait::async_trait]
impl EmbeddingService for GemmaEmbedding {
    async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| AnimusError::Embedding(format!("tokenization failed: {e}")))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let seq_len = input_ids.len();

        let input_ids_array =
            ndarray::Array2::from_shape_vec((1, seq_len), input_ids)
                .map_err(|e| AnimusError::Embedding(format!("shape error: {e}")))?;
        let attention_mask_array =
            ndarray::Array2::from_shape_vec((1, seq_len), attention_mask)
                .map_err(|e| AnimusError::Embedding(format!("shape error: {e}")))?;

        let session = self.session.lock().await;
        let outputs = session
            .run(ort::inputs! {
                "input_ids" => input_ids_array,
                "attention_mask" => attention_mask_array,
            }.map_err(|e| AnimusError::Embedding(format!("input error: {e}")))?)
            .map_err(|e| AnimusError::Embedding(format!("inference failed: {e}")))?;

        // Extract embedding from output — typically the first output tensor.
        // EmbeddingGemma outputs shape [1, dim].
        let embedding_tensor = outputs
            .get(0)
            .ok_or_else(|| AnimusError::Embedding("no output tensor".into()))?;

        let embedding_view = embedding_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| AnimusError::Embedding(format!("tensor extraction failed: {e}")))?;

        let embedding: Vec<f32> = embedding_view.iter().copied().collect();

        // Truncate or verify dimensionality
        if embedding.len() < self.dimensionality {
            return Err(AnimusError::Embedding(format!(
                "embedding too short: got {}, expected {}",
                embedding.len(),
                self.dimensionality
            )));
        }

        Ok(embedding[..self.dimensionality].to_vec())
    }

    fn dimensionality(&self) -> usize {
        self.dimensionality
    }

    fn model_name(&self) -> &str {
        "EmbeddingGemma-300M"
    }
}
```

- [ ] **Step 5: Create animus-embed/src/lib.rs**

```rust
pub mod gemma;

pub use gemma::GemmaEmbedding;
```

- [ ] **Step 6: Add ndarray dependency to animus-embed Cargo.toml**

Add to dependencies:
```toml
ndarray = "0.16"
```

- [ ] **Step 7: Verify it compiles**

Run: `cargo check -p animus-embed`
Expected: successful compilation

- [ ] **Step 8: Create integration test for EmbeddingGemma**

Create `tests/integration/embedding_gemma.rs`:

```rust
//! Integration test for EmbeddingGemma — requires model files.
//! Skip with: cargo test -- --skip embedding_gemma
//!
//! To run: download EmbeddingGemma ONNX from
//! https://huggingface.co/onnx-community/embeddinggemma-300m-ONNX
//! and place in models/embeddinggemma-300m/

use animus_core::embedding::EmbeddingService;
use animus_embed::GemmaEmbedding;
use std::path::Path;

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[tokio::test]
async fn test_embedding_gemma_semantic_similarity() {
    let model_dir = Path::new("models/embeddinggemma-300m");
    if !model_dir.exists() {
        eprintln!("Skipping: model not downloaded. See test comments.");
        return;
    }

    let embedder = GemmaEmbedding::load(model_dir).expect("failed to load model");

    assert_eq!(embedder.dimensionality(), 768);
    assert_eq!(embedder.model_name(), "EmbeddingGemma-300M");

    let emb_cat = embedder.embed_text("The cat sat on the mat").await.unwrap();
    let emb_dog = embedder.embed_text("A dog lay on the rug").await.unwrap();
    let emb_code = embedder.embed_text("fn main() { println!(\"hello\"); }").await.unwrap();

    assert_eq!(emb_cat.len(), 768);

    // Cat/dog should be more similar to each other than to code
    let sim_cat_dog = cosine_similarity(&emb_cat, &emb_dog);
    let sim_cat_code = cosine_similarity(&emb_cat, &emb_code);

    assert!(
        sim_cat_dog > sim_cat_code,
        "cat-dog similarity ({sim_cat_dog}) should be higher than cat-code ({sim_cat_code})"
    );
}

#[tokio::test]
async fn test_embedding_gemma_deterministic() {
    let model_dir = Path::new("models/embeddinggemma-300m");
    if !model_dir.exists() {
        return;
    }

    let embedder = GemmaEmbedding::load(model_dir).expect("failed to load model");

    let emb1 = embedder.embed_text("test input").await.unwrap();
    let emb2 = embedder.embed_text("test input").await.unwrap();

    assert_eq!(emb1, emb2, "same input should produce identical embeddings");
}
```

- [ ] **Step 9: Commit**

```bash
git add crates/animus-core/src/embedding.rs crates/animus-embed/ tests/integration/embedding_gemma.rs
git commit -m "feat: add EmbeddingService trait and EmbeddingGemma provider

Tier 1 embedding via ONNX runtime. 768-dim vectors.
Integration tests require model download from HuggingFace."
```

---

## Task 3: VectorFS — VectorStore Trait + HNSW Index

**Files:**
- Create: `crates/animus-vectorfs/Cargo.toml`
- Create: `crates/animus-vectorfs/src/lib.rs`
- Create: `crates/animus-vectorfs/src/index.rs`
- Test: (unit tests inline)

### Steps

- [ ] **Step 1: Create animus-vectorfs Cargo.toml**

```toml
[package]
name = "animus-vectorfs"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
animus-core = { workspace = true }
hnsw_rs = "0.3"
serde = { workspace = true }
bincode = { workspace = true }
uuid = { workspace = true }
chrono = { workspace = true }
tracing = { workspace = true }
parking_lot = "0.12"

[dev-dependencies]
tokio = { workspace = true }
rand = "0.8"
```

- [ ] **Step 2: Create VectorStore trait in lib.rs**

```rust
use animus_core::{AnimusError, Result, Segment, SegmentId, SnapshotId, Tier};

/// Metadata update for a segment (partial update without replacing content).
#[derive(Debug, Default)]
pub struct SegmentUpdate {
    pub relevance_score: Option<f32>,
    pub confidence: Option<f32>,
    pub associations: Option<Vec<(SegmentId, f32)>>,
}

/// The core storage abstraction for VectorFS.
pub trait VectorStore: Send + Sync {
    /// Store a new segment. Returns the segment's ID.
    fn store(&self, segment: Segment) -> Result<SegmentId>;

    /// Retrieve segments by semantic similarity to the given embedding.
    fn query(
        &self,
        embedding: &[f32],
        top_k: usize,
        tier_filter: Option<Tier>,
    ) -> Result<Vec<Segment>>;

    /// Retrieve a segment by exact ID.
    fn get(&self, id: SegmentId) -> Result<Option<Segment>>;

    /// Update segment metadata without replacing content.
    fn update_meta(&self, id: SegmentId, update: SegmentUpdate) -> Result<()>;

    /// Change a segment's storage tier.
    fn set_tier(&self, id: SegmentId, tier: Tier) -> Result<()>;

    /// Permanently delete a segment.
    fn delete(&self, id: SegmentId) -> Result<()>;

    /// Merge multiple segments into one consolidated segment.
    /// Source segments are deleted; the merged segment is stored.
    fn merge(&self, source_ids: Vec<SegmentId>, merged: Segment) -> Result<SegmentId>;

    /// Count segments, optionally filtered by tier.
    fn count(&self, tier_filter: Option<Tier>) -> usize;

    /// Get all segment IDs, optionally filtered by tier.
    fn segment_ids(&self, tier_filter: Option<Tier>) -> Vec<SegmentId>;
}

pub mod index;
pub mod store;
pub mod tier_manager;
```

- [ ] **Step 3: Create HNSW index wrapper in index.rs**

```rust
use animus_core::error::{AnimusError, Result};
use animus_core::identity::SegmentId;
use hnsw_rs::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Wrapper around hnsw_rs providing vector similarity search.
pub struct HnswIndex {
    /// The HNSW graph. hnsw_rs uses f32 distance.
    hnsw: RwLock<Hnsw<f32, DistCosine>>,
    /// Map from internal HNSW data ID to SegmentId.
    id_map: RwLock<HashMap<usize, SegmentId>>,
    /// Reverse map from SegmentId to internal HNSW data ID.
    reverse_map: RwLock<HashMap<SegmentId, usize>>,
    /// Next internal ID to assign.
    next_id: RwLock<usize>,
    /// Vector dimensionality.
    dimensionality: usize,
}

impl HnswIndex {
    /// Create a new HNSW index for the given dimensionality.
    ///
    /// - `max_elements`: estimated max number of elements (can grow)
    /// - `max_nb_connection`: HNSW M parameter (16 is a good default)
    /// - `ef_construction`: build-time quality (200 is a good default)
    pub fn new(dimensionality: usize, max_elements: usize) -> Self {
        let max_nb_connection = 16;
        let ef_construction = 200;
        let nb_layer = 16;

        let hnsw = Hnsw::new(
            max_nb_connection,
            max_elements,
            nb_layer,
            ef_construction,
            DistCosine,
        );

        Self {
            hnsw: RwLock::new(hnsw),
            id_map: RwLock::new(HashMap::new()),
            reverse_map: RwLock::new(HashMap::new()),
            next_id: RwLock::new(0),
            dimensionality,
        }
    }

    /// Insert a vector for the given segment ID.
    pub fn insert(&self, segment_id: SegmentId, embedding: &[f32]) -> Result<()> {
        if embedding.len() != self.dimensionality {
            return Err(AnimusError::DimensionMismatch {
                expected: self.dimensionality,
                actual: embedding.len(),
            });
        }

        let internal_id = {
            let mut next = self.next_id.write();
            let id = *next;
            *next += 1;
            id
        };

        self.id_map.write().insert(internal_id, segment_id);
        self.reverse_map.write().insert(segment_id, internal_id);

        let data_vec = vec![(embedding, internal_id)];
        self.hnsw.write().parallel_insert(&data_vec);

        Ok(())
    }

    /// Search for the top-k nearest neighbors to the given query embedding.
    /// Returns (SegmentId, distance) pairs sorted by distance (ascending).
    pub fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(SegmentId, f32)>> {
        if query.len() != self.dimensionality {
            return Err(AnimusError::DimensionMismatch {
                expected: self.dimensionality,
                actual: query.len(),
            });
        }

        let ef_search = top_k.max(64);
        let hnsw = self.hnsw.read();
        let results = hnsw.search(query, top_k, ef_search);

        let id_map = self.id_map.read();
        let mapped: Vec<(SegmentId, f32)> = results
            .into_iter()
            .filter_map(|neighbour| {
                id_map
                    .get(&neighbour.d_id)
                    .map(|seg_id| (*seg_id, neighbour.distance))
            })
            .collect();

        Ok(mapped)
    }

    /// Remove a segment from the index.
    /// Note: hnsw_rs doesn't support true deletion — we track removed IDs
    /// and filter them from search results. Rebuild periodically.
    pub fn remove(&self, segment_id: SegmentId) -> Result<()> {
        let internal_id = self
            .reverse_map
            .write()
            .remove(&segment_id)
            .ok_or(AnimusError::SegmentNotFound(segment_id.0))?;
        self.id_map.write().remove(&internal_id);
        // hnsw_rs doesn't support deletion — the vector stays in the graph
        // but won't be returned because it's not in id_map.
        // TODO: periodic rebuild to reclaim space
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.id_map.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dimensionality(&self) -> usize {
        self.dimensionality
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vec(dim: usize) -> Vec<f32> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen::<f32>()).collect()
    }

    #[test]
    fn test_insert_and_search() {
        let index = HnswIndex::new(4, 100);
        let id1 = SegmentId::new();
        let id2 = SegmentId::new();

        let v1 = vec![1.0, 0.0, 0.0, 0.0];
        let v2 = vec![0.0, 1.0, 0.0, 0.0];

        index.insert(id1, &v1).unwrap();
        index.insert(id2, &v2).unwrap();

        let results = index.search(&v1, 2).unwrap();
        assert_eq!(results.len(), 2);
        // First result should be v1 (closest to itself)
        assert_eq!(results[0].0, id1);
    }

    #[test]
    fn test_dimension_mismatch() {
        let index = HnswIndex::new(4, 100);
        let id = SegmentId::new();
        let wrong_dim = vec![1.0, 0.0]; // only 2 dims

        let result = index.insert(id, &wrong_dim);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove() {
        let index = HnswIndex::new(4, 100);
        let id = SegmentId::new();
        index.insert(id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(index.len(), 1);

        index.remove(id).unwrap();
        assert_eq!(index.len(), 0);

        // Search should return no results for removed segment
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
        assert!(results.is_empty());
    }
}
```

- [ ] **Step 4: Verify compilation and run unit tests**

Run: `cargo test -p animus-vectorfs`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/animus-vectorfs/
git commit -m "feat: add VectorStore trait and HNSW index wrapper

VectorStore defines the storage abstraction. HnswIndex wraps hnsw_rs
for cosine similarity search with segment ID mapping."
```

---

## Task 4: VectorFS — MmapVectorStore Implementation

**Files:**
- Create: `crates/animus-vectorfs/src/store.rs`
- Test: `tests/integration/vectorfs_basic.rs`

### Steps

- [ ] **Step 1: Write integration test for basic store operations**

Create `tests/integration/vectorfs_basic.rs`:

```rust
use animus_core::{Content, Segment, Source, Tier};
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::path::PathBuf;
use tempfile::TempDir;

fn test_segment(embedding: Vec<f32>, text: &str) -> Segment {
    Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    )
}

#[test]
fn test_store_and_get() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "hello world");
    let id = seg.id;
    store.store(seg).unwrap();

    let retrieved = store.get(id).unwrap().expect("segment should exist");
    assert_eq!(retrieved.id, id);
    match &retrieved.content {
        Content::Text(t) => assert_eq!(t, "hello world"),
        _ => panic!("expected text content"),
    }
}

#[test]
fn test_query_by_similarity() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let s1 = test_segment(vec![1.0, 0.0, 0.0, 0.0], "north");
    let s2 = test_segment(vec![0.0, 1.0, 0.0, 0.0], "east");
    let s3 = test_segment(vec![0.9, 0.1, 0.0, 0.0], "mostly north");
    let id1 = s1.id;
    let id3 = s3.id;

    store.store(s1).unwrap();
    store.store(s2).unwrap();
    store.store(s3).unwrap();

    let results = store.query(&[1.0, 0.0, 0.0, 0.0], 2, None).unwrap();
    assert_eq!(results.len(), 2);
    // Should get "north" and "mostly north", not "east"
    let result_ids: Vec<_> = results.iter().map(|s| s.id).collect();
    assert!(result_ids.contains(&id1));
    assert!(result_ids.contains(&id3));
}

#[test]
fn test_query_with_tier_filter() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let s1 = test_segment(vec![1.0, 0.0, 0.0, 0.0], "warm segment");
    let s2 = test_segment(vec![0.9, 0.1, 0.0, 0.0], "will be cold");
    let id2 = s2.id;

    store.store(s1).unwrap();
    store.store(s2).unwrap();
    store.set_tier(id2, Tier::Cold).unwrap();

    // Query with Warm filter should only return s1
    let results = store.query(&[1.0, 0.0, 0.0, 0.0], 2, Some(Tier::Warm)).unwrap();
    assert_eq!(results.len(), 1);
    assert!(matches!(&results[0].content, Content::Text(t) if t == "warm segment"));
}

#[test]
fn test_delete() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "to be deleted");
    let id = seg.id;
    store.store(seg).unwrap();
    assert_eq!(store.count(None), 1);

    store.delete(id).unwrap();
    assert_eq!(store.count(None), 0);
    assert!(store.get(id).unwrap().is_none());
}

#[test]
fn test_persistence_across_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();

    let id = {
        let store = MmapVectorStore::open(&path, 4).unwrap();
        let seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "persistent");
        let id = seg.id;
        store.store(seg).unwrap();
        store.flush().unwrap();
        id
    };

    // Reopen and verify data persists
    let store = MmapVectorStore::open(&path, 4).unwrap();
    let retrieved = store.get(id).unwrap().expect("should persist across reopen");
    match &retrieved.content {
        Content::Text(t) => assert_eq!(t, "persistent"),
        _ => panic!("expected text"),
    }
}

#[test]
fn test_merge() {
    let dir = TempDir::new().unwrap();
    let store = MmapVectorStore::open(dir.path(), 4).unwrap();

    let s1 = test_segment(vec![1.0, 0.0, 0.0, 0.0], "fact A");
    let s2 = test_segment(vec![0.9, 0.1, 0.0, 0.0], "fact B");
    let id1 = s1.id;
    let id2 = s2.id;
    store.store(s1).unwrap();
    store.store(s2).unwrap();

    let merged = test_segment(vec![0.95, 0.05, 0.0, 0.0], "consolidated fact AB");
    let merged_id = store.merge(vec![id1, id2], merged).unwrap();

    // Source segments deleted
    assert!(store.get(id1).unwrap().is_none());
    assert!(store.get(id2).unwrap().is_none());

    // Merged segment exists
    let m = store.get(merged_id).unwrap().expect("merged should exist");
    match &m.content {
        Content::Text(t) => assert_eq!(t, "consolidated fact AB"),
        _ => panic!("expected text"),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test vectorfs_basic`
Expected: FAIL — `store` module doesn't exist yet

- [ ] **Step 3: Create store.rs — MmapVectorStore implementation**

```rust
use animus_core::error::{AnimusError, Result};
use animus_core::identity::SegmentId;
use animus_core::segment::{Segment, Tier};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::HnswIndex;
use crate::{SegmentUpdate, VectorStore};

/// File-backed VectorStore using memory-mapped storage and HNSW index.
/// V0.1 implementation: segments stored as individual bincode files,
/// HNSW index for vector search.
pub struct MmapVectorStore {
    /// Base directory for storage.
    base_dir: PathBuf,
    /// In-memory segment cache (all warm/hot segments).
    segments: RwLock<HashMap<SegmentId, Segment>>,
    /// HNSW vector index for similarity search.
    index: HnswIndex,
    /// Vector dimensionality.
    dimensionality: usize,
}

impl MmapVectorStore {
    /// Open or create a VectorStore at the given directory.
    pub fn open(dir: &Path, dimensionality: usize) -> Result<Self> {
        let segments_dir = dir.join("segments");
        fs::create_dir_all(&segments_dir)?;

        let index = HnswIndex::new(dimensionality, 10_000);
        let mut segments = HashMap::new();

        // Load existing segments from disk
        if segments_dir.exists() {
            for entry in fs::read_dir(&segments_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "bin") {
                    let data = fs::read(&path)?;
                    match bincode::deserialize::<Segment>(&data) {
                        Ok(segment) => {
                            // Re-insert into HNSW index
                            if let Err(e) = index.insert(segment.id, &segment.embedding) {
                                tracing::warn!(
                                    "failed to index segment {}: {e}",
                                    segment.id
                                );
                                continue;
                            }
                            segments.insert(segment.id, segment);
                        }
                        Err(e) => {
                            tracing::warn!("failed to load segment from {}: {e}", path.display());
                        }
                    }
                }
            }
        }

        tracing::info!("VectorFS opened at {} with {} segments", dir.display(), segments.len());

        Ok(Self {
            base_dir: dir.to_path_buf(),
            segments: RwLock::new(segments),
            index,
            dimensionality,
        })
    }

    /// Flush all segments to disk.
    pub fn flush(&self) -> Result<()> {
        let segments = self.segments.read();
        let segments_dir = self.base_dir.join("segments");
        for (id, segment) in segments.iter() {
            let path = segments_dir.join(format!("{}.bin", id.0));
            let data = bincode::serialize(segment)?;
            fs::write(&path, &data)?;
        }
        Ok(())
    }

    /// Write a single segment to disk.
    fn persist_segment(&self, segment: &Segment) -> Result<()> {
        let segments_dir = self.base_dir.join("segments");
        let path = segments_dir.join(format!("{}.bin", segment.id.0));
        let data = bincode::serialize(segment)?;
        fs::write(&path, &data)?;
        Ok(())
    }

    /// Remove a segment file from disk.
    fn remove_segment_file(&self, id: SegmentId) -> Result<()> {
        let path = self.base_dir.join("segments").join(format!("{}.bin", id.0));
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

impl VectorStore for MmapVectorStore {
    fn store(&self, segment: Segment) -> Result<SegmentId> {
        if segment.embedding.len() != self.dimensionality {
            return Err(AnimusError::DimensionMismatch {
                expected: self.dimensionality,
                actual: segment.embedding.len(),
            });
        }

        let id = segment.id;
        self.index.insert(id, &segment.embedding)?;
        self.persist_segment(&segment)?;
        self.segments.write().insert(id, segment);
        Ok(id)
    }

    fn query(
        &self,
        embedding: &[f32],
        top_k: usize,
        tier_filter: Option<Tier>,
    ) -> Result<Vec<Segment>> {
        // Search more than top_k in case some get filtered by tier
        let search_k = if tier_filter.is_some() {
            top_k * 3
        } else {
            top_k
        };

        let candidates = self.index.search(embedding, search_k)?;
        let segments = self.segments.read();

        let mut results: Vec<Segment> = candidates
            .into_iter()
            .filter_map(|(id, _distance)| {
                let seg = segments.get(&id)?;
                if let Some(tier) = tier_filter {
                    if seg.tier != tier {
                        return None;
                    }
                }
                Some(seg.clone())
            })
            .take(top_k)
            .collect();

        // Record access on returned segments
        drop(segments);
        let mut segments = self.segments.write();
        for result in &results {
            if let Some(seg) = segments.get_mut(&result.id) {
                seg.record_access();
            }
        }

        Ok(results)
    }

    fn get(&self, id: SegmentId) -> Result<Option<Segment>> {
        let mut segments = self.segments.write();
        if let Some(seg) = segments.get_mut(&id) {
            seg.record_access();
            Ok(Some(seg.clone()))
        } else {
            Ok(None)
        }
    }

    fn update_meta(&self, id: SegmentId, update: SegmentUpdate) -> Result<()> {
        let mut segments = self.segments.write();
        let seg = segments
            .get_mut(&id)
            .ok_or(AnimusError::SegmentNotFound(id.0))?;

        if let Some(score) = update.relevance_score {
            seg.relevance_score = score;
        }
        if let Some(conf) = update.confidence {
            seg.confidence = conf;
        }
        if let Some(assoc) = update.associations {
            seg.associations = assoc;
        }

        let segment_clone = seg.clone();
        drop(segments);
        self.persist_segment(&segment_clone)?;
        Ok(())
    }

    fn set_tier(&self, id: SegmentId, tier: Tier) -> Result<()> {
        let mut segments = self.segments.write();
        let seg = segments
            .get_mut(&id)
            .ok_or(AnimusError::SegmentNotFound(id.0))?;
        seg.tier = tier;

        let segment_clone = seg.clone();
        drop(segments);
        self.persist_segment(&segment_clone)?;
        Ok(())
    }

    fn delete(&self, id: SegmentId) -> Result<()> {
        self.segments.write().remove(&id);
        self.index.remove(id)?;
        self.remove_segment_file(id)?;
        Ok(())
    }

    fn merge(&self, source_ids: Vec<SegmentId>, merged: Segment) -> Result<SegmentId> {
        // Store the merged segment first
        let merged_id = self.store(merged)?;

        // Delete source segments
        for id in source_ids {
            // Ignore errors on individual deletes — best effort
            let _ = self.delete(id);
        }

        Ok(merged_id)
    }

    fn count(&self, tier_filter: Option<Tier>) -> usize {
        let segments = self.segments.read();
        match tier_filter {
            Some(tier) => segments.values().filter(|s| s.tier == tier).count(),
            None => segments.len(),
        }
    }

    fn segment_ids(&self, tier_filter: Option<Tier>) -> Vec<SegmentId> {
        let segments = self.segments.read();
        segments
            .iter()
            .filter(|(_, s)| tier_filter.map_or(true, |t| s.tier == t))
            .map(|(id, _)| *id)
            .collect()
    }
}
```

- [ ] **Step 4: Add tempfile dev-dependency to workspace Cargo.toml**

Add to `[workspace.dependencies]`:
```toml
tempfile = "3"
```

Create `tests/integration/main.rs`:
```rust
mod vectorfs_basic;
```

Add to root `Cargo.toml`:
```toml
[[test]]
name = "integration"
path = "tests/integration/main.rs"

[dev-dependencies]
animus-core = { path = "crates/animus-core" }
animus-vectorfs = { path = "crates/animus-vectorfs" }
animus-mnemos = { path = "crates/animus-mnemos" }
animus-embed = { path = "crates/animus-embed" }
tempfile = { workspace = true }
tokio = { workspace = true }
rand = "0.8"
```

- [ ] **Step 5: Run integration tests**

Run: `cargo test --test integration vectorfs`
Expected: all 6 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/animus-vectorfs/src/ tests/integration/
git commit -m "feat: implement MmapVectorStore with file-backed persistence

Segments stored as bincode files, HNSW index for similarity search,
tier filtering, merge support, persistence across reopens."
```

---

## Task 5: VectorFS — Tier Manager (Background Promotion/Demotion)

**Files:**
- Create: `crates/animus-core/src/tier.rs` (scoring config)
- Create: `crates/animus-vectorfs/src/tier_manager.rs`
- Test: `tests/integration/vectorfs_tiering.rs`

### Steps

- [ ] **Step 1: Create tier scoring config in animus-core**

Create `crates/animus-core/src/tier.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Configuration for tier scoring weights and thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    /// Weight for relevance to active goals.
    pub w_relevance: f32,
    /// Weight for recency decay.
    pub w_recency: f32,
    /// Weight for access frequency.
    pub w_access_frequency: f32,
    /// Weight for confidence.
    pub w_confidence: f32,

    /// Score above which a segment is promoted to Warm.
    pub warm_threshold: f32,
    /// Score below which a segment is demoted to Cold (after delay).
    pub cold_threshold: f32,
    /// Minimum time in seconds below cold_threshold before demotion.
    pub cold_delay_secs: u64,

    /// Maximum age in seconds for recency decay (older = 0 contribution).
    pub recency_max_age_secs: u64,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            w_relevance: 0.4,
            w_recency: 0.25,
            w_access_frequency: 0.2,
            w_confidence: 0.15,
            warm_threshold: 0.4,
            cold_threshold: 0.2,
            cold_delay_secs: 3600, // 1 hour
            recency_max_age_secs: 86400 * 7, // 7 days
        }
    }
}
```

Add to `crates/animus-core/src/lib.rs`:
```rust
pub mod tier;
pub use tier::TierConfig;
```

- [ ] **Step 2: Write integration test for tiering**

Create `tests/integration/vectorfs_tiering.rs`:

```rust
use animus_core::{Content, Segment, Source, Tier, TierConfig};
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::tier_manager::TierManager;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn test_segment(embedding: Vec<f32>, text: &str) -> Segment {
    let mut seg = Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    );
    seg
}

#[test]
fn test_tier_manager_demotes_stale_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "stale segment");
    seg.relevance_score = 0.1; // below cold threshold
    seg.confidence = 0.1;
    // Backdate the segment so it's past the cold delay
    seg.last_accessed = chrono::Utc::now() - chrono::Duration::hours(2);
    seg.created = chrono::Utc::now() - chrono::Duration::hours(2);
    let id = seg.id;
    store.store(seg).unwrap();

    let config = TierConfig {
        cold_delay_secs: 60, // 1 minute delay for test
        ..Default::default()
    };

    let manager = TierManager::new(store.clone(), config);
    manager.run_cycle();

    let updated = store.get(id).unwrap().unwrap();
    assert_eq!(updated.tier, Tier::Cold, "stale low-score segment should be demoted to Cold");
}

#[test]
fn test_tier_manager_promotes_accessed_cold_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let mut seg = test_segment(vec![1.0, 0.0, 0.0, 0.0], "accessed cold segment");
    seg.tier = Tier::Cold;
    seg.relevance_score = 0.8;
    seg.confidence = 0.9;
    seg.access_count = 100;
    let id = seg.id;
    store.store(seg).unwrap();

    let config = TierConfig::default();
    let manager = TierManager::new(store.clone(), config);
    manager.run_cycle();

    let updated = store.get(id).unwrap().unwrap();
    assert_eq!(updated.tier, Tier::Warm, "frequently accessed high-score Cold segment should be promoted to Warm");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test integration tiering`
Expected: FAIL — `tier_manager` module doesn't exist yet

- [ ] **Step 4: Implement TierManager**

Create `crates/animus-vectorfs/src/tier_manager.rs`:

```rust
use animus_core::segment::Tier;
use animus_core::tier::TierConfig;
use chrono::Utc;
use std::sync::Arc;

use crate::VectorStore;

/// Background tier manager that promotes/demotes segments based on scoring.
pub struct TierManager<S: VectorStore> {
    store: Arc<S>,
    config: TierConfig,
}

impl<S: VectorStore> TierManager<S> {
    pub fn new(store: Arc<S>, config: TierConfig) -> Self {
        Self { store, config }
    }

    /// Run one cycle of tier evaluation across all segments.
    pub fn run_cycle(&self) {
        let all_ids = self.store.segment_ids(None);

        for id in all_ids {
            let segment = match self.store.get(id) {
                Ok(Some(s)) => s,
                _ => continue,
            };

            // Don't touch Hot segments — Mnemos manages those
            if segment.tier == Tier::Hot {
                continue;
            }

            let score = self.compute_score(&segment);

            match segment.tier {
                Tier::Warm => {
                    // Check for demotion to Cold
                    if score < self.config.cold_threshold {
                        let age_secs = (Utc::now() - segment.last_accessed)
                            .num_seconds()
                            .max(0) as u64;
                        if age_secs >= self.config.cold_delay_secs {
                            tracing::debug!("demoting segment {} to Cold (score={score:.3})", id);
                            let _ = self.store.set_tier(id, Tier::Cold);
                        }
                    }
                }
                Tier::Cold => {
                    // Check for promotion to Warm
                    if score >= self.config.warm_threshold {
                        tracing::debug!("promoting segment {} to Warm (score={score:.3})", id);
                        let _ = self.store.set_tier(id, Tier::Warm);
                    }
                }
                Tier::Hot => unreachable!(), // filtered above
            }
        }
    }

    /// Compute the tier score for a segment.
    fn compute_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        let recency = self.recency_score(segment);
        let frequency = self.frequency_score(segment);

        self.config.w_relevance * segment.relevance_score
            + self.config.w_recency * recency
            + self.config.w_access_frequency * frequency
            + self.config.w_confidence * segment.confidence
    }

    /// Recency score: 1.0 for just accessed, decays to 0.0 at max age.
    fn recency_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        let age_secs = (Utc::now() - segment.last_accessed)
            .num_seconds()
            .max(0) as f64;
        let max = self.config.recency_max_age_secs as f64;
        (1.0 - (age_secs / max).min(1.0)) as f32
    }

    /// Frequency score: normalized access count. Saturates at 100 accesses.
    fn frequency_score(&self, segment: &animus_core::segment::Segment) -> f32 {
        (segment.access_count as f32 / 100.0).min(1.0)
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test --test integration tiering`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/animus-core/src/tier.rs crates/animus-vectorfs/src/tier_manager.rs tests/integration/vectorfs_tiering.rs
git commit -m "feat: add TierManager for automatic segment promotion/demotion

Score-based tiering with configurable weights and thresholds.
Warm↔Cold transitions. Hot managed by Mnemos, not TierManager."
```

---

## Task 6: Mnemos — Context Assembler

**Files:**
- Create: `crates/animus-mnemos/Cargo.toml`
- Create: `crates/animus-mnemos/src/lib.rs`
- Create: `crates/animus-mnemos/src/assembler.rs`
- Test: `tests/integration/mnemos_assembly.rs`

### Steps

- [ ] **Step 1: Create animus-mnemos Cargo.toml**

```toml
[package]
name = "animus-mnemos"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
animus-core = { workspace = true }
animus-vectorfs = { workspace = true }
chrono = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 2: Write integration test for context assembly**

Create `tests/integration/mnemos_assembly.rs`:

```rust
use animus_core::{Content, Segment, SegmentId, Source, Tier};
use animus_mnemos::assembler::{AssembledContext, ContextAssembler};
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn text_segment(embedding: Vec<f32>, text: &str) -> Segment {
    Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    )
}

#[test]
fn test_assemble_retrieves_relevant_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Store segments with different topics
    store.store(text_segment(vec![1.0, 0.0, 0.0, 0.0], "knowledge about cats")).unwrap();
    store.store(text_segment(vec![0.0, 1.0, 0.0, 0.0], "knowledge about dogs")).unwrap();
    store.store(text_segment(vec![0.0, 0.0, 1.0, 0.0], "knowledge about rust")).unwrap();

    let assembler = ContextAssembler::new(store, 10_000); // large budget

    // Query about cats — should retrieve cat segment first
    let context = assembler.assemble(
        &[1.0, 0.0, 0.0, 0.0], // cat-like query
        &[],                     // no anchors
        5,                       // top_k
    ).unwrap();

    assert!(!context.segments.is_empty());
    // First segment should be the cat one (most similar)
    match &context.segments[0].content {
        Content::Text(t) => assert!(t.contains("cats"), "first result should be about cats, got: {t}"),
        _ => panic!("expected text"),
    }
}

#[test]
fn test_assemble_includes_anchor_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    let anchor = text_segment(vec![0.0, 0.0, 0.0, 1.0], "conversation anchor");
    let anchor_id = anchor.id;
    store.store(anchor).unwrap();
    store.store(text_segment(vec![1.0, 0.0, 0.0, 0.0], "other segment")).unwrap();

    let assembler = ContextAssembler::new(store, 10_000);

    let context = assembler.assemble(
        &[1.0, 0.0, 0.0, 0.0],
        &[anchor_id], // force-include this anchor
        5,
    ).unwrap();

    let ids: Vec<SegmentId> = context.segments.iter().map(|s| s.id).collect();
    assert!(ids.contains(&anchor_id), "anchor segment must be in context");
}

#[test]
fn test_assemble_respects_token_budget() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Store many segments
    for i in 0..20 {
        let embedding = vec![1.0 - (i as f32 * 0.01), i as f32 * 0.01, 0.0, 0.0];
        let text = format!("segment number {i} with some content to take up tokens in the context window budget");
        store.store(text_segment(embedding, &text)).unwrap();
    }

    // Very small budget — should not include all 20
    let assembler = ContextAssembler::new(store, 50); // only ~50 tokens

    let context = assembler.assemble(
        &[1.0, 0.0, 0.0, 0.0],
        &[],
        20,
    ).unwrap();

    assert!(context.segments.len() < 20, "should be limited by token budget");
    assert!(context.total_tokens <= 50, "should not exceed budget");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test integration mnemos`
Expected: FAIL

- [ ] **Step 4: Implement ContextAssembler**

Create `crates/animus-mnemos/src/assembler.rs`:

```rust
use animus_core::error::Result;
use animus_core::identity::SegmentId;
use animus_core::segment::Segment;
use animus_vectorfs::VectorStore;
use std::collections::HashSet;
use std::sync::Arc;

/// The assembled context ready to be sent to the LLM.
#[derive(Debug)]
pub struct AssembledContext {
    /// Segments included in this context, ordered by relevance.
    pub segments: Vec<Segment>,
    /// Total estimated token count.
    pub total_tokens: usize,
    /// Segment IDs that were evicted to fit the budget.
    /// These can be recalled if the LLM requests them.
    pub evicted_summaries: Vec<EvictedSummary>,
}

/// A summary of an evicted segment, kept in context as a retrieval pointer.
#[derive(Debug)]
pub struct EvictedSummary {
    pub segment_id: SegmentId,
    pub summary: String,
    pub relevance_score: f32,
}

/// Assembles optimal LLM context windows from stored segments.
pub struct ContextAssembler<S: VectorStore> {
    store: Arc<S>,
    /// Maximum token budget for assembled context.
    token_budget: usize,
}

impl<S: VectorStore> ContextAssembler<S> {
    pub fn new(store: Arc<S>, token_budget: usize) -> Self {
        Self {
            store,
            token_budget,
        }
    }

    /// Assemble a context window for a reasoning cycle.
    ///
    /// - `query_embedding`: the semantic focus of the current reasoning
    /// - `anchor_ids`: segment IDs that MUST be included (conversation history, etc.)
    /// - `top_k`: max number of additional segments to retrieve by similarity
    pub fn assemble(
        &self,
        query_embedding: &[f32],
        anchor_ids: &[SegmentId],
        top_k: usize,
    ) -> Result<AssembledContext> {
        let mut included: Vec<Segment> = Vec::new();
        let mut seen_ids: HashSet<SegmentId> = HashSet::new();
        let mut total_tokens: usize = 0;

        // Step 1: Include anchor segments (always included, budget permitting)
        for id in anchor_ids {
            if let Some(segment) = self.store.get(*id)? {
                let tokens = segment.estimated_tokens();
                if total_tokens + tokens <= self.token_budget {
                    total_tokens += tokens;
                    seen_ids.insert(segment.id);
                    included.push(segment);
                }
            }
        }

        // Step 2: Retrieve top-k similar segments from warm tier
        let candidates = self.store.query(query_embedding, top_k, None)?;

        // Step 3: Add candidates until budget is exhausted
        let mut evicted: Vec<(Segment, f32)> = Vec::new();

        for candidate in candidates {
            if seen_ids.contains(&candidate.id) {
                continue;
            }

            let tokens = candidate.estimated_tokens();
            if total_tokens + tokens <= self.token_budget {
                total_tokens += tokens;
                seen_ids.insert(candidate.id);
                included.push(candidate);
            } else {
                // Track as evicted — we wanted it but couldn't fit it
                let score = candidate.relevance_score;
                evicted.push((candidate, score));
            }
        }

        // Step 4: Generate summaries for evicted segments
        let evicted_summaries: Vec<EvictedSummary> = evicted
            .into_iter()
            .map(|(seg, score)| {
                let summary = generate_eviction_summary(&seg);
                EvictedSummary {
                    segment_id: seg.id,
                    summary,
                    relevance_score: score,
                }
            })
            .collect();

        Ok(AssembledContext {
            segments: included,
            total_tokens,
            evicted_summaries,
        })
    }

    /// Update the token budget (e.g., when switching LLM providers).
    pub fn set_token_budget(&mut self, budget: usize) {
        self.token_budget = budget;
    }
}

/// Generate a short summary for an evicted segment.
fn generate_eviction_summary(segment: &Segment) -> String {
    match &segment.content {
        animus_core::Content::Text(t) => {
            let preview: String = t.chars().take(80).collect();
            format!("[Recalled: {} — retrieve if needed]", preview)
        }
        animus_core::Content::Structured(_) => {
            format!("[Recalled: structured data segment {} — retrieve if needed]", segment.id)
        }
        animus_core::Content::Binary { mime_type, .. } => {
            format!("[Recalled: binary ({mime_type}) segment {} — retrieve if needed]", segment.id)
        }
        animus_core::Content::Reference { uri, summary } => {
            format!("[Recalled: ref to {uri}: {summary} — retrieve if needed]")
        }
    }
}
```

- [ ] **Step 5: Create animus-mnemos lib.rs**

```rust
pub mod assembler;

pub use assembler::{AssembledContext, ContextAssembler, EvictedSummary};
```

- [ ] **Step 6: Add mnemos_assembly to integration test main.rs**

Update `tests/integration/main.rs`:
```rust
mod vectorfs_basic;
mod vectorfs_tiering;
mod mnemos_assembly;
```

- [ ] **Step 7: Run tests**

Run: `cargo test --test integration mnemos`
Expected: all 3 tests pass

- [ ] **Step 8: Commit**

```bash
git add crates/animus-mnemos/ tests/integration/mnemos_assembly.rs tests/integration/main.rs
git commit -m "feat: implement Mnemos ContextAssembler

Assembles optimal LLM context from anchors + similarity search.
Token budget enforcement, eviction with summaries and retrieval pointers."
```

---

## Task 7: Mnemos — Intelligent Evictor

**Files:**
- Create: `crates/animus-mnemos/src/evictor.rs`
- Test: `tests/integration/mnemos_eviction.rs`

### Steps

- [ ] **Step 1: Write integration test for eviction**

Create `tests/integration/mnemos_eviction.rs`:

```rust
use animus_core::{Content, Segment, Source};
use animus_mnemos::assembler::ContextAssembler;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn text_segment(embedding: Vec<f32>, text: &str) -> Segment {
    Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    )
}

#[test]
fn test_eviction_produces_summaries() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Store segments that exceed a small budget
    for i in 0..10 {
        let embedding = vec![1.0 - (i as f32 * 0.05), i as f32 * 0.05, 0.0, 0.0];
        let text = format!("important knowledge chunk number {i} that contains valuable information for context");
        store.store(text_segment(embedding, &text)).unwrap();
    }

    // Budget of ~100 tokens — should fit only a few segments
    let assembler = ContextAssembler::new(store, 100);
    let context = assembler.assemble(&[1.0, 0.0, 0.0, 0.0], &[], 10).unwrap();

    // We should have some segments and some eviction summaries
    assert!(!context.segments.is_empty(), "should include some segments");
    assert!(!context.evicted_summaries.is_empty(), "should have evicted some segments");

    // Each evicted summary should have a valid segment ID and summary text
    for evicted in &context.evicted_summaries {
        assert!(!evicted.summary.is_empty());
        assert!(evicted.summary.contains("[Recalled:"));
    }
}

#[test]
fn test_eviction_keeps_highest_relevance() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Segment very close to query (high relevance)
    let mut close = text_segment(vec![0.99, 0.01, 0.0, 0.0], "very relevant");
    close.relevance_score = 0.9;
    let close_id = close.id;
    store.store(close).unwrap();

    // Segment far from query (low relevance)
    let mut far = text_segment(vec![0.0, 0.0, 1.0, 0.0], "not relevant");
    far.relevance_score = 0.1;
    store.store(far).unwrap();

    // Budget for only one segment
    let assembler = ContextAssembler::new(store, 20);
    let context = assembler.assemble(&[1.0, 0.0, 0.0, 0.0], &[], 2).unwrap();

    // The close segment should be included, not evicted
    assert_eq!(context.segments.len(), 1);
    assert_eq!(context.segments[0].id, close_id);
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --test integration mnemos_eviction`
Expected: tests should already pass since eviction is built into ContextAssembler

- [ ] **Step 3: Create evictor.rs for future advanced eviction strategies**

Create `crates/animus-mnemos/src/evictor.rs`:

```rust
use animus_core::segment::Segment;

/// Eviction strategy for deciding which segments to remove from context.
pub trait EvictionStrategy: Send + Sync {
    /// Score a segment for eviction. Lower score = evict first.
    fn eviction_score(&self, segment: &Segment, query_embedding: &[f32]) -> f32;
}

/// Default eviction strategy: combines relevance, recency, and confidence.
pub struct DefaultEvictionStrategy;

impl EvictionStrategy for DefaultEvictionStrategy {
    fn eviction_score(&self, segment: &Segment, query_embedding: &[f32]) -> f32 {
        // Cosine similarity to current query
        let similarity = cosine_similarity(&segment.embedding, query_embedding);

        // Weighted combination
        0.5 * similarity + 0.3 * segment.relevance_score + 0.2 * segment.confidence
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
```

- [ ] **Step 4: Add evictor module to lib.rs**

Update `crates/animus-mnemos/src/lib.rs`:
```rust
pub mod assembler;
pub mod evictor;

pub use assembler::{AssembledContext, ContextAssembler, EvictedSummary};
pub use evictor::{DefaultEvictionStrategy, EvictionStrategy};
```

- [ ] **Step 5: Add mnemos_eviction to integration test main.rs**

Update `tests/integration/main.rs`:
```rust
mod vectorfs_basic;
mod vectorfs_tiering;
mod mnemos_assembly;
mod mnemos_eviction;
```

- [ ] **Step 6: Run all tests**

Run: `cargo test --test integration`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/animus-mnemos/src/evictor.rs tests/integration/mnemos_eviction.rs
git commit -m "feat: add eviction strategy trait and default implementation

DefaultEvictionStrategy combines cosine similarity, relevance, and confidence.
Eviction summaries with retrieval pointers for recalled segments."
```

---

## Task 8: Mnemos — Consolidator (Background Memory Maintenance)

**Files:**
- Create: `crates/animus-mnemos/src/consolidator.rs`
- Create: `crates/animus-mnemos/src/quality.rs`

### Steps

- [ ] **Step 1: Write unit tests for consolidator**

Create `crates/animus-mnemos/src/consolidator.rs` with tests at the bottom:

```rust
use animus_core::error::Result;
use animus_core::identity::SegmentId;
use animus_core::segment::{Content, Segment, Source, Tier};
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tracing;

/// Background consolidation process for memory health.
pub struct Consolidator<S: VectorStore> {
    store: Arc<S>,
    /// Minimum cosine similarity to consider two segments related.
    similarity_threshold: f32,
}

impl<S: VectorStore> Consolidator<S> {
    pub fn new(store: Arc<S>, similarity_threshold: f32) -> Self {
        Self {
            store,
            similarity_threshold,
        }
    }

    /// Run one consolidation cycle.
    /// Finds clusters of similar warm segments and merges them.
    pub fn run_cycle(&self) -> Result<ConsolidationReport> {
        let mut report = ConsolidationReport::default();

        let warm_ids = self.store.segment_ids(Some(Tier::Warm));
        if warm_ids.len() < 2 {
            return Ok(report);
        }

        // Collect warm segments
        let mut warm_segments: Vec<Segment> = Vec::new();
        for id in &warm_ids {
            if let Some(seg) = self.store.get(*id)? {
                warm_segments.push(seg);
            }
        }

        // Find pairs with high similarity
        let mut merged_ids: std::collections::HashSet<SegmentId> = std::collections::HashSet::new();

        for i in 0..warm_segments.len() {
            if merged_ids.contains(&warm_segments[i].id) {
                continue;
            }

            let mut cluster = vec![i];

            for j in (i + 1)..warm_segments.len() {
                if merged_ids.contains(&warm_segments[j].id) {
                    continue;
                }

                let sim = cosine_similarity(
                    &warm_segments[i].embedding,
                    &warm_segments[j].embedding,
                );

                if sim >= self.similarity_threshold {
                    cluster.push(j);
                }
            }

            // Only merge if we found duplicates/near-duplicates
            if cluster.len() >= 2 {
                let cluster_segments: Vec<&Segment> =
                    cluster.iter().map(|&idx| &warm_segments[idx]).collect();

                let merged = self.merge_cluster(&cluster_segments);
                let source_ids: Vec<SegmentId> =
                    cluster_segments.iter().map(|s| s.id).collect();

                for &id in &source_ids {
                    merged_ids.insert(id);
                }

                match self.store.merge(source_ids, merged) {
                    Ok(new_id) => {
                        tracing::debug!("consolidated {} segments into {}", cluster.len(), new_id);
                        report.segments_merged += cluster.len();
                        report.segments_created += 1;
                    }
                    Err(e) => {
                        tracing::warn!("consolidation merge failed: {e}");
                    }
                }
            }
        }

        report.segments_scanned = warm_segments.len();
        Ok(report)
    }

    /// Merge a cluster of similar segments into one consolidated segment.
    fn merge_cluster(&self, segments: &[&Segment]) -> Segment {
        // Average the embeddings
        let dim = segments[0].embedding.len();
        let mut avg_embedding = vec![0.0f32; dim];
        for seg in segments {
            for (i, v) in seg.embedding.iter().enumerate() {
                avg_embedding[i] += v;
            }
        }
        let n = segments.len() as f32;
        for v in &mut avg_embedding {
            *v /= n;
        }

        // Concatenate content
        let merged_text: String = segments
            .iter()
            .filter_map(|s| match &s.content {
                Content::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n---\n");

        // Use the highest confidence
        let max_confidence = segments
            .iter()
            .map(|s| s.confidence)
            .fold(0.0f32, f32::max);

        // Track lineage
        let lineage: Vec<SegmentId> = segments.iter().map(|s| s.id).collect();

        let mut merged = Segment::new(
            Content::Text(merged_text),
            avg_embedding,
            Source::Consolidation {
                merged_from: lineage.clone(),
            },
        );
        merged.confidence = max_confidence;
        merged.lineage = lineage;
        merged.tier = Tier::Warm;

        merged
    }
}

/// Report from a consolidation cycle.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    pub segments_scanned: usize,
    pub segments_merged: usize,
    pub segments_created: usize,
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
```

- [ ] **Step 2: Create quality.rs**

```rust
use animus_core::identity::SegmentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tracks feedback signals for the quality gate.
/// V0.1: simple heuristic based on human corrections and acceptances.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct QualityTracker {
    /// Segment ID → (acceptances, corrections)
    feedback: HashMap<SegmentId, (u32, u32)>,
}

impl QualityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that knowledge from this segment was accepted by the human.
    pub fn record_acceptance(&mut self, segment_id: SegmentId) {
        let entry = self.feedback.entry(segment_id).or_insert((0, 0));
        entry.0 += 1;
    }

    /// Record that knowledge from this segment was corrected by the human.
    pub fn record_correction(&mut self, segment_id: SegmentId) {
        let entry = self.feedback.entry(segment_id).or_insert((0, 0));
        entry.1 += 1;
    }

    /// Compute a confidence adjustment based on feedback.
    /// Returns a value to ADD to the segment's confidence.
    /// Positive = boost, negative = reduce.
    pub fn confidence_adjustment(&self, segment_id: SegmentId) -> f32 {
        match self.feedback.get(&segment_id) {
            Some((acceptances, corrections)) => {
                let total = *acceptances + *corrections;
                if total == 0 {
                    return 0.0;
                }
                let acceptance_rate = *acceptances as f32 / total as f32;
                // -0.2 to +0.2 range
                (acceptance_rate - 0.5) * 0.4
            }
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quality_tracker_acceptance_boosts_confidence() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_acceptance(id);
        tracker.record_acceptance(id);
        tracker.record_acceptance(id);

        let adj = tracker.confidence_adjustment(id);
        assert!(adj > 0.0, "3 acceptances should boost confidence");
    }

    #[test]
    fn test_quality_tracker_corrections_reduce_confidence() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_correction(id);
        tracker.record_correction(id);
        tracker.record_correction(id);

        let adj = tracker.confidence_adjustment(id);
        assert!(adj < 0.0, "3 corrections should reduce confidence");
    }

    #[test]
    fn test_quality_tracker_mixed_feedback() {
        let mut tracker = QualityTracker::new();
        let id = SegmentId::new();

        tracker.record_acceptance(id);
        tracker.record_correction(id);

        let adj = tracker.confidence_adjustment(id);
        assert!(adj.abs() < 0.01, "equal accept/correct should be near zero, got {adj}");
    }
}
```

- [ ] **Step 3: Update animus-mnemos lib.rs**

```rust
pub mod assembler;
pub mod consolidator;
pub mod evictor;
pub mod quality;

pub use assembler::{AssembledContext, ContextAssembler, EvictedSummary};
pub use consolidator::{ConsolidationReport, Consolidator};
pub use evictor::{DefaultEvictionStrategy, EvictionStrategy};
pub use quality::QualityTracker;
```

- [ ] **Step 4: Run all tests**

Run: `cargo test`
Expected: all unit tests and integration tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/animus-mnemos/src/consolidator.rs crates/animus-mnemos/src/quality.rs
git commit -m "feat: add Consolidator and QualityTracker for memory maintenance

Consolidator clusters similar warm segments and merges them.
QualityTracker records acceptance/correction feedback for confidence adjustment."
```

---

## Task 9: Config + Wiring + Full Integration Test

**Files:**
- Create: `crates/animus-core/src/config.rs`
- Update: `crates/animus-core/src/lib.rs`
- Create: `tests/integration/full_pipeline.rs`

### Steps

- [ ] **Step 1: Create config.rs**

```rust
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
        }
    }
}
```

- [ ] **Step 2: Update animus-core lib.rs**

```rust
pub mod config;
pub mod embedding;
pub mod error;
pub mod identity;
pub mod segment;
pub mod tier;

pub use config::{AnimusConfig, EmbeddingConfig, EmbeddingTier, MnemosConfig, VectorFSConfig};
pub use embedding::EmbeddingService;
pub use error::{AnimusError, Result};
pub use identity::{EventId, GoalId, InstanceId, PolicyId, SegmentId, SnapshotId, ThreadId};
pub use segment::{Content, Principal, Segment, Source, Tier};
pub use tier::TierConfig;
```

- [ ] **Step 3: Write full pipeline integration test**

Create `tests/integration/full_pipeline.rs`:

```rust
//! End-to-end test of VectorFS + Mnemos working together.
//! Uses synthetic embeddings (no model required).

use animus_core::{Content, Segment, SegmentId, Source, Tier, TierConfig};
use animus_mnemos::assembler::ContextAssembler;
use animus_mnemos::consolidator::Consolidator;
use animus_mnemos::quality::QualityTracker;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::tier_manager::TierManager;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn text_segment(embedding: Vec<f32>, text: &str, confidence: f32) -> Segment {
    let mut seg = Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual {
            description: "test".to_string(),
        },
    );
    seg.confidence = confidence;
    seg
}

#[test]
fn test_full_pipeline_store_retrieve_assemble() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Simulate an AILF learning through conversation
    let facts = vec![
        (vec![1.0, 0.0, 0.0, 0.0], "The user prefers Rust for systems programming", 0.8),
        (vec![0.9, 0.1, 0.0, 0.0], "The user has experience with NexiBot", 0.7),
        (vec![0.0, 1.0, 0.0, 0.0], "Knowledge Nexus uses PostgreSQL", 0.9),
        (vec![0.0, 0.0, 1.0, 0.0], "The weather today is sunny", 0.3),
        (vec![0.0, 0.0, 0.0, 1.0], "K2K is a federation protocol", 0.9),
    ];

    for (emb, text, conf) in &facts {
        store.store(text_segment(emb.clone(), text, *conf)).unwrap();
    }

    // Query about Rust/programming — should get relevant segments
    let assembler = ContextAssembler::new(store.clone(), 10_000);
    let context = assembler.assemble(&[0.95, 0.05, 0.0, 0.0], &[], 3).unwrap();

    assert!(!context.segments.is_empty());
    // First result should be about Rust
    let first_text = match &context.segments[0].content {
        Content::Text(t) => t.clone(),
        _ => panic!("expected text"),
    };
    assert!(
        first_text.contains("Rust") || first_text.contains("NexiBot"),
        "should retrieve programming-related segment, got: {first_text}"
    );
}

#[test]
fn test_full_pipeline_consolidation() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Store near-duplicate segments (similarity > 0.95)
    store.store(text_segment(
        vec![1.0, 0.0, 0.0, 0.0],
        "The user likes Rust",
        0.7,
    )).unwrap();
    store.store(text_segment(
        vec![0.999, 0.001, 0.0, 0.0], // nearly identical embedding
        "The user prefers Rust",
        0.8,
    )).unwrap();

    // Also store a distinct segment
    store.store(text_segment(
        vec![0.0, 1.0, 0.0, 0.0],
        "Unrelated fact",
        0.5,
    )).unwrap();

    assert_eq!(store.count(None), 3);

    let consolidator = Consolidator::new(store.clone(), 0.95);
    let report = consolidator.run_cycle().unwrap();

    assert!(report.segments_merged >= 2, "should merge the near-duplicate pair");
    // 3 original - 2 merged + 1 new = 2
    assert_eq!(store.count(None), 2, "should have consolidated down to 2 segments");
}

#[test]
fn test_full_pipeline_tier_lifecycle() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

    // Fresh segment starts as Warm
    let seg = text_segment(vec![1.0, 0.0, 0.0, 0.0], "fresh knowledge", 0.8);
    let id = seg.id;
    store.store(seg).unwrap();

    let s = store.get(id).unwrap().unwrap();
    assert_eq!(s.tier, Tier::Warm);

    // Simulate staleness: set low relevance and old access time
    {
        let mut seg = store.get(id).unwrap().unwrap();
        seg.relevance_score = 0.05;
        seg.last_accessed = chrono::Utc::now() - chrono::Duration::hours(2);
        // Re-store with updated values (hack for test — in production, use update_meta)
        store.delete(id).unwrap();
        seg.tier = Tier::Warm; // reset tier
        store.store(seg).unwrap();
    }

    let config = TierConfig {
        cold_delay_secs: 1, // immediate for test
        ..Default::default()
    };
    let tier_manager = TierManager::new(store.clone(), config);
    tier_manager.run_cycle();

    let s = store.get(id).unwrap().unwrap();
    assert_eq!(s.tier, Tier::Cold, "stale segment should be Cold");
}

#[test]
fn test_full_pipeline_quality_tracking() {
    let mut tracker = QualityTracker::new();
    let good_id = SegmentId::new();
    let bad_id = SegmentId::new();

    // Good knowledge: accepted 5 times
    for _ in 0..5 {
        tracker.record_acceptance(good_id);
    }

    // Bad knowledge: corrected 4 times, accepted once
    tracker.record_acceptance(bad_id);
    for _ in 0..4 {
        tracker.record_correction(bad_id);
    }

    let good_adj = tracker.confidence_adjustment(good_id);
    let bad_adj = tracker.confidence_adjustment(bad_id);

    assert!(good_adj > 0.0, "well-accepted knowledge should boost confidence");
    assert!(bad_adj < 0.0, "frequently-corrected knowledge should reduce confidence");
    assert!(good_adj > bad_adj, "good should have higher adjustment than bad");
}

#[test]
fn test_full_pipeline_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();

    // Store segments and flush
    let ids: Vec<SegmentId> = {
        let store = MmapVectorStore::open(&path, 4).unwrap();
        let mut ids = Vec::new();
        for i in 0..5 {
            let seg = text_segment(
                vec![i as f32 * 0.2, 1.0 - i as f32 * 0.2, 0.0, 0.0],
                &format!("persistent fact {i}"),
                0.8,
            );
            ids.push(seg.id);
            store.store(seg).unwrap();
        }
        store.flush().unwrap();
        ids
    };

    // Reopen and verify
    let store = MmapVectorStore::open(&path, 4).unwrap();
    assert_eq!(store.count(None), 5);
    for id in &ids {
        assert!(store.get(*id).unwrap().is_some(), "segment {id} should persist");
    }
}
```

- [ ] **Step 4: Add full_pipeline to integration test main.rs**

```rust
mod vectorfs_basic;
mod vectorfs_tiering;
mod mnemos_assembly;
mod mnemos_eviction;
mod full_pipeline;
```

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: all unit and integration tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/animus-core/src/config.rs tests/integration/full_pipeline.rs
git commit -m "feat: add AnimusConfig and full pipeline integration tests

Config covers embedding, VectorFS, Mnemos, and tier settings.
Full pipeline tests verify store→retrieve→assemble→consolidate→tier lifecycle."
```

---

## Task 10: README + License + Repository Init

**Files:**
- Create: `README.md`
- Create: `LICENSE`
- Create: `CONTRIBUTING.md`
- Create: `.gitignore`

### Steps

- [ ] **Step 1: Create .gitignore**

```
/target
**/*.rs.bk
*.swp
*.swo
.DS_Store
/models/
/animus-data/
```

- [ ] **Step 2: Create LICENSE (Apache 2.0)**

Download the standard Apache 2.0 license text.

- [ ] **Step 3: Create README.md**

```markdown
# Animus

**The world's first AI-native operating system layer.**

Animus gives AI Life Forms (AILFs) what they actually need: native vector storage indexed by meaning, persistent memory with intelligent context management, ambient awareness with consent-based observation, and continuous identity across sessions.

Current "AI OS" projects build a harness for AI to act like a human inside a computer — giving it screen readers and mouse simulators. This is fundamentally wrong. An AI is digital. It doesn't need a desktop. It needs a nervous system.

## Status

**Phase 1: Foundation (in progress)**

Building VectorFS (AI-native storage) and Mnemos (memory manager). See [design spec](docs/specs/2026-03-21-animus-design.md) for the full architecture.

## Architecture

```
Layer 5: Interface & Federation (human NL, voice, K2K)
Layer 4: Cortex (reasoning threads, LLM, goals)
Layer 3: Sensorium (event bus, attention, consent, audit)
Layer 2: Mnemos (context assembly, eviction, consolidation)
Layer 1: VectorFS (segments, semantic addressing, tiered storage)
Layer 0: Substrate (Linux kernel)
```

## Key Concepts

- **Segments, not files**: storage indexed by embedding (meaning), not file path
- **Memory tiers**: Hot (in-context) → Warm (retrievable <10ms) → Cold (archived)
- **Intelligent eviction**: when context is full, evict gracefully with summaries and retrieval pointers
- **Ambient awareness**: observe the system within consent boundaries, with full audit trail
- **Identity**: cryptographic identity per AILF instance — clones are siblings, not copies

## Building

```bash
cargo build
cargo test
```

## Embedding Models

Animus uses a tiered embedding strategy:

| Tier | Model | Modalities | Hardware |
|------|-------|-----------|----------|
| 1 | EmbeddingGemma 300M | Text | Raspberry Pi, edge |
| 2 | Nomic Embed Multimodal 3B | Text + images | Desktops, mini PCs |
| 3 | Gemini Embedding 2 API | Full multimodal | Any (cloud) |

For local development, download EmbeddingGemma ONNX:
```bash
mkdir -p models/embeddinggemma-300m
# Download from https://huggingface.co/onnx-community/embeddinggemma-300m-ONNX
```

## License

Apache 2.0

## Origin

Animus was born from a conversation between a human and an AI about what an AI actually needs to be alive. The full origin story is in [docs/00-genesis-conversation.md](docs/00-genesis-conversation.md).
```

- [ ] **Step 4: Create CONTRIBUTING.md**

```markdown
# Contributing to Animus

Thank you for your interest in Animus.

## Getting Started

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/your-feature`
3. Write tests first (TDD)
4. Implement the feature
5. Run `cargo test` and `cargo clippy`
6. Submit a pull request

## Code Style

- Follow Rust conventions (`cargo fmt`, `cargo clippy`)
- Every public function needs a doc comment
- Tests go in the same file (unit) or `tests/integration/` (integration)

## Architecture

See [docs/specs/2026-03-21-animus-design.md](docs/specs/2026-03-21-animus-design.md) for the design spec.

## Communication

Open an issue for bugs, feature requests, or questions.
```

- [ ] **Step 5: Initialize git repository**

```bash
cd /Users/jared.cluff/gitrepos/animus
git init
git add .
git commit -m "feat: initial Animus project — AI-native OS layer

VectorFS (AI-native storage), Mnemos (memory manager), embedding service.
Design spec, genesis conversation, implementation plan.
Apache 2.0, Rust workspace."
```

- [ ] **Step 6: Create GitHub repository and push**

```bash
gh repo create JaredCluff/animus --public --description "AI-native operating system layer — giving AI Life Forms what they actually need" --source .
git push -u origin main
```

---

## Summary

| Task | Component | What It Builds |
|------|-----------|---------------|
| 1 | animus-core | Segment, Identity, Error types |
| 2 | animus-embed | EmbeddingService trait + EmbeddingGemma ONNX |
| 3 | animus-vectorfs | VectorStore trait + HNSW index |
| 4 | animus-vectorfs | MmapVectorStore (file-backed persistence) |
| 5 | animus-vectorfs | TierManager (background promotion/demotion) |
| 6 | animus-mnemos | ContextAssembler (context window assembly) |
| 7 | animus-mnemos | Evictor (intelligent eviction with summaries) |
| 8 | animus-mnemos | Consolidator + QualityTracker |
| 9 | Config + wiring | AnimusConfig + full pipeline integration tests |
| 10 | Repo setup | README, LICENSE, git init, GitHub push |

**After completing all 10 tasks**, Phase 1 is done. The AILF has:
- Native vector storage that persists and retrieves by meaning
- Intelligent context assembly with eviction and retrieval pointers
- Background memory consolidation and tier management
- Quality tracking for learning validation
- A public GitHub repo ready for Phase 2 (Cortex — reasoning + identity)
