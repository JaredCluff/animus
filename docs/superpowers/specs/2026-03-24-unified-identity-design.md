# Animus Unified Identity & Attention Model

**Date**: 2026-03-24
**Status**: Approved
**Scope**: Unified identity across channels, situational awareness, principal registry, delegation correlation, memory quality gate

---

## Problem

Animus currently creates separate isolated reasoning threads per channel conversation (`telegram:8593276557`, `nats:animus.out.claude`). This makes Animus behave like multiple separate instances rather than one persistent AI life form. When Jared starts a conversation on Telegram and continues on NATS, Animus has no continuity. When it delegates a task to Claude Code and Claude responds, the response lands in an unrelated thread.

The root cause: **conversation identity is tied to channel** when it should be tied to the principal (the person or agent).

---

## Design Principles

1. **One Animus** — singular identity, not per-channel instances. Threads are attention windows, not separate identities.
2. **Attention shifts, identity doesn't** — like a person who can hold multiple conversations and shift focus without becoming a different person.
3. **Peripheral awareness** — even while focused on one conversation, Animus is dimly aware of all other active conversations.
4. **Channels are delivery rails** — Telegram, NATS, terminal are transports, not identities.
5. **Memory stays clean** — noise is filtered at write time, not pruned after the fact.

---

## Component 1: Shared Identity Context

**What**: `AnimusIdentity` (already exists) is injected into every thread's system context identically — not just the primary terminal thread.

**Change**: Every conversation thread gets the same "who I am" preamble: instance ID, name, purpose, owner. Threads are attention foci that share one identity, not separate agents.

**Files**: `animus-runtime/src/main.rs` — system prompt construction per thread.

---

## Component 2: Situational Awareness

**What**: A lightweight `SituationalAwareness` component maintains a live summary of all active conversations. This summary is injected into the active thread's context at every reasoning turn as "peripheral awareness."

**Format** (injected as a section in the system prompt):
```
## Active Conversations (peripheral awareness)
• jared [telegram] — discussing memory architecture — active 2m ago
• claude-code [nats] — awaiting response on memory protection task — 8m ago
• reflection — idle
```

**Sizing**:
- Dynamic — one line per active conversation (~15 tokens each)
- Total budget: 200–500 tokens, scales with active conversation count
- Peripheral awareness is the **last** content added to context and **first** compressed when space is tight
- Compression: shorten summaries, then group idle/old threads as "N idle threads"

**Aging**:
- Only conversations active within a **recency window** (default: 24h) appear in peripheral awareness
- Conversations older than the window are not tracked in peripheral awareness — they exist in VectorFS and can be recalled on demand
- Each conversation entry tracks `last_active: DateTime<Utc>`

**New type**: `SituationalAwareness` in `animus-cortex/src/situational_awareness.rs`
```rust
pub struct ConversationSummary {
    pub principal_id: String,
    pub channel: String,
    pub summary: String,       // one-line topic description
    pub status: ConvStatus,    // Active | Waiting | Idle
    pub last_active: DateTime<Utc>,
}

pub enum ConvStatus {
    Active,    // currently focused here
    Waiting,   // sent a message, awaiting response
    Idle,      // no recent activity
}

// Note: ConvStatus is entirely independent of ThreadStatus (Active/Suspended/Background/Completed).
// ConvStatus tracks conversation-level activity for peripheral awareness only, not thread lifecycle.
```

**Files**: NEW `animus-cortex/src/situational_awareness.rs`, `animus-runtime/src/main.rs`

---

## Component 3: Principal Registry

**What**: A config-driven mapping from channel-specific identifiers to stable named principals with roles.

**Config** (in `config.toml`):
```toml
[[channels.principals]]
id = "jared"
role = "owner"
channels = ["telegram:8593276557", "terminal"]

[[channels.principals]]
id = "claude-code"
role = "ai-agent"
channels = ["nats:animus.in.claude"]
```

**Resolution**: When a message arrives, Animus resolves the sender's principal before thread lookup:
1. Build lookup key: `"telegram:8593276557"`
2. Match against principal channel bindings → `principal_id = "jared"`
3. Look up or create conversation thread keyed on `principal_id`, not `channel:thread_id`
4. Channel stored as reply metadata only

**Fallback**: Messages from unregistered senders use the old `channel:thread_id` key (backward compatible).

**Principal roles**:
- `owner` — Jared; highest trust, full autonomy access
- `ai-agent` — Claude Code or other AI peers; Acts-level trust by default
- `peer` — other humans; Normal trust
- `system` — internal system events

**New type**: `PrincipalConfig` in `animus-core/src/config.rs`
**Files**: `animus-core/src/config.rs`, `animus-runtime/src/main.rs`

---

## Component 4: Delegation Correlation

**What**: When Animus delegates work to Claude Code via `nats_publish`, it tags the outbound message with the originating conversation's principal ID. When the NATS response arrives, the handler routes it back to the originating conversation thread instead of spawning a new NATS thread.

**Mechanism**:
- `nats_publish` tool accepts optional `conversation_id` param (or infers from `tool_ctx.active_conversation_id`)
- Outbound NATS payload is wrapped: `{"payload": "...", "x-conversation-id": "jared", "x-reply-subject": "animus.out.claude"}`
- Or: NATS headers (if NATS JetStream headers are available)
- NATS channel handler: on inbound message, check for `x-conversation-id` field; if present, use it as the thread key instead of the reply subject

**Fallback**: If no conversation ID present (e.g., unsolicited NATS message), use default NATS thread key as before.

**Files**: `animus-cortex/src/tools/nats_publish.rs`, `animus-channel/src/nats/mod.rs`, `animus-runtime/src/main.rs`

---

## Component 5: Memory Quality Gate

