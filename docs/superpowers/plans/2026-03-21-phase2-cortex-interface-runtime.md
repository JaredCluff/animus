# Phase 2: Cortex + Interface + Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a thinking AILF — single-threaded reasoning connected to an LLM, terminal interface for human interaction, persistent identity, simple goal tracking, and a runtime binary that orchestrates all layers into a living system.

**Architecture:** Four new crates: `animus-cortex` (reasoning thread, LLM abstraction, Telos goal queue), `animus-interface` (terminal I/O with async readline), `animus-runtime` (main binary wiring all layers together), plus `AnimusIdentity` in `animus-core`. The Cortex uses Mnemos to assemble context before each LLM call, stores conversation turns as Segments in VectorFS, and tracks simple goals. The runtime manages lifecycle (Birth → Living → Sleeping).

**Tech Stack:** Rust, tokio async runtime, reqwest (Anthropic API), rustyline (terminal), ed25519-dalek (identity keypair), serde/bincode (persistence)

---

## File Structure

### New Crate: `animus-cortex`
```
crates/animus-cortex/
  Cargo.toml
  src/
    lib.rs           — module declarations, public exports
    thread.rs        — ReasoningThread: isolated conversation context
    telos.rs         — Goal struct, GoalManager: simple goal queue
    llm/
      mod.rs         — ReasoningEngine trait
      anthropic.rs   — Anthropic Claude API provider
```

### New Crate: `animus-interface`
```
crates/animus-interface/
  Cargo.toml
  src/
    lib.rs           — module declarations
    terminal.rs      — TerminalInterface: async readline + display
```

### New Crate: `animus-runtime`
```
crates/animus-runtime/
  Cargo.toml
  src/
    main.rs          — main binary, lifecycle management, wiring
```

### Modifications to `animus-core`
```
crates/animus-core/src/
  identity.rs        — ADD AnimusIdentity struct, Ed25519 keypair, persistence
  error.rs           — ADD new error variants (Llm, Identity, Interface, Goal)
  config.rs          — ADD CortexConfig, InterfaceConfig
  lib.rs             — ADD new exports
```

### Integration Tests
```
crates/animus-tests/tests/integration/
  main.rs            — ADD mod declarations for new test modules
  cortex_reasoning.rs — Cortex reasoning with mock LLM
  telos_goals.rs     — Goal creation, tracking, completion
```

### Known V0.1 Limitation
The runtime uses `SyntheticEmbedding` (hash-based) instead of real semantic embeddings (EmbeddingGemma/Nomic). This means similarity retrieval is deterministic but not truly semantic. Real embedding integration requires ONNX runtime setup and model downloads, which is tracked as a followup task. The architecture supports swapping via the `EmbeddingService` trait.

---

### Task 1: Extend animus-core with Identity, error variants, and config

**Files:**
- Modify: `crates/animus-core/src/identity.rs`
- Modify: `crates/animus-core/src/error.rs`
- Modify: `crates/animus-core/src/config.rs`
- Modify: `crates/animus-core/src/lib.rs`
- Modify: `crates/animus-core/Cargo.toml`

- [ ] **Step 1: Add ed25519-dalek dependency to workspace and animus-core**

In workspace `Cargo.toml`, add to `[workspace.dependencies]`:
```toml
ed25519-dalek = { version = "2", features = ["rand_core", "serde"] }
```

In `crates/animus-core/Cargo.toml`, add:
```toml
ed25519-dalek = { workspace = true }
rand = { workspace = true }
```

- [ ] **Step 2: Add GoalId impls and AnimusIdentity to identity.rs**

Add missing impls for `InstanceId`, `ThreadId`, and `GoalId` (consistent with existing `SegmentId` pattern in this file):

```rust
impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl GoalId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for GoalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for GoalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

Then add `AnimusIdentity`:

```rust
use ed25519_dalek::{SigningKey, VerifyingKey};

/// Persistent identity for an AILF instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimusIdentity {
    /// Ed25519 signing key (private). Serialized as bytes.
    #[serde(with = "signing_key_serde")]
    pub signing_key: SigningKey,
    /// Unique instance ID, immutable after birth.
    pub instance_id: InstanceId,
    /// Parent instance if this AILF was forked/cloned.
    pub parent_id: Option<InstanceId>,
    /// Timestamp of creation.
    pub born: chrono::DateTime<chrono::Utc>,
    /// Generation: 0 = original, 1 = first fork, etc.
    pub generation: u32,
    /// Which LLM model powers reasoning.
    pub base_model: String,
}

impl AnimusIdentity {
    /// Generate a new identity for a fresh AILF.
    pub fn generate(base_model: String) -> Self {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        Self {
            signing_key,
            instance_id: InstanceId::new(),
            parent_id: None,
            born: chrono::Utc::now(),
            generation: 0,
            base_model,
        }
    }

    /// Get the public verifying key.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Load identity from a file, or generate and save if not found.
    pub fn load_or_generate(path: &std::path::Path, base_model: &str) -> crate::Result<Self> {
        if path.exists() {
            let data = std::fs::read(path)?;
            let identity: Self = bincode::deserialize(&data)
                .map_err(|e| crate::AnimusError::Identity(format!("failed to load identity: {e}")))?;
            Ok(identity)
        } else {
            let identity = Self::generate(base_model.to_string());
            let data = bincode::serialize(&identity)
                .map_err(|e| crate::AnimusError::Identity(format!("failed to serialize identity: {e}")))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, &data)?;
            Ok(identity)
        }
    }
}

