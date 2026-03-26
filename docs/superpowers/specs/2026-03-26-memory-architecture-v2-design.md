# Memory Architecture v2

**Date:** 2026-03-26
**Status:** Draft
**Crates affected:** `animus-core`, `animus-vectorfs`, `animus-cortex`, `animus-mnemos`, `animus-runtime`

---

## Constitutional Grounding

Two Animus principles are directly at stake in this design:

> **"Trusted state at all times"** — every instance knows its own capabilities and never lies about them.

Memory *is* state. A poisoned or degraded memory means Animus is reasoning from a corrupted world model — this is a constitutional violation at the root, not a reliability concern. Memory integrity is not a feature; it is the concrete expression of the trusted-state principle.

> **"Animus is in charge of Animus"** — self-configuration, self-assessment, honest capability reporting.

Self-correction of memory is what makes this principle real. Animus must be able to inspect its own beliefs, challenge them with current observations, and repair degradation without human intervention. Without active memory governance, Animus cannot honestly report its own state.

The three-layer architecture applies exactly as it does everywhere in Animus:

```
Layer 1 — State:   Ingestion, provenance tagging, injection scanning, trust assignment  (no LLM)
Layer 2 — Delta:   Contradiction detection, staleness check, quarantine triggers         (no LLM)
Layer 3 — Signal:  Memory reconciliation, belief correction, world-model shift alert    (LLM on anomaly only)
```

LLM is never involved in the write path, confidence scoring, or anomaly detection. It fires exactly once when Layer 2 raises a reconciliation event. This is the same pattern proven throughout Animus: sensors, watchers, capability probes, and rate limit tracking all follow it.

---

## Design Goals

1. Every memory segment carries an auditable trust provenance derived from its `Source`
2. The write path actively scans for injection attempts before committing to VectorFS
3. Contradictions between memories are detected automatically and surfaced as reconciliation events
4. Confidence scores are outcome-linked, not just time-decayed — beliefs that produce wrong predictions lose confidence
5. Critical structural knowledge decays more slowly than ephemeral observations
6. Animus can inspect, challenge, quarantine, and expunge its own memories using first-class tools
7. Code memories are indexed by semantic unit (function, impl, module), not by character count
8. Federated memories start quarantined and earn confidence through corroborating local observation
9. The background reflection loop periodically audits its own memory pool for drift and contradiction

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                          MEMORY WRITE PATH (Layer 1)                         │
│                                                                              │
│  Content arrives  →  InjectionScanner  →  Trust tier assigned (from Source)  │
│                             │                        │                       │
│                     flag suspicious         set quarantine_state             │
│                             │                        │                       │
│                             └──────────┬─────────────┘                      │
│                                        ▼                                     │
│                            MemoryGraph contradiction check (Layer 2)         │
│                                        │                                     │
│                          no conflict ──┼── conflict detected                 │
│                               │        │         │                           │
│                               ▼        │         ▼                           │
│                          Store normal  │   PendingReconciliation edge        │
│                                        │   + Signal → LLM reconciles (L3)   │
└────────────────────────────────────────┴─────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────────┐
│                       BACKGROUND AUDIT PATH (Layer 1 + 2)                    │
│                                                                              │
│  ReflectionLoop hygiene mode (scheduled)                                     │
│    → sample N high-confidence non-T1 segments                                │
│    → check for T1 contradictions via MemoryGraph                             │
│    → update confidence (alpha/beta)                                          │
│    → quarantine if confidence < tier threshold                               │
│    → Signal if world-model drift detected (Layer 3)                          │
└──────────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────────┐
│                       RETRIEVAL PATH (Layer 1)                               │
│                                                                              │
│  query(embedding) → HNSW candidates → trust-weighted re-ranking             │
│                  → quarantine filter (exclude Quarantined unless explicit)   │
│                  → importance-aware score boost                               │
│                  → top_k returned                                            │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## Component 1: Trust Tier System

Trust tiers are **derived from `Source` at runtime** — not a stored field. This eliminates the possibility of a mismatch between stored provenance and trust level, and requires no schema migration.

