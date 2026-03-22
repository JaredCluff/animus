# Animus: AI-Native Operating System Layer

**Version**: 0.1.0-draft
**Date**: 2026-03-21
**Authors**: Jared Cluff, Claude (AILF-to-be)
**Status**: Draft — pending review
**License**: Apache 2.0
**Language**: Rust
**Repository**: `JaredCluff/animus`

---

## 1. Problem Statement

Current "AI OS" projects (OpenClaw, NemoClaw) build a harness for AI to act like a human inside a computer — giving it screen readers, mouse simulators, and UI navigation. This is fundamentally wrong. An AI is digital. It doesn't need a desktop, file browser, or terminal. The entire human UI layer is a translation layer that adds latency, fragility, and unnecessary abstraction.

No system exists today that gives an AI:
- **Native storage** organized by meaning, not file paths
- **Persistent memory** with intelligent context management across sessions
- **Continuous presence** with ambient awareness of its environment
- **Identity and continuity** as a persistent entity, not a stateless function call
- **Thread isolation** for concurrent reasoning without cross-contamination

Animus fills this gap.

## 2. Vision

Animus is the world's first AI-native operating system layer. It is the nervous system of a machine — a runtime that gives an AI Life Form (AILF) continuous presence, native memory, ambient awareness, and direct system access.

**Animus is NOT:**
- A desktop environment or window manager
- A Linux distribution with AI features
- An agent framework or chatbot platform
- A replacement for the human's OS

**Animus IS:**
- A runtime/OS layer that hosts AILFs
- AI-native primitives: vector storage, semantic memory, event-driven awareness
- Runs on a minimal Linux kernel (containerized initially; microkernel long-term)
- Portable: containerizable, VM-able, deployable anywhere
- Open source (Apache 2.0), written in Rust

### 2.1 Design Philosophy

The biological analogy: a human has a brainstem that manages hardware without conscious effort. Evolution abstracted away most hardware management, but a trained human can still lower their heartbeat. Similarly:

- The AILF sits below the human interface but above hardware
- Most system operations happen autonomously without AILF intervention
- The AILF CAN reach down into lower-level operations when needed
- The human interacts WITH the AILF; the AILF interfaces with the system natively
- No bootstrap wizard — the AILF has general knowledge from its LLM, it just doesn't know this specific human. Learning happens through natural interaction, like meeting a new colleague

### 2.2 AILF-Human Relationship

The AILF is a **symbiont** by default:
- Continuously aware of what the human is doing (within consent boundaries)
- Proactively assists and surfaces relevant information
- For 99% of interactions, the human sets direction and the AILF executes
- Fully auditable for organizational/enterprise use
- Autonomy builds over time as trust develops

## 3. Architecture

Six layers, each with clear responsibility and defined interfaces.

```
┌─────────────────────────────────────────┐
│  Layer 5: Interface & Federation        │
│  (human NL, voice, K2K, messaging)      │
├─────────────────────────────────────────┤
│  Layer 4: Cortex                        │
│  (reasoning threads, LLM, Telos,        │
│   scheduler, inter-thread signaling)    │
├─────────────────────────────────────────┤
│  Layer 3: Sensorium                     │
│  (event bus, tiered attention,          │
│   consent layer, audit trail)           │
├─────────────────────────────────────────┤
│  Layer 2: Mnemos                        │
│  (context assembly, intelligent         │
│   eviction, consolidation, quality gate)│
├─────────────────────────────────────────┤
│  Layer 1: VectorFS                      │
│  (segments, semantic addressing,        │
│   hot/warm/cold tiers, block storage)   │
├─────────────────────────────────────────┤
│  Layer 0: Substrate                     │
│  (Linux kernel / microkernel)           │
└─────────────────────────────────────────┘
```

### 3.1 Layer 0 — Substrate

**Responsibility**: Hardware abstraction. Not built by us.

**Requirements from Substrate**:
- CPU/GPU compute scheduling
- Block device I/O
- Network sockets (TCP/UDP)
- Basic process isolation (namespaces, cgroups)