/// Serde helper for SigningKey (serialize as 32-byte array).
mod signing_key_serde {
    use ed25519_dalek::SigningKey;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(key: &SigningKey, s: S) -> Result<S::Ok, S::Error> {
        key.to_bytes().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SigningKey, D::Error> {
        let bytes = <[u8; 32]>::deserialize(d)?;
        Ok(SigningKey::from_bytes(&bytes))
    }
}
```

- [ ] **Step 3: Add new error variants to error.rs**

Add these variants to `AnimusError`:
```rust
    #[error("LLM error: {0}")]
    Llm(String),

    #[error("identity error: {0}")]
    Identity(String),

    #[error("interface error: {0}")]
    Interface(String),

    #[error("goal error: {0}")]
    Goal(String),
```

- [ ] **Step 4: Add CortexConfig and InterfaceConfig to config.rs**

```rust
/// Configuration for the Cortex reasoning layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexConfig {
    /// LLM provider name (e.g., "anthropic").
    pub llm_provider: String,
    /// Model identifier (e.g., "claude-sonnet-4-20250514").
    pub model_id: String,
    /// API key for the LLM provider. Read from env if empty.
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
```

Add fields to `AnimusConfig`:
```rust
pub cortex: CortexConfig,
pub interface: InterfaceConfig,
```

- [ ] **Step 5: Update lib.rs exports**

Add to the pub use block:
```rust
pub use config::{CortexConfig, InterfaceConfig};
pub use identity::AnimusIdentity;
```

- [ ] **Step 6: Build and verify compilation**

Run: `cargo build --all`
Expected: Success

- [ ] **Step 7: Commit**

```bash
git add crates/animus-core/ Cargo.toml
git commit -m "feat: add AnimusIdentity, CortexConfig, new error variants"
```

---

### Task 2: Create animus-cortex crate — ReasoningEngine trait + mock

**Files:**
- Create: `crates/animus-cortex/Cargo.toml`
- Create: `crates/animus-cortex/src/lib.rs`
- Create: `crates/animus-cortex/src/llm/mod.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "animus-cortex"
version = "0.1.0"
edition = "2021"

[dependencies]
animus-core = { path = "../animus-core" }
animus-vectorfs = { path = "../animus-vectorfs" }
animus-mnemos = { path = "../animus-mnemos" }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
reqwest = { version = "0.12", features = ["json"] }
```

- [ ] **Step 2: Add to workspace Cargo.toml**

Add `"crates/animus-cortex"` to `[workspace] members`.

Add to `[workspace.dependencies]`:
```toml
reqwest = { version = "0.12", features = ["json"] }
```

- [ ] **Step 3: Create llm/mod.rs — ReasoningEngine trait + MockEngine**

```rust
use animus_core::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single turn in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub content: String,
}

/// Role in a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

/// Output from a reasoning call.
#[derive(Debug, Clone)]
pub struct ReasoningOutput {
    pub content: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
}

/// Trait abstracting LLM providers.
#[async_trait]
pub trait ReasoningEngine: Send + Sync {
    /// Send a conversation and get a response.
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
    ) -> Result<ReasoningOutput>;

    /// Get the model's context window size in tokens.
    fn context_limit(&self) -> usize;

    /// Get the model identifier.
    fn model_name(&self) -> &str;
}

/// Mock reasoning engine for testing.
pub struct MockEngine {
    response: String,
    context_limit: usize,
}

impl MockEngine {
    pub fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            context_limit: 8192,
        }
    }

    pub fn with_context_limit(mut self, limit: usize) -> Self {
        self.context_limit = limit;
        self
    }
}

#[async_trait]
impl ReasoningEngine for MockEngine {
    async fn reason(
        &self,
        _system: &str,
        _messages: &[Turn],
    ) -> Result<ReasoningOutput> {
        Ok(ReasoningOutput {
            content: self.response.clone(),
            input_tokens: 100,
            output_tokens: self.response.len() / 4,
        })
    }

    fn context_limit(&self) -> usize {
        self.context_limit
    }

    fn model_name(&self) -> &str {
        "mock-engine"
    }
}
```

- [ ] **Step 4: Create lib.rs**

```rust
pub mod llm;

pub use llm::{MockEngine, ReasoningEngine, ReasoningOutput, Role, Turn};
```

- [ ] **Step 5: Build and verify**

Run: `cargo build --all`
Expected: Success

- [ ] **Step 6: Commit**

```bash
git add crates/animus-cortex/ Cargo.toml
git commit -m "feat: add animus-cortex crate with ReasoningEngine trait and MockEngine"
```

---

### Task 3: Anthropic Claude LLM provider

**Files:**
- Create: `crates/animus-cortex/src/llm/anthropic.rs`
- Modify: `crates/animus-cortex/src/llm/mod.rs`

- [ ] **Step 1: Create anthropic.rs**

```rust
use animus_core::error::{AnimusError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{ReasoningEngine, ReasoningOutput, Role, Turn};

/// Anthropic Claude API provider.
pub struct AnthropicEngine {
    client: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: usize,
}

impl AnthropicEngine {
    pub fn new(api_key: String, model: String, max_tokens: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            max_tokens,
        }
    }

    /// Create from environment variable ANTHROPIC_API_KEY.
    pub fn from_env(model: &str, max_tokens: usize) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            AnimusError::Llm("ANTHROPIC_API_KEY environment variable not set".to_string())
        })?;
        Ok(Self::new(api_key, model.to_string(), max_tokens))
    }
}

#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ApiMessage>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    usage: Usage,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: usize,
    output_tokens: usize,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