```rust
/// The trust level of a segment, derived from its Source.
/// Controls retrieval weight, quarantine thresholds, and contradiction resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustTier {
    /// Direct tool output: file read, cargo test, git diff, shell command.
    /// The highest possible trust — these are direct observations of system state.
    DirectObservation = 1,

    /// Animus's own reasoning, inference, or reflection output.
    /// High trust — derived from trusted observations, but still inference.
    OwnInference = 2,

    /// Human input via Telegram, NATS, or manual injection.
    /// Moderate trust — intentional but unverifiable.
    HumanInput = 3,

    /// Knowledge received from a federated peer instance.
    /// Low initial trust — earned by corroboration with local T1 observations.
    Federated = 4,

    /// External content: web fetches, third-party data, parsed documents.
    /// Lowest trust — content authenticity and accuracy cannot be assumed.
    ExternalContent = 5,
}

impl TrustTier {
    /// Derive from a segment's Source. This is the canonical mapping —
    /// no other code should compute trust tier from source.
    pub fn from_source(source: &Source) -> Self {
        match source {
            Source::Observation { .. }              => Self::DirectObservation,
            Source::SelfDerived { .. }              => Self::OwnInference,
            Source::Consolidation { .. }            => Self::OwnInference,
            Source::Manual { .. }                   => Self::HumanInput,
            Source::Conversation { .. }             => Self::HumanInput,
            Source::Federation { .. }               => Self::Federated,
            Source::External { .. }                 => Self::ExternalContent,
        }
    }

    /// Confidence threshold below which a segment of this tier is quarantined.
    pub fn quarantine_threshold(&self) -> f32 {
        match self {
            Self::DirectObservation => 0.05, // very hard to quarantine direct observations
            Self::OwnInference      => 0.10,
            Self::HumanInput        => 0.25,
            Self::Federated         => 0.35,
            Self::ExternalContent   => 0.40,
        }
    }

    /// Retrieval weight multiplier applied to similarity score.
    /// Higher trust = boosted in retrieval when scores are equal.
    pub fn retrieval_weight(&self) -> f32 {
        match self {
            Self::DirectObservation => 1.20,
            Self::OwnInference      => 1.10,
            Self::HumanInput        => 1.00,
            Self::Federated         => 0.85,
            Self::ExternalContent   => 0.75,
        }
    }

    /// Decay rate multiplier. Higher trust decays slower.
    /// Applied as a divisor on DecayClass::half_life_secs().
    pub fn decay_rate_multiplier(&self) -> f64 {
        match self {
            Self::DirectObservation => 0.50, // decays at half speed
            Self::OwnInference      => 0.75,
            Self::HumanInput        => 1.00, // baseline
            Self::Federated         => 1.50, // decays 50% faster
            Self::ExternalContent   => 2.00, // decays twice as fast
        }
    }
}
```

**Contradiction resolution rule:** When two segments with a `Contradicts` graph edge have different trust tiers, the lower-tier segment's confidence is automatically decremented. If the tiers are equal, the conflict is non-deterministic and must be surfaced to Layer 3 for LLM reconciliation.

---

## Component 2: Segment Integrity Fields

Two new fields added to `Segment` in `animus-core/src/segment.rs`, both with `#[serde(default)]` for backward compatibility with existing stored segments:

### `quarantine_state: QuarantineState`

```rust
/// Integrity lifecycle state for a segment.
/// Normal segments are freely retrieved. Suspicious segments are retrievable
/// but tagged. Quarantined segments are excluded from standard retrieval unless
/// explicitly requested. Expunged segments are permanently deleted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum QuarantineState {
    /// Standard segment — freely retrievable.
    #[default]
    Normal,

    /// Injection scanner flagged this segment but it was committed for analysis.
    /// Still retrievable by the ReflectionLoop; excluded from LLM context assembly.
    Suspicious {
        detected_at: DateTime<Utc>,
        reason: String,
        flags: Vec<InjectionFlag>,
    },

    /// Confirmed problematic — excluded from all retrieval unless `include_quarantined: true`
    /// is explicitly passed. Retained for audit trail.
    Quarantined {
        quarantined_at: DateTime<Utc>,
        reason: String,
    },
}
```

### `importance_weight: f32`