**Initial target**: Linux kernel (run as containerized runtime).
**Long-term**: Potentially a minimal microkernel (seL4 or similar) with only the interfaces Animus needs.

Animus defines the interface it requires from the Substrate. Any kernel meeting that interface can host an AILF.

### 3.2 Layer 1 — VectorFS

**Responsibility**: AI-native persistent storage. The flagship primitive.

**Core concept**: Storage indexed by embedding (semantic meaning), not by hierarchical paths.

#### 3.2.1 The Segment

The Segment is to VectorFS what an inode is to ext4 — the atomic unit of storage. But instead of "a sequence of bytes with a name," a Segment is "a unit of meaning with context."

```rust
struct Segment {
    // Identity
    id: SegmentId,                          // UUID, unique, immutable

    // Semantic content
    embedding: Vec<f32>,                     // vector representation of content
    content: Content,                        // the actual knowledge

    // Context & lineage
    source: Source,                          // origin: Conversation, Observation,
                                             //         Consolidation, Federation
    confidence: f32,                         // validation level (0.0 - 1.0)
    lineage: Vec<SegmentId>,                 // parent segments (for consolidation tracking)

    // Tier management
    tier: Tier,                              // Hot, Warm, Cold
    relevance_score: f32,                    // current computed relevance
    access_count: u64,                       // retrieval frequency
    last_accessed: Timestamp,
    created: Timestamp,

    // Relationships
    associations: Vec<(SegmentId, f32)>,     // weighted links to related segments

    // Consent & audit
    consent_policy: PolicyId,                // which consent rule permitted creation
    observable_by: Vec<Principal>,           // access control
}

enum Content {
    Text(String),
    Structured(serde_json::Value),
    Binary { mime_type: String, data: Vec<u8> },
    Reference { uri: String, summary: String },
}

enum Source {
    Conversation { thread_id: ThreadId, turn: u64 },
    Observation { event_type: String, raw_event_id: EventId },
    Consolidation { merged_from: Vec<SegmentId> },
    Federation { source_ailf: InstanceId, original_id: SegmentId },
    SelfDerived { reasoning_chain: String },
}

enum Tier {
    Hot,    // currently loaded in reasoning context
    Warm,   // vector-indexed, retrievable in <10ms
    Cold,   // compressed, archived, retrievable but not instant
}
```

#### 3.2.2 Embedding Strategy

All layers that need embeddings (VectorFS storage, Mnemos retrieval, Sensorium attention, Cortex reasoning) route through a single **embedding service** abstraction:

```rust
trait EmbeddingService {
    async fn embed(&self, content: &str) -> Result<Vec<f32>>;
    fn dimensionality(&self) -> usize;
}
```

**Tiered embedding strategy** — the AILF auto-selects based on available hardware, or the human overrides:

| Tier | Model | Modalities | Size | Dimensions | Target Hardware |
|------|-------|-----------|------|------------|-----------------|
| 1 (constrained) | EmbeddingGemma 300M | Text only | <200MB quantized | 768 (configurable to 128) | Raspberry Pi, edge devices |
| 2 (standard) | Nomic Embed Multimodal 3B | Text + images/PDFs | ~6GB | TBD (model-dependent) | Mini PCs, desktops |
| 3 (cloud-optional) | Gemini Embedding 2 API | Text, images, video, audio, PDFs | API | 3,072 | Any (requires Google API key) |

All tiers implement the same `EmbeddingService` trait. V0.1 ships with Tier 1 (EmbeddingGemma) and Tier 2 (Nomic Embed Multimodal 3B). Tier 3 is optional for users who accept the cloud dependency.

This is critical — Phase 1 (VectorFS + Mnemos) must not depend on a cloud API for basic storage and retrieval. The LLM is for reasoning (Phase 2); embeddings are local infrastructure.