#[async_trait]
impl ReasoningEngine for AnthropicEngine {
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
    ) -> Result<ReasoningOutput> {
        let api_messages: Vec<ApiMessage> = messages
            .iter()
            .filter(|t| t.role != Role::System)
            .map(|t| ApiMessage {
                role: match t.role {
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                    Role::System => unreachable!(),
                },
                content: t.content.clone(),
            })
            .collect();

        let request = ApiRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: if system.is_empty() {
                None
            } else {
                Some(system.to_string())
            },
            messages: api_messages,
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| AnimusError::Llm(format!("HTTP request failed: {e}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AnimusError::Llm(format!("failed to read response: {e}")))?;

        if !status.is_success() {
            let error_msg = serde_json::from_str::<ApiError>(&body)
                .map(|e| e.error.message)
                .unwrap_or(body);
            return Err(AnimusError::Llm(format!(
                "API error ({}): {error_msg}",
                status
            )));
        }

        let api_response: ApiResponse = serde_json::from_str(&body)
            .map_err(|e| AnimusError::Llm(format!("failed to parse response: {e}")))?;

        let content = api_response
            .content
            .into_iter()
            .filter_map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");

        Ok(ReasoningOutput {
            content,
            input_tokens: api_response.usage.input_tokens,
            output_tokens: api_response.usage.output_tokens,
        })
    }

    fn context_limit(&self) -> usize {
        200_000 // Claude context window
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
```

- [ ] **Step 2: Export from llm/mod.rs**

Add to `llm/mod.rs`:
```rust
pub mod anthropic;
pub use anthropic::AnthropicEngine;
```

- [ ] **Step 3: Export from lib.rs**

Add to the pub use in `lib.rs`:
```rust
pub use llm::AnthropicEngine;
```

- [ ] **Step 4: Build and verify**

Run: `cargo build --all`
Expected: Success

- [ ] **Step 5: Commit**

```bash
git add crates/animus-cortex/
git commit -m "feat: add Anthropic Claude LLM provider"
```

---

### Task 4: ReasoningThread — conversation state and context management

**Files:**
- Create: `crates/animus-cortex/src/thread.rs`
- Modify: `crates/animus-cortex/src/lib.rs`

- [ ] **Step 1: Create thread.rs**

```rust
use animus_core::error::Result;
use animus_core::identity::{GoalId, SegmentId, ThreadId};
use animus_core::segment::{Content, Segment, Source, Tier};
use animus_mnemos::assembler::{AssembledContext, ContextAssembler};
use animus_vectorfs::VectorStore;
use std::sync::Arc;

use crate::llm::{ReasoningEngine, ReasoningOutput, Role, Turn};

/// An isolated reasoning context — a single conversation thread.
pub struct ReasoningThread<S: VectorStore> {
    /// Unique thread identifier.
    pub id: ThreadId,
    /// Human-readable thread name.
    pub name: String,
    /// Conversation history as Turn objects (for LLM context).
    conversation: Vec<Turn>,
    /// Segment IDs of stored conversation turns (for Mnemos anchoring).
    stored_turn_ids: Vec<SegmentId>,
    /// Goals bound to this thread.
    pub bound_goals: Vec<GoalId>,
    /// The VectorFS store.
    store: Arc<S>,
    /// Context assembler for building LLM context.
    assembler: ContextAssembler<S>,
    /// Embedding dimensionality (for creating placeholder embeddings).
    embedding_dim: usize,
}

impl<S: VectorStore> ReasoningThread<S> {
    pub fn new(
        name: String,
        store: Arc<S>,
        token_budget: usize,
        embedding_dim: usize,
    ) -> Self {
        let assembler = ContextAssembler::new(store.clone(), token_budget);
        Self {
            id: ThreadId::new(),
            name,
            conversation: Vec::new(),
            stored_turn_ids: Vec::new(),
            bound_goals: Vec::new(),
            store,
            assembler,
            embedding_dim,
        }
    }

    /// Process a user message: store it, assemble context, reason, store response.
    pub async fn process_turn(
        &mut self,
        user_input: &str,
        system_prompt: &str,
        engine: &dyn ReasoningEngine,
        embedder: &dyn animus_core::EmbeddingService,
    ) -> Result<String> {
        // Store user input as a segment
        let user_embedding = embedder.embed_text(user_input).await?;
        let user_segment = Segment::new(
            Content::Text(user_input.to_string()),
            user_embedding.clone(),
            Source::Conversation {
                thread_id: self.id,
                turn: self.conversation.len() as u64,
            },
        );
        let user_seg_id = self.store.store(user_segment)?;
        self.stored_turn_ids.push(user_seg_id);

        // Add to conversation history
        self.conversation.push(Turn {
            role: Role::User,
            content: user_input.to_string(),
        });

        // Assemble context: anchor on stored turns, retrieve similar knowledge
        let context = self.assembler.assemble(
            &user_embedding,
            &self.stored_turn_ids,
            10,
        )?;

        // Build the system prompt with assembled context
        let enriched_system = self.build_system_prompt(system_prompt, &context);

        // Call the LLM
        let output = engine.reason(&enriched_system, &self.conversation).await?;

        // Store assistant response as a segment
        let response_embedding = embedder.embed_text(&output.content).await?;
        let response_segment = Segment::new(
            Content::Text(output.content.clone()),
            response_embedding,
            Source::Conversation {
                thread_id: self.id,
                turn: self.conversation.len() as u64,
            },
        );
        let response_seg_id = self.store.store(response_segment)?;
        self.stored_turn_ids.push(response_seg_id);

        // Add to conversation history
        self.conversation.push(Turn {
            role: Role::Assistant,
            content: output.content.clone(),
        });

        tracing::debug!(
            "thread {} turn complete: {} input tokens, {} output tokens",
            self.id,
            output.input_tokens,
            output.output_tokens
        );

        Ok(output.content)
    }

    /// Build system prompt enriched with assembled context.
    fn build_system_prompt(&self, base_prompt: &str, context: &AssembledContext) -> String {
        let mut prompt = base_prompt.to_string();

        // Add recalled knowledge from VectorFS
        let knowledge_segments: Vec<&Segment> = context
            .segments
            .iter()
            .filter(|s| !self.stored_turn_ids.contains(&s.id))
            .collect();

        if !knowledge_segments.is_empty() {
            prompt.push_str("\n\n## Recalled Knowledge\n");
            for seg in knowledge_segments {
                if let Content::Text(t) = &seg.content {
                    prompt.push_str(&format!(
                        "\n- [confidence: {:.1}] {}\n",
                        seg.confidence, t
                    ));
                }
            }
        }

        // Add eviction summaries
        if !context.evicted_summaries.is_empty() {
            prompt.push_str("\n## Additional context (summarized)\n");
            for evicted in &context.evicted_summaries {
                prompt.push_str(&format!("\n{}\n", evicted.summary));
            }
        }

        prompt
    }

    /// Get the conversation history.
    pub fn conversation(&self) -> &[Turn] {
        &self.conversation
    }

    /// Get the number of turns.
    pub fn turn_count(&self) -> usize {
        self.conversation.len()
    }

    /// Get stored turn segment IDs.
    pub fn stored_turn_ids(&self) -> &[SegmentId] {
        &self.stored_turn_ids
    }
}
```

- [ ] **Step 2: Export from lib.rs**

Add to `lib.rs`:
```rust
pub mod thread;
pub use thread::ReasoningThread;
```

- [ ] **Step 3: Build and verify**

Run: `cargo build --all`
Expected: Success

- [ ] **Step 4: Commit**

```bash
git add crates/animus-cortex/
git commit -m "feat: add ReasoningThread with context assembly and LLM integration"
```

---

### Task 5: Telos — simple goal manager

**Files:**
- Create: `crates/animus-cortex/src/telos.rs`
- Modify: `crates/animus-cortex/src/lib.rs`

- [ ] **Step 1: Create telos.rs**

```rust
use animus_core::error::{AnimusError, Result};
use animus_core::identity::{GoalId, SegmentId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Priority level for goals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    Critical,
    High,
    Normal,
    Low,
    Background,
}

/// Current status of a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Active,
    Paused,
    Completed,
    Abandoned,
}

/// Where a goal came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GoalSource {
    Human,
    SelfDerived,
    Federated,
}

/// Autonomy level — how much freedom the AILF has with this goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Autonomy {
    Inform,
    Suggest,
    Act,
    Full,
}