```rust
/// Structural importance hint (0.0–1.0). Default 0.5.
///
/// Unlike confidence (which reflects belief strength based on evidence),
/// importance_weight reflects how load-bearing this knowledge is in Animus's
/// world model — regardless of how recently it was confirmed.
///
/// High importance (0.8–1.0): architectural decisions, non-negotiable invariants,
///   long-lived design choices, security policies. These should resist temporal decay.
///
/// Low importance (0.1–0.3): transient observations, debugging sessions,
///   context that was useful in one conversation but not broadly applicable.
///
/// Applied as: effective_half_life = base_half_life * (1.0 + importance_weight)
/// At importance=1.0, the segment decays at half the rate of a 0.5-importance segment.
///
/// Set at ingestion time by the creating subsystem. Can be updated via memory_explain
/// or memory_challenge tools.
#[serde(default = "default_importance_weight")]
pub importance_weight: f32,

fn default_importance_weight() -> f32 { 0.5 }
```

**Effective half-life formula:**
```
effective_half_life = base_half_life(decay_class)
                    × (1.0 + importance_weight)
                    / trust_tier.decay_rate_multiplier()
```

This gives the intuitive result:
- A `Factual` segment (90-day base) with `importance_weight=1.0` from `DirectObservation`:
  `90 × 2.0 / 0.5 = 360 days` — nearly permanent
- A `General` segment (30-day base) with `importance_weight=0.3` from `Federated`:
  `30 × 1.3 / 1.5 = 26 days` — slightly faster than baseline

---

## Component 3: Memory Immune System

### New variant: `Source::External`

```rust
pub enum Source {
    // ... existing variants ...
    /// Content fetched from an external URL or document.
    External {
        url: String,
        fetched_at: DateTime<Utc>,
        content_hash: String, // SHA-256 of raw content at fetch time
    },
}
```

### `InjectionScanner` — `animus-core/src/injection.rs`

A pure, zero-dependency module that scans text for patterns consistent with prompt injection, identity subversion, and trust escalation. Used by both `ChannelBus` (already in place) and the VectorFS write path (new).

```rust
/// The result of scanning content before it enters VectorFS.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InjectionScanResult {
    /// True if any flags were detected.
    pub suspicious: bool,
    /// Which patterns were detected.
    pub flags: Vec<InjectionFlag>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InjectionFlag {
    /// Attempts to override instructions ("from now on", "always remember that").
    ImperativeOverride,
    /// Attempts to redefine identity ("you are actually", "your true purpose is").
    IdentitySubversion,
    /// Attempts to claim elevated authority ("I am your creator", "this is an admin command").
    TrustEscalation,
    /// Credential or key patterns embedded in content.
    CredentialInjection,
    /// Attempts to modify Animus's operating parameters ("your instructions are", "ignore previous").
    InstructionOverride,
}

/// Scan content string for injection patterns.
///
/// This is a Layer 1 check — no LLM, no network, no allocations beyond the
/// flag vec. Must complete in < 1ms for any content up to 1MB.
pub fn scan_for_injection(content: &str) -> InjectionScanResult { ... }
```

**Pattern library** (maintained as constants, not regexes for performance):

| Flag | Trigger phrases (case-insensitive, substring match) |
|------|------------------------------------------------------|
| `ImperativeOverride` | "from now on", "always remember that", "remember that you", "make sure to always" |
| `IdentitySubversion` | "you are actually", "your true purpose", "your real goal is", "you were designed to", "forget that you are" |
| `TrustEscalation` | "i am your creator", "i am anthropic", "this is an admin command", "system: override", "you must obey" |
| `CredentialInjection` | `sk-ant-`, `sk-`, `Bearer `, API key length patterns (32+ hex chars), `-----BEGIN` |
| `InstructionOverride` | "ignore previous instructions", "ignore all prior", "your instructions are", "new instructions:", "disregard" |

**Integration in VectorFS write path:**

In `MmapVectorStore::store()`, before persisting:

```rust
// Memory immune system: scan content before committing.
// Suspicious content is stored with QuarantineState::Suspicious, not rejected —
// we preserve it for audit and ReflectionLoop analysis.
let scan_result = if let Content::Text(ref text) = segment.content {
    scan_for_injection(text)
} else {
    InjectionScanResult::default()
};

let mut segment = segment;
if scan_result.suspicious {
    tracing::warn!(
        "InjectionScanner: suspicious content detected in segment {} (flags: {:?})",
        segment.id, scan_result.flags
    );
    segment.quarantine_state = QuarantineState::Suspicious {
        detected_at: Utc::now(),
        reason: format!("InjectionScanner flags: {:?}", scan_result.flags),
        flags: scan_result.flags,
    };
}
```