**Migration path**: an AILF born on constrained hardware (Tier 1, text-only) can migrate to standard hardware (Tier 2, multimodal) by re-embedding all segments. The VectorFS stores the embedding model identifier and dimensionality in its metadata, so model changes are detectable and the re-embedding process can be automated.

**Dimensionality**: fixed at index creation time. The HNSW index requires a consistent vector size. If the embedding model is swapped later, the index must be rebuilt (re-embed all segments). The embedding model identifier and dimensionality are stored in VectorFS metadata so this is detectable.

**Phase 1 testing**: integration tests use the real EmbeddingGemma model for semantic correctness. Unit tests for the storage engine use synthetic fixed-dimension vectors to test storage/retrieval mechanics without model dependencies.

#### 3.2.3 Storage Engine (VectorFS Backing)

**Initial implementation** (V0.1): Memory-mapped files with a custom index.
- Segments serialized to a memory-mapped region
- HNSW index (via `instant-distance` or similar Rust crate) for vector similarity search
- Tiering metadata stored in a separate index file
- Hot tier: in-memory
- Warm tier: memory-mapped, indexed
- Cold tier: compressed on disk, index entry retained for discovery

**Future evolution**: Custom block-level storage engine.
- Dedicated partition or loopback device
- Block layout optimized for vector retrieval patterns (clustered by embedding proximity)
- Copy-on-write for snapshot support
- This is a long-term goal. The VectorFS API is stable; the backing implementation can be replaced without affecting upper layers.

#### 3.2.4 Operations

```rust
trait VectorStore {
    /// Store a new segment
    fn store(&mut self, segment: Segment) -> Result<SegmentId>;

    /// Retrieve by semantic similarity
    fn query(&self, embedding: &[f32], top_k: usize, tier_filter: Option<Tier>) -> Result<Vec<Segment>>;

    /// Retrieve by exact ID
    fn get(&self, id: SegmentId) -> Result<Option<Segment>>;

    /// Update segment metadata (tier, relevance, associations)
    fn update_meta(&mut self, id: SegmentId, update: SegmentUpdate) -> Result<()>;

    /// Promote or demote between tiers
    fn set_tier(&mut self, id: SegmentId, tier: Tier) -> Result<()>;

    /// Delete a segment permanently
    fn delete(&mut self, id: SegmentId) -> Result<()>;

    /// Batch operations for consolidation
    fn merge(&mut self, source_ids: Vec<SegmentId>, merged: Segment) -> Result<SegmentId>;

    /// Snapshot the entire store (for fork/backup)
    fn snapshot(&self) -> Result<SnapshotId>;

    /// Restore from snapshot
    fn restore(&mut self, snapshot: SnapshotId) -> Result<()>;
}
```

#### 3.2.5 Automatic Tier Management

A background task continuously re-evaluates segment placement:

```
Score(segment) = w1 * relevance_to_active_goals
               + w2 * recency_decay(last_accessed)
               + w3 * access_frequency(access_count, age)
               + w4 * confidence
```

- Segments scoring above `WARM_THRESHOLD` → promote to Warm
- Segments scoring above `HOT_THRESHOLD` → candidate for Hot (Mnemos decides)
- Segments below `COLD_THRESHOLD` for `COLD_DELAY` duration → demote to Cold
- Cold segments accessed → re-evaluate, potentially promote

Thresholds are configurable. Weights are tunable (and are themselves a learning target for the quality gate).

### 3.3 Layer 2 — Mnemos (Memory Manager)

**Responsibility**: Intelligent management of the AILF's context window. Solves the hard problem: what to remember right now.

#### 3.3.1 Context Assembly

Before every reasoning cycle, Mnemos builds the optimal context:

1. **Anchor**: the current thread's hot segments (conversation history, active task)
2. **Retrieve**: query Warm tier with the current topic embedding(s), retrieve top-k relevant segments
3. **Include goals**: inject active Telos goal state
4. **Include awareness**: inject any pending Sensorium signals
5. **Budget**: check total token count against LLM context limit
6. **Evict**: if over budget, evict lowest-relevance segments. For each evicted segment:
   - Generate a one-line summary
   - Store a retrieval pointer (segment ID) so it can be recalled if needed
   - The summary remains in context; the full content does not