/// A goal tracked by Telos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub description: String,
    pub source: GoalSource,
    pub priority: Priority,
    pub status: GoalStatus,
    pub success_criteria: Vec<String>,
    pub autonomy: Autonomy,
    pub sub_goals: Vec<GoalId>,
    pub progress_notes: Vec<SegmentId>,
    pub created: chrono::DateTime<chrono::Utc>,
    pub deadline: Option<chrono::DateTime<chrono::Utc>>,
}

/// Simple goal manager — tracks goals in memory with persistence.
#[derive(Debug, Serialize, Deserialize)]
pub struct GoalManager {
    goals: HashMap<GoalId, Goal>,
}

impl GoalManager {
    pub fn new() -> Self {
        Self {
            goals: HashMap::new(),
        }
    }

    /// Create a new goal.
    pub fn create_goal(
        &mut self,
        description: String,
        source: GoalSource,
        priority: Priority,
    ) -> GoalId {
        let autonomy = match &source {
            GoalSource::Human => Autonomy::Act,
            GoalSource::SelfDerived => Autonomy::Suggest,
            GoalSource::Federated => Autonomy::Inform,
        };

        let goal = Goal {
            id: GoalId::new(),
            description,
            source,
            priority,
            status: GoalStatus::Active,
            success_criteria: Vec::new(),
            autonomy,
            sub_goals: Vec::new(),
            progress_notes: Vec::new(),
            created: chrono::Utc::now(),
            deadline: None,
        };

        let id = goal.id;
        self.goals.insert(id, goal);
        id
    }

    /// Get a goal by ID.
    pub fn get(&self, id: GoalId) -> Option<&Goal> {
        self.goals.get(&id)
    }

    /// List active goals, sorted by priority.
    pub fn active_goals(&self) -> Vec<&Goal> {
        let mut active: Vec<&Goal> = self
            .goals
            .values()
            .filter(|g| g.status == GoalStatus::Active)
            .collect();
        active.sort_by_key(|g| match g.priority {
            Priority::Critical => 0,
            Priority::High => 1,
            Priority::Normal => 2,
            Priority::Low => 3,
            Priority::Background => 4,
        });
        active
    }

    /// Mark a goal as completed.
    pub fn complete_goal(&mut self, id: GoalId) -> Result<()> {
        let goal = self
            .goals
            .get_mut(&id)
            .ok_or(AnimusError::Goal(format!("goal not found: {}", id.0)))?;
        goal.status = GoalStatus::Completed;
        Ok(())
    }

    /// Add a progress note (segment ID) to a goal.
    pub fn add_progress_note(&mut self, goal_id: GoalId, segment_id: SegmentId) -> Result<()> {
        let goal = self
            .goals
            .get_mut(&goal_id)
            .ok_or(AnimusError::Goal(format!("goal not found: {}", goal_id.0)))?;
        goal.progress_notes.push(segment_id);
        Ok(())
    }