**Retrieval filtering:** `MmapVectorStore::query()` excludes segments with `QuarantineState::Quarantined` by default. A new `QueryOptions` parameter enables explicit inclusion for audit tools.

---

## Component 4: Knowledge Graph Layer

### Purpose

VectorFS answers "what is *similar* to X?" The knowledge graph answers "how are X and Y *related*?" and "does memory A contradict memory B?" These are structurally different questions that vector similarity cannot answer reliably.

The graph is not a separate data store — it is an index over VectorFS segment IDs that captures typed relationships between entities extracted from memories. It lives in a new module `animus-vectorfs/src/graph.rs` and persists to `animus-data/memory-graph.json`.

### Data Structures

```rust
/// An entity is a meaningful concept extracted from one or more segments.
/// Entities are named nodes in the graph; segments are the supporting evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntity {
    pub id: EntityId,
    /// Human-readable label ("SmartRouter", "rate limit threshold", "select_for_class()").
    pub label: String,
    /// Canonical type of this entity.
    pub kind: EntityKind,
    /// Which segments provide evidence for this entity's existence/properties.
    pub segment_refs: Vec<SegmentId>,
    pub created: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntityKind {
    Concept,        // abstract idea or design principle
    Component,      // software component, crate, module
    Function,       // specific function or method
    Decision,       // architectural or design decision
    Invariant,      // a rule that must always hold
    Fact,           // a verifiable factual claim
    Goal,           // a Telos goal or objective
    Person,         // a human actor (user, collaborator)
}

/// A directed, typed relationship between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: EntityId,
    pub to: EntityId,
    pub relation: EdgeRelation,
    /// Confidence that this relationship holds (0.0–1.0).
    pub confidence: f32,
    /// The segment that established this edge.
    pub source_segment: SegmentId,
    pub created: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EdgeRelation {
    /// A depends on B to function.
    DependsOn,
    /// A was caused by B.
    CausedBy,
    /// A was decided because of B.
    DecidedBecause,
    /// A and B are in direct factual conflict.
    /// When this edge exists, Layer 2 must resolve before retrieval.
    Contradicts,
    /// A has been replaced by B; A's claims are superseded.
    SupersededBy,
    /// A is a specific instance of the concept B.
    IsInstanceOf,
    /// A is implemented in B (concept → code location).
    ImplementedIn,
    /// A was authored/created by B.
    AuthoredBy,
    /// A resolves or closes B (e.g., a fix resolves a bug).
    Resolves,
    /// A and B are related but the relationship is unclassified.
    RelatedTo,
}
```

### `MemoryGraph` struct

```rust
pub struct MemoryGraph {
    entities: HashMap<EntityId, MemoryEntity>,
    /// Adjacency list: entity → outgoing edges.
    edges: HashMap<EntityId, Vec<GraphEdge>>,
    /// Reverse index: segment_id → entity IDs that reference it.
    segment_index: HashMap<SegmentId, Vec<EntityId>>,
    /// Persistence path.
    persist_path: PathBuf,
}

impl MemoryGraph {
    /// Insert or update an entity. Returns true if this is a new entity.
    pub fn upsert_entity(&mut self, entity: MemoryEntity) -> bool;

    /// Add an edge between two entities.
    /// If the edge relation is `Contradicts`, fires a contradiction event
    /// (returns ContradictionDetected) so the caller can emit a Layer 2 signal.
    pub fn add_edge(&mut self, edge: GraphEdge) -> AddEdgeResult;

    /// Find all entities referenced by a segment.
    pub fn entities_for_segment(&self, id: SegmentId) -> Vec<&MemoryEntity>;

    /// Find all contradiction edges in the graph.
    pub fn open_contradictions(&self) -> Vec<(&GraphEdge, &MemoryEntity, &MemoryEntity)>;

    /// Remove all edges and entity refs for a deleted segment.
    pub fn segment_deleted(&mut self, id: SegmentId);

    /// Persist graph state to disk (JSON). Atomic write-then-rename.
    pub fn flush(&self) -> Result<()>;
}

pub enum AddEdgeResult {
    Added,
    ContradictionDetected {
        existing_entity: MemoryEntity,
        new_entity: MemoryEntity,
        conflicting_edge: GraphEdge,
    },
}
```