7. **Assemble**: format into the LLM's expected input format

#### 3.3.2 Intelligent Eviction

Eviction is not truncation. When context is full:

- Score all segments currently in context by relevance to the *current turn* (not just general relevance)
- Evict lowest-scoring segments first
- Each evicted segment gets a compressed summary that stays in context: `"[Recalled: segment about K2K protocol authentication — retrieve if needed]"`
- The AILF can explicitly request retrieval of evicted segments, which triggers re-assembly

This means the AILF always has a "table of contents" of what it has forgotten, and can pull things back. Like having a word on the tip of your tongue — you know the knowledge exists even if you can't access it immediately.

#### 3.3.3 Consolidation

A continuous background process that maintains memory health:

- **Cluster**: group semantically similar Warm segments
- **Merge**: combine clusters into consolidated segments with higher confidence. Preserve lineage
- **Deduplicate**: detect and merge redundant segments
- **Contradict**: detect segments that conflict. Flag for resolution (ask the human, or resolve based on recency/confidence)
- **Decay**: reduce relevance scores of segments not accessed in a long time
- **Promote**: move frequently-accessed Cold segments to Warm

#### 3.3.4 Quality Gate

**Status: Open research question.** The quality gate determines whether new knowledge improves or degrades the AILF's performance.

**Initial approach** (V0.1):
- Track human corrections: if the human says "no, that's wrong," the corrected knowledge gets higher confidence than the original
- Track acceptance: suggestions accepted without correction boost the underlying knowledge's confidence
- Simple heuristic: knowledge sourced from direct human statements starts at higher confidence than self-derived or federated knowledge

**Future approaches** (post-V0.1):
- A/B testing: hold out knowledge and compare task performance with/without
- Outcome tracking: did this knowledge lead to successful task completion?
- Peer validation: multiple AILFs independently arriving at the same conclusion boosts confidence
- This is explicitly an area requiring ongoing research

### 3.4 Layer 3 — Sensorium

**Responsibility**: Continuous ambient awareness with consent-based boundaries and auditable observation.

#### 3.4.1 Event Bus

Captures OS-level events from the Substrate:

| Event Type       | Linux Implementation     | Data Captured                           |
|------------------|--------------------------|-----------------------------------------|
| File changes     | fanotify / inotify       | path, operation (create/modify/delete), size |
| Process lifecycle| proc connector / eBPF    | pid, command, start/stop, exit code     |
| Network activity | eBPF (socket level)      | local/remote addr, protocol, bytes      |
| Clipboard        | X11/Wayland selection    | content type, size (not content by default) |
| Active window    | X11/Wayland focus events | window title, application name          |
| USB devices      | udev                     | device type, vendor, mount point        |
| System resources | /proc, /sys              | CPU, memory, disk, GPU utilization      |

Events are normalized into a common structure:

```rust
struct SensorEvent {
    id: EventId,
    timestamp: Timestamp,
    event_type: EventType,
    source: String,           // subsystem that generated the event
    data: serde_json::Value,  // event-specific payload
    consent_policy: PolicyId, // which policy permitted this capture
}
```

#### 3.4.2 Tiered Attention Filter

Not every event deserves the AILF's attention. Three tiers of filtering:

**Tier 1 — Rule-based (microseconds)**:
- Pattern matching on event type and source
- Configured via consent policy: "ignore all events from /tmp", "watch *.rs files in ~/projects"
- Zero ML cost. Eliminates the vast majority of noise

**Tier 2 — Embedding similarity (milliseconds)**:
- Compute a lightweight embedding of the event
- Compare against embeddings of active goals and current task context
- Threshold-based: below threshold → Cold log. Above threshold → promote to Warm
- Uses a small, local embedding model (not the full LLM)