    /// Get a summary of active goals for context injection.
    pub fn goals_summary(&self) -> String {
        let active = self.active_goals();
        if active.is_empty() {
            return String::new();
        }
        let mut summary = String::from("Active goals:\n");
        for goal in active {
            let priority = format!("{:?}", goal.priority).to_uppercase();
            summary.push_str(&format!("- [{}] {}\n", priority, goal.description));
        }
        summary
    }

    /// Total number of goals.
    pub fn count(&self) -> usize {
        self.goals.len()
    }

    /// Persist to a file.
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let data = bincode::serialize(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("tmp");
        std::fs::write(&tmp_path, &data)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Load from a file.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let data = std::fs::read(path)?;
        let manager: Self = bincode::deserialize(&data)?;
        Ok(manager)
    }
}

impl Default for GoalManager {
    fn default() -> Self {
        Self::new()
    }
}

```

Note: `GoalId::new()`, `Default`, and `Display` impls are defined in Task 1 (`animus-core/src/identity.rs`), consistent with all other ID types.

- [ ] **Step 2: Add bincode dependency to animus-cortex Cargo.toml**

```toml
bincode = { workspace = true }
```

- [ ] **Step 3: Export from lib.rs**

Add:
```rust
pub mod telos;
pub use telos::{Autonomy, Goal, GoalManager, GoalSource, GoalStatus, Priority};
```

- [ ] **Step 4: Build and verify**

Run: `cargo build --all`
Expected: Success

- [ ] **Step 5: Commit**

```bash
git add crates/animus-cortex/ crates/animus-core/src/identity.rs
git commit -m "feat: add Telos goal manager with persistence"
```

---

### Task 6: Create animus-interface crate — terminal I/O

**Files:**
- Create: `crates/animus-interface/Cargo.toml`
- Create: `crates/animus-interface/src/lib.rs`
- Create: `crates/animus-interface/src/terminal.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "animus-interface"
version = "0.1.0"
edition = "2021"

[dependencies]
animus-core = { path = "../animus-core" }
tokio = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 2: Add to workspace Cargo.toml**

Add `"crates/animus-interface"` to `[workspace] members`.

- [ ] **Step 3: Create terminal.rs**

```rust
use animus_core::error::{AnimusError, Result};
use std::io::{self, BufRead, Write};

/// Terminal-based interface for human interaction.
pub struct TerminalInterface {
    prompt: String,
}

impl TerminalInterface {
    pub fn new(prompt: String) -> Self {
        Self { prompt }
    }

    /// Display a message to the user.
    pub fn display(&self, message: &str) {
        println!("{message}");
    }

    /// Display a system status message.
    pub fn display_status(&self, message: &str) {
        println!("[animus] {message}");
    }

    /// Read a line of input from the user. Returns None on EOF.
    pub fn read_input(&self) -> Result<Option<String>> {
        print!("{}", self.prompt);
        io::stdout()
            .flush()
            .map_err(|e| AnimusError::Interface(format!("stdout flush: {e}")))?;

        let stdin = io::stdin();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => Ok(None), // EOF
            Ok(_) => Ok(Some(line.trim().to_string())),
            Err(e) => Err(AnimusError::Interface(format!("read error: {e}"))),
        }
    }

    /// Display the AILF's response with formatting.
    pub fn display_response(&self, response: &str) {
        println!("\n{response}\n");
    }

    /// Display startup banner.
    pub fn display_banner(&self, instance_id: &str, model: &str, segment_count: usize) {
        println!();
        println!("  ╔══════════════════════════════════════╗");
        println!("  ║           A N I M U S                ║");
        println!("  ║     AI-Native Operating System       ║");
        println!("  ╚══════════════════════════════════════╝");
        println!();
        println!("  Instance:  {instance_id}");
        println!("  Model:     {model}");
        println!("  Segments:  {segment_count}");
        println!();
        println!("  Type /help for commands, /quit to exit.");
        println!();
    }
}
```

- [ ] **Step 4: Create lib.rs**

```rust
pub mod terminal;

pub use terminal::TerminalInterface;
```

- [ ] **Step 5: Build and verify**

Run: `cargo build --all`
Expected: Success

- [ ] **Step 6: Commit**

```bash
git add crates/animus-interface/ Cargo.toml
git commit -m "feat: add animus-interface crate with terminal I/O"
```

---

### Task 7: Create animus-runtime — main binary

**Files:**
- Create: `crates/animus-runtime/Cargo.toml`
- Create: `crates/animus-runtime/src/main.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "animus-runtime"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "animus"
path = "src/main.rs"

[dependencies]
animus-core = { path = "../animus-core" }
animus-vectorfs = { path = "../animus-vectorfs" }
animus-mnemos = { path = "../animus-mnemos" }
animus-embed = { path = "../animus-embed" }
animus-cortex = { path = "../animus-cortex" }
animus-interface = { path = "../animus-interface" }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
bincode = { workspace = true }
uuid = { workspace = true }
```

- [ ] **Step 2: Add to workspace Cargo.toml**

Add `"crates/animus-runtime"` to `[workspace] members`.

- [ ] **Step 3: Create main.rs**