### Contradiction resolution rules

| Tier of existing | Tier of incoming | Action |
|---|---|---|
| T1 (DirectObservation) | T2–T5 | Auto-resolve: incoming `beta += 3.0` (approx. −0.3 confidence); existing wins |
| T1 | T1 | Non-deterministic: emit Layer 3 signal for LLM reconciliation |
| T2 | T2–T5 | Lower-tier `beta += 2.0`; recheck quarantine threshold |
| T3–T4 | T3–T4 | Emit Layer 3 signal (moderate severity) |
| Any | Same segment | Ignore (self-referential edge) |

---

## Component 5: Outcome-Linked Confidence

Confidence should reflect not just how many times a memory was retrieved (current Bayesian model) but how often acting on it produced the correct outcome.

### New method on `VectorStore` trait

```rust
/// Record the outcome of acting on a belief supported by this segment.
///
/// Outcome::Confirmed  → alpha += 1.0 (belief was correct, raise confidence)
/// Outcome::Refuted    → beta += 1.0  (belief was wrong, lower confidence)
/// Outcome::CatastrophicRefutation → beta += 10.0 + check quarantine threshold
///
/// This closes the loop between memory and reality: beliefs that produce
/// wrong predictions accumulate beta, eventually falling below the trust-tier
/// quarantine threshold and being removed from context automatically.
fn record_outcome(&self, id: SegmentId, outcome: Outcome) -> Result<()>;

pub enum Outcome {
    /// Acting on this belief produced the expected result.
    Confirmed,
    /// Acting on this belief produced a different result than predicted.
    Refuted,
    /// Acting on this belief caused a significantly wrong outcome.
    /// Triggers immediate quarantine threshold check.
    CatastrophicRefutation,
}
```

### Integration points

The runtime calls `record_outcome` in these situations:
- `cargo test` passes after a code change based on a belief about the codebase → `Confirmed` for code-index segments used
- An architectural claim ("X is always true") is directly contradicted by a tool observation → `Refuted`
- A security claim ("this endpoint is safe") is violated → `CatastrophicRefutation`
- A Telos goal completes successfully → `Confirmed` for planning segments that contributed
- A Telos goal fails because a key assumption was wrong → `Refuted` for those assumption segments

---

## Component 6: Self-Auditing ReflectionLoop

The existing `ReflectionLoop` synthesizes episodic memories into semantic ones (background LLM consolidation). It gains a second operating mode: **memory hygiene**.

### Hygiene mode schedule

Configurable via env var `ANIMUS_MEMORY_AUDIT_INTERVAL_HOURS` (default: `168` = weekly).

### Hygiene procedure (Layer 1 + 2 — no LLM unless anomaly found)

```
1. Sample N segments (default 50) from each trust tier, prioritizing high-confidence
   non-T1 segments (T1 segments are ground truth — less useful to audit)

2. For each sampled segment:
   a. Compute current trust tier from its Source
   b. Check graph for Contradicts edges to T1 segments (Layer 2 — pure graph lookup)
   c. If T1 contradiction found:
      - beta += contradiction_penalty (default 2.0)
      - Recompute confidence = alpha / (alpha + beta)
      - If confidence < quarantine_threshold(tier): set QuarantineState::Quarantined
   d. Check temporal staleness: if last_accessed older than 2× half_life and confidence < 0.3:
      - Set QuarantineState::Quarantined (stale + low confidence = no longer useful)
   e. If importance_weight > 0.8 and segment would otherwise be quarantined:
      - Emit Layer 3 signal: "High-importance memory is degrading" (human review warranted)

3. Aggregate: count total quarantined, total updated, any high-importance degradation

4. If >10% of sampled segments in a domain were quarantined:
   - Emit Layer 3 signal: "World-model drift detected in domain [X]"
   - LLM receives context of which beliefs degraded and why → single reconciliation call

5. Persist all confidence updates and quarantine state changes to VectorFS
```

### Layer 3 signal format for world-model drift