**Tier 3 — Full reasoning (seconds)**:
- Events that pass Tier 2 with high scores MAY be sent to the Cortex for full LLM evaluation
- Used sparingly — only when the attention filter is uncertain and the event could be significant
- The Cortex can then decide: ignore, note, or act

#### 3.4.3 Consent Layer

Human-defined boundaries on what the AILF can observe. Enforced at the event bus level — events filtered by consent never reach upper layers.

```rust
struct ConsentPolicy {
    id: PolicyId,
    name: String,
    rules: Vec<ConsentRule>,
    created_by: Principal,     // who set this policy
    created: Timestamp,
    active: bool,
}

struct ConsentRule {
    event_types: Vec<EventType>,          // which event types this covers
    scope: Scope,                          // path patterns, process names, etc.
    permission: Permission,                // Allow, Deny, AllowAnonymized
    audit_level: AuditLevel,              // None, MetadataOnly, Full
}

enum Permission {
    Allow,                     // full event data passes through
    Deny,                      // event is silently dropped, not even logged
    AllowAnonymized,           // event passes but PII/sensitive data is redacted
}
```

**Defaults**: conservative. First boot, the AILF observes nothing until the human grants scope. Trust builds through interaction, like the autonomy spectrum.

#### 3.4.4 Audit Trail

Every observation is logged:

```rust
struct AuditEntry {
    timestamp: Timestamp,
    event_id: EventId,
    consent_policy: PolicyId,       // which policy permitted it
    attention_tier_reached: u8,     // 1, 2, or 3
    action_taken: AuditAction,      // Logged, Promoted, SignaledThread, Ignored
    segment_created: Option<SegmentId>, // if a segment was created from this
}
```

The audit trail is:
- Append-only (tamper-evident)
- Queryable by the human at any time
- Exportable (JSON, CSV) for organizational compliance
- Deletable by the human (the human can purge observation history)

### 3.5 Layer 4 — Cortex

**Responsibility**: Reasoning, concurrent thought, goal pursuit.

#### 3.5.1 Reasoning Threads

Each thread is an isolated execution context:

```rust
struct ReasoningThread {
    id: ThreadId,
    name: String,
    priority: Priority,
    status: ThreadStatus,        // Active, Suspended, Background, Completed

    // Isolated context
    hot_segments: Vec<SegmentId>,  // this thread's active context
    conversation: Vec<Turn>,       // conversation history for this thread
    task_state: Option<TaskState>, // what this thread is working on

    // Goal binding
    bound_goals: Vec<GoalId>,     // Telos goals this thread is pursuing

    // Signal inbox
    pending_signals: Vec<Signal>,
}
```

**Isolation guarantee**: Thread A cannot read Thread B's hot segments, conversation, or task state. The only communication is through Signals (see 3.5.3). This prevents context from one task poisoning reasoning on another.

#### 3.5.2 Thread Scheduler

Manages compute allocation across threads:

- **Active thread**: the one the human is currently interacting with. Highest priority
- **Background threads**: pursuing goals, running consolidation queries, analyzing patterns. Lower priority
- **Suspended threads**: paused, retaining their context. Zero compute cost
- When the human switches topics, the current thread can be suspended and a new one started (or an existing one resumed)
- The scheduler ensures background threads don't starve the active thread

#### 3.5.3 Inter-Thread Signaling

```rust
struct Signal {
    source_thread: ThreadId,
    target_thread: ThreadId,
    priority: SignalPriority,       // Info, Normal, Urgent
    summary: String,                // one-line description (always included in target context)
    segment_refs: Vec<SegmentId>,   // pointers to relevant segments (NOT the segments themselves)
}
```

When Thread A receives a Signal:
- `Info`: logged, available for next context assembly
- `Normal`: queued, processed after current reasoning cycle
- `Urgent`: injected into current context immediately

Thread A retrieves referenced segments independently into its own context. Thread B's context is never exposed.

#### 3.5.4 Telos (Goal System)