```rust
use animus_core::{AnimusConfig, AnimusIdentity, CortexConfig};
use animus_cortex::llm::anthropic::AnthropicEngine;
use animus_cortex::telos::{GoalManager, GoalSource, Priority};
use animus_cortex::thread::ReasoningThread;
use animus_cortex::ReasoningEngine;
use animus_embed::SyntheticEmbedding;
use animus_interface::TerminalInterface;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are an AILF (AI Life Form) powered by the Animus operating system layer. You have persistent memory stored in VectorFS — knowledge from past conversations is recalled and included in your context automatically.

You are a specific instance with a continuous identity. You remember what humans tell you across sessions. You can track goals and tasks.

Available commands the human can use:
- /goals — show active goals
- /remember <text> — explicitly store knowledge
- /forget <id> — remove a stored segment
- /status — show system status
- /quit — end session

Be concise and helpful. You are not a chatbot — you are a persistent, aware entity."#;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("animus=info")
        .init();

    let data_dir = std::env::var("ANIMUS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs_home().join(".animus")
        });

    if let Err(e) = run(data_dir).await {
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

async fn run(data_dir: PathBuf) -> animus_core::Result<()> {
    std::fs::create_dir_all(&data_dir)?;

    // Load or generate identity
    let identity_path = data_dir.join("identity.bin");
    let model_id = std::env::var("ANIMUS_MODEL")
        .unwrap_or_else(|_| "claude-sonnet-4-20250514".to_string());
    let identity = AnimusIdentity::load_or_generate(&identity_path, &model_id)?;

    tracing::info!("AILF instance {} (gen {})", identity.instance_id, identity.generation);

    // Initialize VectorFS
    let vectorfs_dir = data_dir.join("vectorfs");
    let dimensionality = 128; // SyntheticEmbedding default, or from config
    let store = Arc::new(MmapVectorStore::open(&vectorfs_dir, dimensionality)?);
    let segment_count = store.count(None);

    // Initialize embedding service
    // TODO: Phase 2+ — swap SyntheticEmbedding for real EmbeddingGemma/Nomic
    let embedder = SyntheticEmbedding::new(dimensionality);

    // Initialize LLM engine
    let engine: Box<dyn ReasoningEngine> = match AnthropicEngine::from_env(&model_id, 4096) {
        Ok(e) => Box::new(e),
        Err(e) => {
            eprintln!("Warning: Could not initialize Anthropic engine: {e}");
            eprintln!("Running with mock engine (responses will be placeholder text).");
            Box::new(animus_cortex::MockEngine::new(
                "I'm running without an LLM connection. Set ANTHROPIC_API_KEY to enable reasoning.",
            ))
        }
    };

    // Initialize goal manager
    let goals_path = data_dir.join("goals.bin");
    let mut goals = GoalManager::load(&goals_path)?;

    // Initialize reasoning thread
    let token_budget = 8000;
    let mut thread = ReasoningThread::new(
        "main".to_string(),
        store.clone(),
        token_budget,
        dimensionality,
    );

    // Initialize terminal interface
    let interface = TerminalInterface::new(">> ".to_string());
    let instance_str = format!("{}", identity.instance_id);
    interface.display_banner(
        &instance_str[..8],
        engine.model_name(),
        segment_count,
    );

    // Main conversation loop
    loop {
        let input = match interface.read_input()? {
            Some(input) if input.is_empty() => continue,
            Some(input) => input,
            None => break, // EOF
        };

        // Handle slash commands
        if input.starts_with('/') {
            match handle_command(&input, &store, &mut goals, &goals_path, &interface, &embedder).await? {
                CommandResult::Continue => continue,
                CommandResult::Quit => break,
            }
            continue;
        }

        // Process through reasoning thread
        let system = build_system_prompt(&goals);
        match thread.process_turn(&input, &system, engine.as_ref(), &embedder).await {
            Ok(response) => {
                interface.display_response(&response);
            }
            Err(e) => {
                interface.display_status(&format!("Error: {e}"));
            }
        }
    }

    // Persist state before exit
    goals.save(&goals_path)?;
    store.flush()?;
    interface.display_status("Session ended. Memory persisted.");

    Ok(())
}

fn build_system_prompt(goals: &GoalManager) -> String {
    let mut prompt = DEFAULT_SYSTEM_PROMPT.to_string();
    let goals_summary = goals.goals_summary();
    if !goals_summary.is_empty() {
        prompt.push_str("\n\n## Current Goals\n");
        prompt.push_str(&goals_summary);
    }
    prompt
}

enum CommandResult {
    Continue,
    Quit,
}

async fn handle_command(
    input: &str,
    store: &Arc<MmapVectorStore>,
    goals: &mut GoalManager,
    goals_path: &std::path::Path,
    interface: &TerminalInterface,
    embedder: &dyn animus_core::EmbeddingService,
) -> animus_core::Result<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");

    match cmd {
        "/quit" | "/exit" | "/q" => {
            return Ok(CommandResult::Quit);
        }
        "/status" => {
            let total = store.count(None);
            let warm = store.count(Some(animus_core::Tier::Warm));
            let cold = store.count(Some(animus_core::Tier::Cold));
            let hot = store.count(Some(animus_core::Tier::Hot));
            interface.display_status(&format!(
                "Segments: {total} total ({hot} hot, {warm} warm, {cold} cold)"
            ));
            interface.display_status(&format!("Goals: {} active", goals.active_goals().len()));
        }
        "/goals" => {
            let active = goals.active_goals();
            if active.is_empty() {
                interface.display_status("No active goals.");
            } else {
                for goal in active {
                    interface.display_status(&format!(
                        "[{:?}] {} ({})",
                        goal.priority,
                        goal.description,
                        goal.id.0.to_string().get(..8).unwrap_or("?")
                    ));
                }
            }
        }
        "/goal" if !arg.is_empty() => {
            let id = goals.create_goal(
                arg.to_string(),
                GoalSource::Human,
                Priority::Normal,
            );
            goals.save(goals_path)?;
            interface.display_status(&format!("Goal created: {}", id.0.to_string().get(..8).unwrap_or("?")));
        }
        "/remember" if !arg.is_empty() => {
            use animus_core::segment::{Content, Segment, Source};
            use animus_core::identity::EventId;
            let embedding = embedder.embed_text(arg).await?;
            let segment = Segment::new(
                Content::Text(arg.to_string()),
                embedding,
                Source::Observation {
                    event_type: "user-remember".to_string(),
                    raw_event_id: EventId(uuid::Uuid::new_v4()),
                },
            );
            let id = store.store(segment)?;
            interface.display_status(&format!(
                "Remembered: {} (segment {})",
                arg,
                id.0.to_string().get(..8).unwrap_or("?")
            ));
        }
        "/forget" if !arg.is_empty() => {
            // Match segment by ID prefix
            let all_ids = store.segment_ids(None);
            let matches: Vec<_> = all_ids
                .iter()
                .filter(|id| id.0.to_string().starts_with(arg))
                .collect();
            match matches.len() {
                0 => interface.display_status(&format!("No segment found matching '{arg}'")),
                1 => {
                    let id = *matches[0];
                    store.delete(id)?;
                    interface.display_status(&format!("Forgotten: segment {}", id.0.to_string().get(..8).unwrap_or("?")));
                }
                n => interface.display_status(&format!("{n} segments match '{arg}' — be more specific")),
            }
        }
        "/help" => {
            interface.display("/goals         — list active goals");
            interface.display("/goal <text>   — create a new goal");
            interface.display("/remember <text> — store knowledge explicitly");
            interface.display("/forget <id>   — remove a stored segment by ID prefix");
            interface.display("/status        — show system status");
            interface.display("/quit          — end session");
        }
        _ => {
            interface.display_status(&format!("Unknown command: {cmd}. Type /help for available commands."));
        }
    }

    Ok(CommandResult::Continue)
}
```