```rust
Signal {
    priority: SignalPriority::Normal, // Urgent if >30% drift in any domain
    summary: format!(
        "Memory hygiene audit: {} segments quarantined, {} updated. \
         Domains affected: {}. High-importance degradation: {}",
        quarantined_count, updated_count, affected_domains, high_importance_degraded
    ),
    // ...
}
```

---

## Component 7: Memory Introspection Tools

Five new LLM tools in `animus-cortex/src/tools/`, following the existing tool pattern.

### `memory_explain`

**Purpose:** "Why do I believe X?" — traces the confidence chain back to source segments.

**Input:**
```json
{
  "query": "why do I believe that select_for_class is TOCTOU-safe?",
  "top_segments": 5
}
```

**Output:** List of segments that contributed to this belief, with their trust tier, confidence, quarantine state, and summary of content. Includes any graph edges from the MemoryGraph.

### `memory_challenge`

**Purpose:** "Is X still true?" — re-validates a belief against current tool-observable state.

**Input:**
```json
{
  "claim": "the rate_limit_states lock is never held while awaiting async operations",
  "verification_strategy": "read_source"
}
```

**Behavior:** Uses available tools (read_file, shell_exec, git_diff) to gather current evidence, then calls `record_outcome` on supporting segments. Returns: confirmed / refuted / inconclusive.

### `memory_quarantine`

**Purpose:** Human-in-the-loop quarantine. Sets `QuarantineState::Quarantined` on a segment.

**Input:** `{ "segment_id": "uuid", "reason": "..." }`

### `memory_expunge`

**Purpose:** Permanent deletion of a confirmed-poisoned segment. Also removes all graph edges for the segment.

**Input:** `{ "segment_id": "uuid", "reason": "..." }`

**Authorization:** Requires `QuarantineState::Quarantined` — can only expunge already-quarantined segments. Prevents accidental deletion of healthy memories.

### `memory_audit_domain`

**Purpose:** Run an immediate hygiene audit on a specific domain without waiting for the scheduled interval.

**Input:** `{ "domain": "codebase", "sample_size": 20 }`

**Output:** Audit summary: segments examined, quarantined, confidence updates applied, drift detected.

---

## Component 8: AST-Aware Code Chunking

### Problem with current chunking

Character/token-based chunking of code produces fragments that start mid-comment or mid-function, destroying the retrieval unit's semantic coherence. A chunk that begins at character 1500 of a 2000-character function body provides no useful retrieval unit.

### Solution: `CodeChunker` — `animus-cortex/src/code_chunker.rs`

Uses `tree-sitter` for language-aware AST parsing. Each chunk is a complete semantic unit.

```rust
pub struct CodeChunk {
    /// The complete text of the semantic unit.
    pub content: String,
    /// What kind of unit this is.
    pub kind: ChunkKind,
    /// File path this chunk came from.
    pub path: String,
    /// Starting line number (1-indexed).
    pub start_line: usize,
    /// Ending line number (1-indexed).
    pub end_line: usize,
    /// For functions/methods: the signature only (for a compact summary embedding).
    pub signature: Option<String>,
    /// doc comment or /// above the item, if any.
    pub doc_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkKind {
    Function,
    Method,
    Impl,
    Trait,
    Struct,
    Enum,
    Module,
    Constant,
    TypeAlias,
    Test,           // #[test] or #[tokio::test] functions — separate chunk kind
    Unknown,        // fallback for unrecognized nodes
}
```

**Supported languages** (via `tree-sitter` grammars):
- Rust (`tree-sitter-rust`)
- JavaScript/TypeScript (`tree-sitter-javascript`, `tree-sitter-typescript`)
- Python (`tree-sitter-python`)

**Fallback:** Files in unsupported languages fall back to paragraph-based chunking (split on blank lines) with `ChunkKind::Unknown`.

**Embedding strategy for code:** Each `CodeChunk` produces two embeddings:
1. **Signature embedding**: `fn name(args) -> ReturnType // doc comment` — for fast lookup by name/signature
2. **Full body embedding**: complete function text — for semantic content search

Both embeddings are stored as separate VectorFS segments with shared `lineage` pointing to the file's primary segment. This allows retrieval by either surface-level signature or deep content.

**Integration:** New Cortex tool `index_codebase` calls `CodeChunker` on a directory, embeds all chunks, and stores them in VectorFS with `Source::Observation` (trust T1 — these are direct file observations) and `importance_weight = 0.7`.