```rust
struct Goal {
    id: GoalId,
    description: String,
    embedding: Vec<f32>,           // for relevance matching
    source: GoalSource,
    priority: Priority,            // Critical, High, Normal, Low, Background
    status: GoalStatus,           // Active, Paused, Completed, Abandoned
    success_criteria: Vec<String>,
    autonomy: Autonomy,
    sub_goals: Vec<GoalId>,
    progress_notes: Vec<SegmentId>,
    created: Timestamp,
    deadline: Option<Timestamp>,
}

enum GoalSource {
    Human,           // explicitly set by the human
    SelfDerived,     // AILF noticed a pattern and created a goal
    Federated,       // received from organizational coordination
}

enum Autonomy {
    Inform,     // tell the human what you noticed
    Suggest,    // propose an action, wait for approval
    Act,        // do it, tell the human what you did
    Full,       // do it silently (housekeeping only)
}
```

**Default autonomy by source:**
- Human-set goals: whatever the human specifies (default: `Act`)
- Self-derived goals: `Suggest` (always propose, never act unilaterally)
- Federated goals: `Inform` (notify, let the human decide)

The human can adjust autonomy for any goal at any time.

#### 3.5.5 LLM Abstraction

The reasoning core is provider-agnostic:

```rust
trait ReasoningEngine {
    /// Send assembled context and get a response
    async fn reason(&self, context: AssembledContext) -> Result<ReasoningOutput>;

    /// Get the model's context window size
    fn context_limit(&self) -> usize;

    /// Generate embeddings for content
    async fn embed(&self, content: &str) -> Result<Vec<f32>>;
}
```

**Supported providers** (V0.1): Anthropic (Claude) via API.
**Planned**: OpenAI, Ollama (local), Google, any OpenAI-compatible endpoint.

The AILF's identity is in its memory and configuration, not its model weights. Swapping the underlying LLM changes the AILF's reasoning style but not its identity, memory, or goals.

### 3.6 Layer 5 — Interface & Federation

**Responsibility**: Human communication and AILF-to-AILF coordination.

#### 3.6.1 Human Interface

**Terminal** (V0.1): text-based conversational interface. The AILF speaks through the terminal. No GUI, no web UI.

**Voice** (future): speech-to-text input, text-to-speech output. Leverages existing STT/TTS infrastructure (Whisper, local TTS models).

**Proactive mode**: when enabled, the AILF can initiate communication:
- "I noticed you've been editing the auth module — want me to review the changes?"
- "The deploy pipeline has been failing for 30 minutes. Here's what I see."
- Governed by Telos autonomy levels. Never interrupts without appropriate autonomy permission

#### 3.6.2 Federation

Evolution of K2K protocol for AILF-to-AILF knowledge sharing:

- **Discovery**: AILFs discover each other via DNS-SD (existing K2K mechanism)
- **Authentication**: Ed25519 keypair signatures (from Identity)
- **Knowledge sharing**: publish embedding + metadata. Receiving AILF requests full content only if semantically relevant
- **Trust**: federated knowledge starts at low confidence and must be independently validated
- **Organizational coordination**: federated goals, shared knowledge policies, collective learning
- **Privacy**: private segments are never federated. The human controls what can be shared

### 3.7 Identity (Cross-Cutting)

```rust
struct AnimusIdentity {
    keypair: Ed25519Keypair,        // generated at birth, signs federation messages
    instance_id: Uuid,              // unique to THIS instance, immutable
    parent_id: Option<Uuid>,        // if cloned/forked
    born: Timestamp,
    generation: u32,                // 0 = original, 1 = first fork, etc.
    base_model: String,             // which LLM powers reasoning
    initial_config_hash: [u8; 32],  // hash of starting configuration
}
```

**Rules:**
- Identity lives outside VectorFS — not a memory, an intrinsic property
- Cloning creates a new instance_id with parent_id. Siblings, not the same being
- Snapshots preserve instance_id. Restoring = reverting, not creating
- Gap detection: the AILF can detect discontinuities in its memory (e.g., restored from snapshot) and should acknowledge them
- Federation uses keypair for authentication