**What**: A write-time filter on every VectorFS `store()` call that prevents noise from accumulating before it happens.

**Two filters**:

### 5a. Semantic Deduplication
- Before storing a new segment, embed its content and compute cosine similarity against segments written in the last 24h (check top-20 most recent by timestamp)
- If max similarity > **0.92**: skip the write — it's a near-duplicate
- This prevents echo loops and repeated state confirmations from filling memory

### 5b. Null-State Suppression
- Classify incoming content for null-state patterns: "not responding", "silence", "no output", "final", "keepalive failed", empty responses
- If classified as null-state: only store if no similar null-state segment was stored in the **last 1 hour**
- Null-state segments get `DecayClass::Ephemeral` (new variant, 1h half-life) if stored. **Requires adding `Ephemeral` to the `DecayClass` enum and its `half_life_secs()` match arm in `animus-core/src/segment.rs`.**

**Implementation**: Gate lives in a `MemoryQualityGate` wrapper around `VectorStore::store()`, or as a new method `store_with_quality_gate()`.

**Performance**: Similarity check is fast (cosine on cached hot-tier embeddings). For the 24h window, limit check to last 100 segments max to bound latency.

**New type**: `MemoryQualityGate` in `animus-vectorfs/src/quality_gate.rs`
**Files**: NEW `animus-vectorfs/src/quality_gate.rs`, `animus-vectorfs/src/lib.rs`, `animus-runtime/src/main.rs`

---

## Data Flow After Changes

```
Message arrives (any channel)
    ↓
Principal resolution: "telegram:8593276557" → "jared"
    ↓
Thread lookup by principal_id (not channel:thread_id)
    ↓
SituationalAwareness updated: jared → Active
    ↓
Context assembly:
  - Jared's conversation history (hot segments)
  - Peripheral awareness block (other active conversations, aged)
  - Goals, signals
    ↓
Reasoning turn (LLM)
    ↓
Response sent on original channel (delivery metadata)
    ↓
Auto-persist exchange → MemoryQualityGate → VectorFS (if passes)
    ↓
SituationalAwareness updated: jared → Idle
```

---

## Implementation Notes

### Peripheral awareness injection point
The runtime (not `ReasoningThread`) constructs the augmented system prompt by appending the `SituationalAwareness` block to the base system prompt string before calling `run_reasoning_turn`. `ReasoningThread` is deliberately isolated and should not be modified to accept `SituationalAwareness` directly.

### SituationalAwareness status transitions
- Set principal to `Active` immediately before calling `run_reasoning_turn`
- Set principal to `Idle` in a `finally`-style block after `channel_bus.send(outbound)` — this must run even on error so threads do not remain permanently `Active`
- `SituationalAwareness` is **not persisted across restarts** — it is live session state only. VectorFS has the conversation history; peripheral awareness tracks current activity

### `channel_thread_map` key migration
The map key changes from `"channel:thread_id"` → `principal_id` for registered principals. For unregistered senders, the old `"channel:thread_id"` key is preserved in the same map — no fallback regression. The scheduler thread name should be set to the principal ID (or the old key for unregistered senders) for readability in `/threads` output.

### Memory quality gate architecture
`MemoryQualityGate` implements `VectorStore` as a wrapping decorator over the concrete store — **not** a standalone method. This keeps all call sites clean and lets the gate be swapped in/out transparently. The runtime constructs: `Arc<MemoryQualityGate<MmapVectorStore>>` and uses it everywhere `Arc<dyn VectorStore>` is expected.

### Quality gate HNSW query behavior
The similarity check uses the store's existing `query()` (HNSW nearest-neighbor search) — similarity-first, not recency-first. After retrieving top-20 most similar segments, apply a recency post-filter: only consider matches with `created` within the 24h window. If any pass both filters (similarity > threshold AND recent), skip the write. This is efficient — the HNSW call is O(log n) and the post-filter is a scan of 20 results.

### Null-state suppression scope
The null-state filter applies **only** to segments where `source` is `Source::Conversation` or `Source::Manual` (auto-persisted channel exchanges). It does not apply to `Source::SelfDerived`, `Source::Observation`, or `Source::Consolidation` segments — those may legitimately discuss silence or failure.

### Quality gate thresholds in config
Thresholds are configurable via `AnimusConfig` (not hardcoded):
```toml
[vectorfs.quality_gate]
enabled = true
dedup_similarity_threshold = 0.92
dedup_window_hours = 24
null_state_cooldown_minutes = 60
```

## Files Changed

| File | Change |
|------|--------|
| `animus-core/src/config.rs` | Add `PrincipalConfig`, `PrincipalRole`, `QualityGateConfig` under `VectorFsConfig` |
| `animus-core/src/segment.rs` | Add `DecayClass::Ephemeral` variant and `half_life_secs()` match arm (1h = 3600s) |
| `animus-cortex/src/situational_awareness.rs` | NEW — `SituationalAwareness`, `ConversationSummary` |
| `animus-cortex/src/lib.rs` | Export `pub mod situational_awareness` |
| `animus-vectorfs/src/quality_gate.rs` | NEW — `MemoryQualityGate<S: VectorStore>` wrapping decorator |
| `animus-vectorfs/src/lib.rs` | Export `quality_gate` module |
| `animus-cortex/src/tools/nats_publish.rs` | Add optional `conversation_id` param |
| `animus-channel/src/nats/mod.rs` | Read `x-conversation-id` from payload, route accordingly |
| `animus-runtime/src/main.rs` | Principal resolution, situational awareness injection, quality gate wiring |

---

## Out of Scope

- Multi-owner instances (Animus serves one owner, Jared)
- Federation (separate system, separate spec)
- Voice channel (future)
- LoRA/model weight updates (future)