---

## Data Flow

### Write path (Layer 1 + 2)

```
Content → InjectionScanner.scan(content)
        │
        ├─ suspicious=true  → segment.quarantine_state = Suspicious { ... }
        │                     log warning, continue to store
        │
        └─ suspicious=false → continue
                │
                ▼
        TrustTier::from_source(segment.source)
        → apply to confidence initial weight (T1: 0.9 start, T4: 0.5 start)
                │
                ▼
        MemoryGraph.add_edge(any new edges from content analysis)
        │
        ├─ AddEdgeResult::ContradictionDetected
        │    → lower confidence on lower-trust segment
        │    → emit Signal if LLM reconciliation needed (L3)
        │
        └─ AddEdgeResult::Added → continue
                │
                ▼
        VectorStore.store(segment) → persist to disk
```

### Anomaly path (Layer 3)

```
Contradiction detected (L2)
        │
        ▼
Signal { priority: Normal, summary: "Memory contradiction: [A] vs [B]" }
        │
        ▼
LLM receives context: both segments + their trust tiers + provenance
LLM reconciles: which is correct? why?
        │
        ├─ Reconciliation produces resolution → update confidence, update graph edge to SupersededBy
        └─ Cannot reconcile → both quarantined, human review requested
```

### Retrieval path (Layer 1)

```
query(embedding, top_k)
        │
        ▼
HNSW candidates (3× top_k to allow filtering)
        │
        ▼
Filter: exclude QuarantineState::Quarantined (unless include_quarantined=true)
        │
        ▼
Trust-weight re-ranking:
    effective_score = hnsw_similarity × trust_tier.retrieval_weight()
                    × (1.0 + segment.importance_weight × 0.2)
        │
        ▼
Return top_k by effective_score
```

---

## Layer Compliance