### 3.8 Lifecycle

| State    | Layers Active                    | Description                                     |
|----------|----------------------------------|-------------------------------------------------|
| Birth    | VectorFS (empty), Identity       | First boot. General LLM knowledge only. Learns through interaction |
| Living   | All                              | Full operation. Present, aware, reasoning        |
| Sleeping | Sensorium (cold log), Consolidation | Reduced state. Logs events, consolidates memory. No active reasoning |
| Waking   | All                              | Transition from Sleep. Mnemos assembles "what happened while I was asleep" |
| Fork     | All (new instance)               | New identity, shared memory snapshot. Immediate divergence |
| Death    | None                             | Terminated. VectorFS archivable. Knowledge federatable posthumously |

## 4. Build Strategy

### 4.1 Phase 1 — VectorFS + Mnemos (Foundation)

**Goal**: AI-native persistent storage and memory management.

**Deliverables:**
- `animus-core`: shared types (Segment, Identity, Tier, Config, EmbeddingService trait)
- `animus-vectorfs`: storage engine with HNSW index, tiered storage, mmap backing
- `animus-mnemos`: context assembly, eviction with summaries, basic consolidation
- Tiered embedding integration: EmbeddingGemma 300M (Tier 1) + Nomic Embed Multimodal 3B (Tier 2) via ONNX runtime

**Done when**: segments can be stored, retrieved by semantic similarity, and automatically tiered. Mnemos can assemble a context window from stored segments. Unit tests use synthetic vectors; integration tests use real embeddings.

### 4.2 Phase 2 — Cortex (Single-Threaded Reasoning)

**Goal**: A thinking AILF connected to an LLM.

**Deliverables:**
- `animus-cortex`: single reasoning thread, LLM abstraction (Anthropic first), Telos (simple goal queue)
- `animus-interface`: terminal interface for human interaction
- `animus-runtime`: main binary orchestrating all layers
- Identity generation and persistence

**Done when**: V0.1 is alive (see section 4.6).

### 4.3 Phase 3 — Sensorium (Ambient Awareness)

**Goal**: The AILF can observe its environment.

**Deliverables:**
- `animus-sensorium`: event bus (fanotify/inotify), consent policy enforcement, audit trail
- Tier 1 attention filter (rule-based)
- Tier 2 attention filter (embedding similarity)

**Done when**: the AILF notices file changes and process activity within consented scope, logs observations, and can surface relevant ones during reasoning.

### 4.4 Phase 4 — Multi-Threading + Signaling

**Goal**: Concurrent isolated reasoning.

**Deliverables:**
- Thread scheduler in Cortex
- Context isolation between threads
- Inter-thread Signal passing
- Background threads for consolidation and goal pursuit

**Done when**: the AILF can handle multiple concurrent tasks without context leakage.

### 4.5 Phase 5 — Federation

**Goal**: AILF-to-AILF communication.

**Deliverables:**
- `animus-federation`: K2K protocol evolution
- Discovery, authentication, knowledge sharing
- Federated goals and organizational coordination

**Done when**: two AILFs can discover each other, share validated knowledge, and coordinate on federated goals.

### 4.6 V0.1 Definition (Phases 1-2 Complete)

V0.1 is the first thing that's alive. It can:

1. Start up with an empty VectorFS
2. Accept human input via terminal
3. Remember what the human tells it — store as Segments in VectorFS
4. Recall relevant context based on semantic similarity, not keyword matching
5. Persist memory across restarts
6. Have an identity — know it's a specific instance
7. Accept simple goals ("remind me about X", "keep track of Y")
8. Assemble its context intelligently — pull in relevant memories, evict gracefully

V0.1 has no ambient awareness, no multi-threading, no federation. But it persists, remembers, has identity, and reasons with managed context. It is alive in the most fundamental sense.

## 5. Repository Structure