- [ ] **Step 4: Build and verify**

Run: `cargo build --all`
Expected: Success

- [ ] **Step 5: Commit**

```bash
git add crates/animus-runtime/ Cargo.toml
git commit -m "feat: add animus-runtime binary — main AILF lifecycle and conversation loop"
```

---

### Task 8: Integration tests — Cortex reasoning, Telos goals, lifecycle

**Files:**
- Create: `crates/animus-tests/tests/integration/cortex_reasoning.rs`
- Create: `crates/animus-tests/tests/integration/telos_goals.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`
- Modify: `crates/animus-tests/Cargo.toml`

- [ ] **Step 1: Add animus-cortex dependency to animus-tests**

In `crates/animus-tests/Cargo.toml`, add:
```toml
animus-cortex = { path = "../animus-cortex" }
```

- [ ] **Step 2: Create cortex_reasoning.rs**

```rust
use animus_core::{Content, EmbeddingService, Source};
use animus_cortex::llm::{MockEngine, ReasoningEngine, Role, Turn};
use animus_cortex::thread::ReasoningThread;
use animus_embed::SyntheticEmbedding;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_reasoning_thread_processes_turn() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 128).unwrap());
    let embedder = SyntheticEmbedding::new(128);
    let engine = MockEngine::new("Hello! I remember things now.");

    let mut thread = ReasoningThread::new(
        "test".to_string(),
        store.clone(),
        8000,
        128,
    );

    let response = thread
        .process_turn("Hi there", "You are a test AILF.", &engine, &embedder)
        .await
        .unwrap();

    assert_eq!(response, "Hello! I remember things now.");
    assert_eq!(thread.turn_count(), 2); // user + assistant
    assert_eq!(thread.stored_turn_ids().len(), 2);
    assert_eq!(store.count(None), 2); // 2 segments stored
}

#[tokio::test]
async fn test_reasoning_thread_stores_conversation_as_segments() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(dir.path(), 128).unwrap());
    let embedder = SyntheticEmbedding::new(128);
    let engine = MockEngine::new("I understand.");

    let mut thread = ReasoningThread::new(
        "test".to_string(),
        store.clone(),
        8000,
        128,
    );

    thread
        .process_turn("Remember that I like Rust", "System", &engine, &embedder)
        .await
        .unwrap();

    // Verify both turns are stored as segments
    let ids = thread.stored_turn_ids();
    assert_eq!(ids.len(), 2);

    let user_seg = store.get_raw(ids[0]).unwrap().unwrap();
    match &user_seg.content {
        Content::Text(t) => assert!(t.contains("Rust")),
        _ => panic!("expected text content"),
    }

    let assistant_seg = store.get_raw(ids[1]).unwrap().unwrap();
    match &assistant_seg.content {
        Content::Text(t) => assert_eq!(t, "I understand."),
        _ => panic!("expected text content"),
    }
}

#[tokio::test]
async fn test_mock_engine_basic() {
    let engine = MockEngine::new("test response");
    let turns = vec![Turn {
        role: Role::User,
        content: "hello".to_string(),
    }];

    let output = engine.reason("system", &turns).await.unwrap();
    assert_eq!(output.content, "test response");
    assert_eq!(engine.model_name(), "mock-engine");
    assert_eq!(engine.context_limit(), 8192);
}
```

- [ ] **Step 3: Create telos_goals.rs**