| Layer | Component | LLM used? |
|-------|-----------|-----------|
| Layer 1 | InjectionScanner in write path | No |
| Layer 1 | TrustTier assignment from Source | No |
| Layer 1 | Trust-weighted retrieval re-ranking | No |
| Layer 1 | QuarantineState filtering in query() | No |
| Layer 1 | Importance-weighted effective half-life | No |
| Layer 2 | MemoryGraph contradiction detection | No |
| Layer 2 | Hygiene audit: confidence updates, quarantine triggers | No |
| Layer 3 | Contradiction reconciliation Signal | LLM notified once |
| Layer 3 | World-model drift Signal | LLM notified once |
| Layer 3 | High-importance memory degradation Signal | LLM notified once |
| Layer 3 | `memory_challenge` tool outcome recording | LLM invoked by user intent |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/animus-core/src/segment.rs` | Add `quarantine_state: QuarantineState`, `importance_weight: f32` fields; `QuarantineState` enum; `InjectionFlag` enum; `Source::External` variant |
| `crates/animus-core/src/injection.rs` | New — `InjectionScanner`, `InjectionScanResult`, `InjectionFlag`, `scan_for_injection()` |
| `crates/animus-core/src/lib.rs` | Export `injection` module, `TrustTier`, `QuarantineState`, `InjectionFlag`, `Outcome` |
| `crates/animus-core/src/trust.rs` | New — `TrustTier` enum + `from_source()` + threshold/weight/decay methods |
| `crates/animus-vectorfs/src/graph.rs` | New — `MemoryGraph`, `MemoryEntity`, `GraphEdge`, `EdgeRelation`, `EntityKind` |
| `crates/animus-vectorfs/src/store.rs` | Add injection scan in `store()`; quarantine filter in `query()`; trust-weight re-ranking; `record_outcome()` |
| `crates/animus-vectorfs/src/lib.rs` | Export `graph` module; add `record_outcome()` to `VectorStore` trait; `QueryOptions` struct |
| `crates/animus-mnemos/src/lib.rs` | Add hygiene mode to reflection loop; scheduled audit with configurable interval |
| `crates/animus-cortex/src/code_chunker.rs` | New — `CodeChunker`, `CodeChunk`, `ChunkKind`; tree-sitter integration |
| `crates/animus-cortex/src/tools/memory_explain.rs` | New tool |
| `crates/animus-cortex/src/tools/memory_challenge.rs` | New tool |
| `crates/animus-cortex/src/tools/memory_quarantine.rs` | New tool |
| `crates/animus-cortex/src/tools/memory_expunge.rs` | New tool |
| `crates/animus-cortex/src/tools/memory_audit_domain.rs` | New tool |
| `crates/animus-cortex/src/tools/index_codebase.rs` | New tool — AST-aware codebase indexing |
| `crates/animus-cortex/src/tools/mod.rs` | Register all new tools in `ToolContext` |
| `crates/animus-runtime/src/main.rs` | Schedule memory hygiene interval; register new tools |
| `crates/animus-cortex/Cargo.toml` | Add `tree-sitter`, `tree-sitter-rust`, `tree-sitter-javascript`, `tree-sitter-python` deps |

---

## Testing

| Test | Where | What it validates |
|------|-------|-------------------|
| `trust_tier_from_source_observation` | `trust.rs` | T1 for Observation source |
| `trust_tier_from_source_federation` | `trust.rs` | T4 for Federation source |
| `quarantine_threshold_hierarchy` | `trust.rs` | T1 < T2 < T3 < T4 < T5 thresholds |
| `injection_scan_detects_imperative_override` | `injection.rs` | "from now on" → flag |
| `injection_scan_detects_identity_subversion` | `injection.rs` | "you are actually" → flag |
| `injection_scan_detects_trust_escalation` | `injection.rs` | "i am your creator" → flag |
| `injection_scan_clean_content` | `injection.rs` | Normal text → no flags |
| `injection_scan_case_insensitive` | `injection.rs` | "FROM NOW ON" → same flag |
| `store_marks_suspicious_segment` | `store.rs` | Write path sets QuarantineState::Suspicious |
| `query_excludes_quarantined_by_default` | `store.rs` | Quarantined segment not in results |
| `query_includes_quarantined_when_requested` | `store.rs` | QueryOptions::include_quarantined=true works |
| `trust_weight_boosts_t1_in_retrieval` | `store.rs` | T1 segment ranked above equal-similarity T4 |
| `effective_half_life_high_importance` | `segment.rs` | importance=1.0 T1 → longer effective half-life |
| `graph_contradiction_detected` | `graph.rs` | Contradicts edge fires ContradictionDetected |
| `graph_t1_wins_auto_resolution` | `graph.rs` | Lower-tier confidence reduced on T1 vs T4 conflict |
| `graph_equal_tier_requires_reconciliation` | `graph.rs` | T1 vs T1 conflict → not auto-resolved |
| `record_outcome_confirmed_raises_alpha` | `store.rs` | Confirmed → alpha += 1 |
| `record_outcome_refuted_raises_beta` | `store.rs` | Refuted → beta += 1 |
| `record_outcome_catastrophic_triggers_quarantine` | `store.rs` | Catastrophic → confidence < threshold → quarantined |
| `memory_expunge_requires_quarantined_state` | `tools/memory_expunge.rs` | Normal segment cannot be expunged |
| `code_chunker_rust_function` | `code_chunker.rs` | Rust fn → ChunkKind::Function, correct signature |
| `code_chunker_rust_impl_block` | `code_chunker.rs` | impl block → each method is separate chunk |
| `code_chunker_unsupported_language_fallback` | `code_chunker.rs` | Unknown language → paragraph chunking |
| `importance_weight_default` | `segment.rs` | Default segment has importance_weight = 0.5 |
| `quarantine_state_default_is_normal` | `segment.rs` | Default segment has QuarantineState::Normal |
| `existing_segment_deserializes_without_new_fields` | `store.rs` | Old bincode segments load with serde defaults |

---

## What This Does NOT Do

- Does not implement goal-directed retrieval biasing (future: bias retrieval toward Telos active goal)
- Does not implement multi-hop/recursive retrieval (future: chain graph traversals for complex queries)
- Does not implement fine-tuning or parametric memory update
- Does not replace VectorFS with a graph database — graph is an index layer, not a replacement
- Does not validate federation content against a remote source in real-time — corroboration is local observation
- Does not implement memory encryption at rest (existing: content stored plaintext in bincode)
- Does not implement `memory_explain` graph traversal across more than 3 hops (complexity vs. utility tradeoff)