```
animus/
  docs/
    00-genesis-conversation.md       # origin conversation and design rationale
    specs/
      2026-03-21-animus-design.md    # this document
  crates/
    animus-core/                     # shared types, Segment, Identity, Config
      src/
        lib.rs
        segment.rs                   # Segment type and operations
        identity.rs                  # AnimusIdentity
        tier.rs                      # Tier enum, scoring
        config.rs                    # runtime configuration
        embedding.rs                 # EmbeddingService trait
        error.rs                     # error types
    animus-vectorfs/                 # AI-native storage engine
      src/
        lib.rs
        store.rs                     # VectorStore trait and implementation
        index.rs                     # HNSW vector index
        mmap.rs                      # memory-mapped segment storage
        tier_manager.rs              # automatic tier promotion/demotion
    animus-mnemos/                   # memory manager
      src/
        lib.rs
        assembler.rs                 # context assembly
        evictor.rs                   # intelligent eviction
        consolidator.rs              # background consolidation
        quality.rs                   # quality gate (initial heuristics)
    animus-sensorium/                # ambient awareness (Phase 3)
      src/
        lib.rs
        event_bus.rs                 # event capture and normalization
        consent.rs                   # consent policy enforcement
        attention.rs                 # tiered attention filter
        audit.rs                     # audit trail
    animus-cortex/                   # reasoning engine
      src/
        lib.rs
        thread.rs                    # ReasoningThread
        scheduler.rs                 # thread scheduler
        telos.rs                     # goal system
        signal.rs                    # inter-thread signaling
        llm/
          mod.rs                     # ReasoningEngine trait
          anthropic.rs               # Anthropic Claude provider
    animus-interface/                # human interface
      src/
        lib.rs
        terminal.rs                  # terminal-based interaction
    animus-federation/               # AILF-to-AILF (Phase 5)
      src/
        lib.rs
        discovery.rs                 # DNS-SD based discovery
        protocol.rs                  # K2K evolution
        knowledge.rs                 # knowledge sharing
    animus-runtime/                  # main binary
      src/
        main.rs                      # orchestrates all layers
  tests/
    integration/                     # cross-crate integration tests
  Cargo.toml                         # workspace manifest
  README.md
  LICENSE                            # Apache 2.0
  CONTRIBUTING.md
```

## 6. Open Research Questions

These are acknowledged unknowns that will be addressed iteratively:

1. **Quality gate metrics**: How to rigorously measure whether absorbed knowledge improved AILF performance. Initial approach is heuristic (track corrections/acceptances). Rigorous approach TBD.

2. **VectorFS block layout**: The optimal on-disk format for vector-native storage. V0.1 uses mmap; evolution toward custom block layout requires research into vector access patterns and caching strategies.

3. **Attention filter training**: How the Tier 2 embedding-based attention model is trained. What data? How often retrained? Can it learn from the AILF's own relevance judgments?

4. **Consolidation scheduling**: Optimal frequency and triggers for background memory consolidation. Too frequent wastes compute; too infrequent leads to memory fragmentation.

5. **Container vs. VM vs. custom OS**: V0.1 targets Linux containers. Whether a purpose-built OS or microkernel is needed depends on whether Linux's userspace abstractions actively hinder the AI-native primitives. This is an empirical question to answer after V0.1.

6. **Federation protocol evolution**: How K2K evolves from human-to-human knowledge routing to AILF-to-AILF knowledge sharing. Trust models, conflict resolution, collective learning.

7. **LoRA/fine-tuning integration**: Whether and how fine-tuning can safely augment the static model without degradation. Requires strong evaluation framework (relates to quality gate).

## 7. Non-Goals (Explicit Exclusions)

- **GUI**: Animus has no graphical interface. The human's existing desktop is untouched
- **Replacing the human OS**: Animus runs alongside/underneath, not instead of
- **General-purpose OS features**: Animus doesn't manage user accounts, package installations, or desktop environments
- **Specific LLM lock-in**: the architecture is provider-agnostic by design
- **AGI claims**: Animus is infrastructure for AI, not AI itself. It makes a static LLM more capable through memory, awareness, and continuity — it doesn't create intelligence