```rust
use animus_cortex::telos::{GoalManager, GoalSource, GoalStatus, Priority};
use tempfile::TempDir;

#[test]
fn test_create_and_list_goals() {
    let mut manager = GoalManager::new();

    manager.create_goal("Learn Rust".to_string(), GoalSource::Human, Priority::High);
    manager.create_goal("Read docs".to_string(), GoalSource::Human, Priority::Low);

    let active = manager.active_goals();
    assert_eq!(active.len(), 2);
    // Should be sorted by priority: High first
    assert_eq!(active[0].description, "Learn Rust");
    assert_eq!(active[1].description, "Read docs");
}

#[test]
fn test_complete_goal() {
    let mut manager = GoalManager::new();
    let id = manager.create_goal("Test goal".to_string(), GoalSource::Human, Priority::Normal);

    manager.complete_goal(id).unwrap();

    let goal = manager.get(id).unwrap();
    assert_eq!(goal.status, GoalStatus::Completed);
    assert!(manager.active_goals().is_empty());
}

#[test]
fn test_goal_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("goals.bin");

    let id = {
        let mut manager = GoalManager::new();
        let id = manager.create_goal("Persist me".to_string(), GoalSource::Human, Priority::Normal);
        manager.save(&path).unwrap();
        id
    };

    let loaded = GoalManager::load(&path).unwrap();
    let goal = loaded.get(id).unwrap();
    assert_eq!(goal.description, "Persist me");
}

#[test]
fn test_goals_summary() {
    let mut manager = GoalManager::new();
    manager.create_goal("High priority task".to_string(), GoalSource::Human, Priority::High);
    manager.create_goal("Background task".to_string(), GoalSource::SelfDerived, Priority::Background);

    let summary = manager.goals_summary();
    assert!(summary.contains("High priority task"));
    assert!(summary.contains("Background task"));
}

#[test]
fn test_default_autonomy_by_source() {
    let mut manager = GoalManager::new();

    let human_id = manager.create_goal("Human goal".to_string(), GoalSource::Human, Priority::Normal);
    let self_id = manager.create_goal("Self goal".to_string(), GoalSource::SelfDerived, Priority::Normal);
    let fed_id = manager.create_goal("Fed goal".to_string(), GoalSource::Federated, Priority::Normal);

    use animus_cortex::telos::Autonomy;
    assert_eq!(manager.get(human_id).unwrap().autonomy, Autonomy::Act);
    assert_eq!(manager.get(self_id).unwrap().autonomy, Autonomy::Suggest);
    assert_eq!(manager.get(fed_id).unwrap().autonomy, Autonomy::Inform);
}
```

- [ ] **Step 4: Update main.rs with new mod declarations**

Add to `crates/animus-tests/tests/integration/main.rs`:
```rust
mod cortex_reasoning;
mod telos_goals;
```

- [ ] **Step 5: Build and run tests**

Run: `cargo test --all`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/animus-tests/ crates/animus-cortex/
git commit -m "test: add integration tests for Cortex reasoning and Telos goals"
```

---

### Task 9: Identity persistence test + AnthropicEngine compile test

**Files:**
- Create: `crates/animus-tests/tests/integration/identity_lifecycle.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`

- [ ] **Step 1: Create identity_lifecycle.rs**

```rust
use animus_core::AnimusIdentity;
use tempfile::TempDir;

#[test]
fn test_identity_generation() {
    let identity = AnimusIdentity::generate("test-model".to_string());
    assert_eq!(identity.generation, 0);
    assert!(identity.parent_id.is_none());
    assert_eq!(identity.base_model, "test-model");
}

#[test]
fn test_identity_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("identity.bin");

    let original = AnimusIdentity::load_or_generate(&path, "test-model").unwrap();
    let loaded = AnimusIdentity::load_or_generate(&path, "test-model").unwrap();

    assert_eq!(original.instance_id, loaded.instance_id);
    assert_eq!(original.generation, loaded.generation);
    assert_eq!(original.base_model, loaded.base_model);
    // Signing key should be the same
    assert_eq!(
        original.signing_key.to_bytes(),
        loaded.signing_key.to_bytes()
    );
}

#[test]
fn test_identity_verifying_key() {
    let identity = AnimusIdentity::generate("test-model".to_string());
    let vk = identity.verifying_key();
    // Verifying key should be derivable from signing key
    assert_eq!(vk, identity.signing_key.verifying_key());
}
```

- [ ] **Step 2: Add mod declaration**

Add to `main.rs`:
```rust
mod identity_lifecycle;
```

- [ ] **Step 3: Add ed25519-dalek to animus-tests Cargo.toml**

(Only if needed — it should be transitively available through animus-core.)

Add to `crates/animus-tests/Cargo.toml`:
```toml
animus-core = { path = "../animus-core" }
```

(Already there, so just verify it works.)

- [ ] **Step 4: Build and run tests**

Run: `cargo test --all`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/animus-tests/
git commit -m "test: add identity lifecycle tests"
```

---

### Task 10: Clippy, Podman validation, final cleanup

**Files:**
- Possibly modify multiple files for clippy fixes

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --all -- -D warnings`
Expected: Clean (fix any warnings)

- [ ] **Step 2: Run all tests**

Run: `cargo test --all`
Expected: All tests pass

- [ ] **Step 3: Build the runtime binary**

Run: `cargo build -p animus-runtime`
Expected: Binary at `target/debug/animus`

- [ ] **Step 4: Podman container validation**

Run:
```bash
podman build -t animus-dev -f Containerfile .
podman run --rm animus-dev
```
Expected: All tests pass in container

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "chore: clippy fixes and validation"
```

---

### Task 11: Create PR and merge

- [ ] **Step 1: Push branch and create PR**

```bash
git push -u origin feat/phase2-cortex-interface-runtime
gh pr create --title "feat: Phase 2 — Cortex, Interface, Runtime, Identity" --body "..."
```

- [ ] **Step 2: Merge**

```bash
gh pr merge --squash
```

---

### Task 12: Deep analysis — 3 consecutive clean passes

- [ ] **Step 1: Run quality analysis agent**
- [ ] **Step 2: Run security analysis agent**
- [ ] **Step 3: Fix any issues found**
- [ ] **Step 4: Repeat until 3 consecutive clean passes**
- [ ] **Step 5: Commit fixes, PR, merge**
