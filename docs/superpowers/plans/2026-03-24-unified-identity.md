# Unified Identity & Attention Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Animus a single persistent identity across all channels — one mind that shifts attention, not separate instances per channel.

**Architecture:** Five components built bottom-up: (1) DecayClass::Ephemeral decay variant, (2) config types for principals and quality gate, (3) SituationalAwareness for peripheral awareness, (4) MemoryQualityGate to block noise at write time, (5) principal resolution + situational awareness injection wired into main.rs, plus delegation correlation in NATS.

**Tech Stack:** Rust, tokio, serde/toml, chrono, animus-core/cortex/vectorfs/channel/runtime crates

---

## File Map

| File | Change |
|------|--------|
| `crates/animus-core/src/segment.rs` | Add `DecayClass::Ephemeral` |
| `crates/animus-core/src/config.rs` | Add `PrincipalConfig`, `PrincipalRole`, `QualityGateConfig`, wire into `AnimusConfig` |
| `crates/animus-core/src/lib.rs` | Re-export new config types |
| `crates/animus-cortex/src/situational_awareness.rs` | NEW — `SituationalAwareness`, `ConversationSummary`, `ConvStatus` |
| `crates/animus-cortex/src/lib.rs` | Export `pub mod situational_awareness` |
| `crates/animus-vectorfs/src/quality_gate.rs` | NEW — `MemoryQualityGate` |
| `crates/animus-vectorfs/src/lib.rs` | Export `pub mod quality_gate` |
| `crates/animus-cortex/src/tools/nats_publish.rs` | Add optional `conversation_id` param, wrap payload with metadata |
| `crates/animus-channel/src/nats/mod.rs` | Parse `x-conversation-id` from payload, use as thread key when present |
| `crates/animus-runtime/src/main.rs` | Principal resolution, situational awareness injection, quality gate wiring |
| `crates/animus-tests/tests/integration/identity.rs` | NEW — integration tests for principal resolution + situational awareness |

---

## Task 1: Add `DecayClass::Ephemeral`

**Files:**
- Modify: `crates/animus-core/src/segment.rs`

- [ ] **Write the failing test**

Add inside the existing `#[cfg(test)]` block in `segment.rs` (or create one):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ephemeral_has_short_half_life() {
        assert_eq!(DecayClass::Ephemeral.half_life_secs(), 3600.0);
    }
}
```

- [ ] **Run test to confirm it fails**
```
cargo test -p animus-core ephemeral_has_short_half_life
```
Expected: FAIL — `Ephemeral` variant doesn't exist yet.

- [ ] **Add the variant**

In `crates/animus-core/src/segment.rs`, add `Ephemeral` to the enum and match arm:
```rust
pub enum DecayClass {
    Factual,
    Procedural,
    Episodic,
    Opinion,
    #[default]
    General,
    /// Very short-lived noise: keepalive failures, silence loops. Half-life: 1 hour.
    Ephemeral,
}
```
```rust
// in half_life_secs():
Self::Ephemeral => 3600.0,
```

- [ ] **Run test to confirm it passes**
```
cargo test -p animus-core ephemeral_has_short_half_life
```

- [ ] **Full core tests still pass**
```
cargo test -p animus-core
```

---

## Task 2: Config Types — `PrincipalConfig` and `QualityGateConfig`

**Files:**
- Modify: `crates/animus-core/src/config.rs`

- [ ] **Write failing tests** (add to end of config.rs):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principal_resolves_channel_key() {
        let principals = vec![
            PrincipalConfig {
                id: "jared".to_string(),
                role: PrincipalRole::Owner,
                channels: vec!["telegram:8593276557".to_string(), "terminal".to_string()],
            },
        ];
        let key = "telegram:8593276557";
        let found = principals.iter().find(|p| p.channels.iter().any(|c| c == key));
        assert_eq!(found.map(|p| p.id.as_str()), Some("jared"));
    }

    #[test]
    fn quality_gate_defaults_enabled() {
        let cfg = QualityGateConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.dedup_similarity_threshold, 0.92);
        assert_eq!(cfg.dedup_window_hours, 24);
        assert_eq!(cfg.null_state_cooldown_minutes, 60);
    }
}
```

- [ ] **Run to confirm fails**
```
cargo test -p animus-core principal_resolves_channel_key quality_gate_defaults_enabled
```

- [ ] **Add the types** — in `config.rs`, before the `AnimusConfig impl Default`:

```rust
// ---------------------------------------------------------------------------
// Identity — Principals
// ---------------------------------------------------------------------------

/// Role of a known principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalRole {
    /// Instance owner — highest trust.
    Owner,
    /// AI agent peer (e.g., Claude Code).
    AiAgent,
    /// Human peer.
    Peer,
    /// Internal system.
    System,
}

/// A known principal: a stable identity mapped from channel-specific IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalConfig {
    /// Stable identifier (e.g., "jared", "claude-code").
    pub id: String,
    pub role: PrincipalRole,
    /// Channel binding keys in the form "channel_id:sender_id"
    /// (e.g., "telegram:8593276557", "terminal", "nats:animus.in.claude").
    pub channels: Vec<String>,
}

// ---------------------------------------------------------------------------
// Memory Quality Gate
// ---------------------------------------------------------------------------

/// Configures the write-time memory quality filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGateConfig {
    /// Enable/disable the quality gate entirely.
    pub enabled: bool,
    /// Cosine similarity threshold above which a write is considered a duplicate (0.0–1.0).
    pub dedup_similarity_threshold: f32,
    /// Window in hours within which dedup is checked.
    pub dedup_window_hours: u64,
    /// Cooldown in minutes for null-state segments (silence, keepalive failures).
    pub null_state_cooldown_minutes: u64,
}

impl Default for QualityGateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dedup_similarity_threshold: 0.92,
            dedup_window_hours: 24,
            null_state_cooldown_minutes: 60,
        }
    }
}
```

- [ ] **Add `principals` to `ChannelsConfig` and `quality_gate` to `VectorFSConfig`:**

```rust
// In ChannelsConfig:
pub struct ChannelsConfig {
    pub telegram: TelegramChannelConfig,
    pub http_api: HttpApiChannelConfig,
    pub nats: NatsChannelConfig,
    #[serde(default)]
    pub principals: Vec<PrincipalConfig>,
}

// In VectorFSConfig:
pub struct VectorFSConfig {
    pub dimensionality: usize,
    pub max_segments: usize,
    #[serde(default)]
    pub quality_gate: QualityGateConfig,
}
```

- [ ] **Run tests to confirm pass**
```
cargo test -p animus-core
```

---

## Task 3: `SituationalAwareness` Component

**Files:**
- Create: `crates/animus-cortex/src/situational_awareness.rs`
- Modify: `crates/animus-cortex/src/lib.rs`

- [ ] **Write failing tests** — create the file with tests first:

```rust
// crates/animus-cortex/src/situational_awareness.rs
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvStatus {
    Active,
    Waiting,
    Idle,
}
// Note: ConvStatus is independent of ThreadStatus — it tracks conversation-level
// activity for peripheral awareness only, not thread lifecycle.

#[derive(Debug, Clone)]
pub struct ConversationSummary {
    pub principal_id: String,
    pub channel: String,
    pub summary: String,
    pub status: ConvStatus,
    pub last_active: DateTime<Utc>,
}

pub struct SituationalAwareness {
    entries: std::collections::HashMap<String, ConversationSummary>,
    recency_hours: u64,
}

impl SituationalAwareness {
    pub fn new(recency_hours: u64) -> Self {
        Self { entries: Default::default(), recency_hours }
    }

    pub fn set_active(&mut self, principal_id: &str, channel: &str, summary: &str) {
        let entry = self.entries.entry(principal_id.to_string()).or_insert_with(|| ConversationSummary {
            principal_id: principal_id.to_string(),
            channel: channel.to_string(),
            summary: summary.to_string(),
            status: ConvStatus::Active,
            last_active: Utc::now(),
        });
        entry.status = ConvStatus::Active;
        entry.channel = channel.to_string();
        entry.summary = summary.to_string();
        entry.last_active = Utc::now();
    }

    pub fn set_idle(&mut self, principal_id: &str) {
        if let Some(entry) = self.entries.get_mut(principal_id) {
            entry.status = ConvStatus::Idle;
            entry.last_active = Utc::now();
        }
    }

    pub fn set_waiting(&mut self, principal_id: &str) {
        if let Some(entry) = self.entries.get_mut(principal_id) {
            entry.status = ConvStatus::Waiting;
            entry.last_active = Utc::now();
        }
    }

    /// Generate the peripheral awareness block for injection into the system prompt.
    /// Only includes entries active within the recency window.
    /// The `current_principal` entry is labeled "(current focus)".
    pub fn render(&self, current_principal: &str, max_tokens_approx: usize) -> String {
        let cutoff = Utc::now() - chrono::Duration::hours(self.recency_hours as i64);
        let mut lines: Vec<String> = self.entries.values()
            .filter(|e| e.last_active >= cutoff)
            .map(|e| {
                let status_label = if e.principal_id == current_principal {
                    "current focus".to_string()
                } else {
                    match e.status {
                        ConvStatus::Active => "active".to_string(),
                        ConvStatus::Waiting => "awaiting response".to_string(),
                        ConvStatus::Idle => "idle".to_string(),
                    }
                };
                let age = Utc::now().signed_duration_since(e.last_active);
                let age_str = if age.num_minutes() < 1 { "just now".to_string() }
                    else if age.num_hours() < 1 { format!("{}m ago", age.num_minutes()) }
                    else { format!("{}h ago", age.num_hours()) };
                format!("• {} [{}] — {} — {} ({})", e.principal_id, e.channel, e.summary, status_label, age_str)
            })
            .collect();
        lines.sort(); // deterministic ordering

        // Approximate token budget: ~4 chars/token
        let budget_chars = max_tokens_approx * 4;
        let mut result = String::from("## Active Conversations\n");
        for line in &lines {
            if result.len() + line.len() + 1 > budget_chars {
                result.push_str(&format!("• ({} more, within {}h)\n", lines.len() - result.lines().count() + 1, self.recency_hours));
                break;
            }
            result.push_str(line);
            result.push('\n');
        }
        result
    }

    pub fn active_count(&self) -> usize {
        let cutoff = Utc::now() - chrono::Duration::hours(self.recency_hours as i64);
        self.entries.values().filter(|e| e.last_active >= cutoff).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_active_entries() {
        let mut sa = SituationalAwareness::new(24);
        sa.set_active("jared", "telegram", "discussing identity design");
        sa.set_waiting("claude-code");  // sets idle because it doesn't exist yet
        sa.set_active("claude-code", "nats", "memory protection task");
        sa.set_waiting("claude-code");
        let output = sa.render("jared", 500);
        assert!(output.contains("jared"));
        assert!(output.contains("current focus"));
        assert!(output.contains("claude-code"));
        assert!(output.contains("awaiting response"));
    }

    #[test]
    fn render_excludes_old_entries() {
        let mut sa = SituationalAwareness::new(1); // 1 hour window
        sa.set_active("jared", "telegram", "old conversation");
        // Force last_active to be 2 hours ago
        if let Some(entry) = sa.entries.get_mut("jared") {
            entry.last_active = Utc::now() - chrono::Duration::hours(2);
        }
        let output = sa.render("other", 500);
        assert!(!output.contains("jared"));
    }

    #[test]
    fn set_idle_marks_correctly() {
        let mut sa = SituationalAwareness::new(24);
        sa.set_active("jared", "telegram", "test");
        sa.set_idle("jared");
        assert_eq!(sa.entries["jared"].status, ConvStatus::Idle);
    }
}
```

- [ ] **Add module to `animus-cortex/src/lib.rs`:**
```rust
pub mod situational_awareness;
pub use situational_awareness::SituationalAwareness;
```

- [ ] **Run tests to confirm pass**
```
cargo test -p animus-cortex situational_awareness
```

---

## Task 4: `MemoryQualityGate`

**Files:**
- Create: `crates/animus-vectorfs/src/quality_gate.rs`
- Modify: `crates/animus-vectorfs/src/lib.rs`

- [ ] **Create `quality_gate.rs` with tests:**

```rust
// crates/animus-vectorfs/src/quality_gate.rs
use animus_core::{
    segment::{Content, DecayClass, Source},
    Result, Segment, SegmentId, Tier,
};
use animus_core::config::QualityGateConfig;
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use crate::{VectorStore, SegmentUpdate};

/// Null-state patterns: transient failures that don't need repeated storage.
const NULL_STATE_PATTERNS: &[&str] = &[
    "not responding", "silence", "no output", "final silence",
    "keepalive failed", "no response", "no more output",
    "conversation closed", "thread closed", "loop terminated",
];

fn is_null_state(text: &str) -> bool {
    let lower = text.to_lowercase();
    NULL_STATE_PATTERNS.iter().any(|p| lower.contains(p))
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 { 0.0 } else { dot / (mag_a * mag_b) }
}

/// Wrapping VectorStore decorator that applies quality filtering before writes.
/// Pass-through for all other operations.
pub struct MemoryQualityGate {
    inner: Arc<dyn VectorStore>,
    config: QualityGateConfig,
}

impl MemoryQualityGate {
    pub fn new(inner: Arc<dyn VectorStore>, config: QualityGateConfig) -> Self {
        Self { inner, config }
    }
}

impl VectorStore for MemoryQualityGate {
    fn store(&self, segment: Segment) -> Result<SegmentId> {
        if !self.config.enabled {
            return self.inner.store(segment);
        }
        // Only filter channel-sourced segments (Conversation or Manual).
        let is_channel_source = matches!(
            &segment.source,
            Source::Conversation { .. } | Source::Manual { .. }
        );
        if !is_channel_source {
            return self.inner.store(segment);
        }

        let window = chrono::Duration::hours(self.config.dedup_window_hours as i64);
        let dedup_cutoff = Utc::now() - window;

        // Query for the 20 most similar existing segments.
        let candidates = self.inner.query(&segment.embedding, 20, None).unwrap_or_default();
        let recent_similar: Vec<&Segment> = candidates.iter()
            .filter(|s| s.created >= dedup_cutoff)
            .collect();

        // 1. Semantic deduplication: skip if near-duplicate exists within window.
        for s in &recent_similar {
            let sim = cosine_similarity(&segment.embedding, &s.embedding);
            if sim >= self.config.dedup_similarity_threshold {
                tracing::debug!(
                    "MemoryQualityGate: dedup skip (similarity={:.3}, threshold={:.3})",
                    sim, self.config.dedup_similarity_threshold
                );
                return Ok(segment.id);
            }
        }

        // 2. Null-state suppression (Conversation/Manual text segments only).
        if let Content::Text(ref text) = segment.content {
            if is_null_state(text) {
                let cooldown = chrono::Duration::minutes(self.config.null_state_cooldown_minutes as i64);
                let cooldown_cutoff = Utc::now() - cooldown;
                let has_recent_null = candidates.iter().any(|s| {
                    s.created >= cooldown_cutoff
                        && matches!(&s.content, Content::Text(t) if is_null_state(t))
                });
                if has_recent_null {
                    tracing::debug!("MemoryQualityGate: null-state cooldown skip");
                    return Ok(segment.id);
                }
                // Store it as Ephemeral (short-lived).
                let mut seg = segment;
                seg.decay_class = DecayClass::Ephemeral;
                return self.inner.store(seg);
            }
        }

        self.inner.store(segment)
    }

    fn query(&self, embedding: &[f32], top_k: usize, tier_filter: Option<Tier>) -> Result<Vec<Segment>> {
        self.inner.query(embedding, top_k, tier_filter)
    }
    fn get(&self, id: SegmentId) -> Result<Option<Segment>> { self.inner.get(id) }
    fn get_raw(&self, id: SegmentId) -> Result<Option<Segment>> { self.inner.get_raw(id) }
    fn update_meta(&self, id: SegmentId, update: SegmentUpdate) -> Result<()> { self.inner.update_meta(id, update) }
    fn set_tier(&self, id: SegmentId, tier: Tier) -> Result<()> { self.inner.set_tier(id, tier) }
    fn delete(&self, id: SegmentId) -> Result<()> { self.inner.delete(id) }
    fn merge(&self, source_ids: Vec<SegmentId>, merged: Segment) -> Result<SegmentId> { self.inner.merge(source_ids, merged) }
    fn count(&self, tier_filter: Option<Tier>) -> usize { self.inner.count(tier_filter) }
    fn segment_ids(&self, tier_filter: Option<Tier>) -> Vec<SegmentId> { self.inner.segment_ids(tier_filter) }
    fn snapshot(&self, snapshot_dir: &Path) -> Result<usize> { self.inner.snapshot(snapshot_dir) }
    fn restore_from_snapshot(&self, snapshot_dir: &Path) -> Result<usize> { self.inner.restore_from_snapshot(snapshot_dir) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::segment::{Content, Source};
    use animus_embed::synthetic::SyntheticEmbedding;
    use animus_core::EmbeddingService;
    use crate::store::MmapVectorStore;

    async fn make_gate() -> (MemoryQualityGate, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let raw = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
        let cfg = QualityGateConfig::default();
        (MemoryQualityGate::new(raw as Arc<dyn VectorStore>, cfg), tmp)
    }

    fn make_segment(content: &str, embedding: Vec<f32>) -> Segment {
        Segment::new(
            Content::Text(content.to_string()),
            embedding,
            Source::Manual { description: "test".to_string() },
        )
    }

    #[tokio::test]
    async fn dedup_blocks_near_identical() {
        let (gate, _tmp) = make_gate().await;
        let emb = vec![1.0_f32, 0.0, 0.0, 0.0];
        let s1 = make_segment("hello world", emb.clone());
        let id1 = gate.store(s1).unwrap();
        // Second segment with nearly identical embedding
        let s2 = make_segment("hello world slightly different", vec![0.999, 0.001, 0.0, 0.0]);
        let id2 = gate.store(s2).unwrap();
        // id2 should equal s2.id (skipped) but count should still be 1
        assert_eq!(gate.count(None), 1);
        let _ = (id1, id2);
    }

    #[tokio::test]
    async fn unique_content_passes_through() {
        let (gate, _tmp) = make_gate().await;
        let s1 = make_segment("topic A", vec![1.0, 0.0, 0.0, 0.0]);
        let s2 = make_segment("topic B", vec![0.0, 1.0, 0.0, 0.0]);
        gate.store(s1).unwrap();
        gate.store(s2).unwrap();
        assert_eq!(gate.count(None), 2);
    }

    #[tokio::test]
    async fn null_state_stored_as_ephemeral() {
        let (gate, _tmp) = make_gate().await;
        let s = make_segment("silence — not responding", vec![1.0, 0.0, 0.0, 0.0]);
        let id = gate.store(s).unwrap();
        let stored = gate.get(id).unwrap().unwrap();
        assert_eq!(stored.decay_class, DecayClass::Ephemeral);
    }

    #[tokio::test]
    async fn null_state_deduped_within_cooldown() {
        let (gate, _tmp) = make_gate().await;
        let s1 = make_segment("silence — not responding", vec![1.0, 0.0, 0.0, 0.0]);
        gate.store(s1).unwrap();
        // Second null-state soon after — should be skipped
        let s2 = make_segment("silence — keepalive failed", vec![0.99, 0.0, 0.0, 0.01]);
        gate.store(s2).unwrap();
        assert_eq!(gate.count(None), 1);
    }
}
```

- [ ] **Export from `animus-vectorfs/src/lib.rs`:**
```rust
pub mod quality_gate;
pub use quality_gate::MemoryQualityGate;
```

- [ ] **Run tests**
```
cargo test -p animus-vectorfs quality_gate
```

---

## Task 5: Delegation Correlation — NATS payload wrapping

**Files:**
- Modify: `crates/animus-cortex/src/tools/nats_publish.rs`
- Modify: `crates/animus-channel/src/nats/mod.rs`

- [ ] **Update `nats_publish.rs`** — add optional `conversation_id` parameter and wrap payload:

```rust
// In parameters_schema, add:
"conversation_id": {
    "type": "string",
    "description": "Optional: principal ID of the originating conversation (e.g. 'jared'). When set, Animus's response will be routed back to that conversation's thread."
}
// required stays ["subject", "payload"]
```

In `execute()`, before publishing:
```rust
let conversation_id = params["conversation_id"].as_str();
let wire_payload = if let Some(cid) = conversation_id {
    // Wrap payload with routing metadata so the responder can route back.
    serde_json::json!({
        "payload": payload,
        "x-conversation-id": cid,
    }).to_string()
} else {
    payload.to_string()
};

match client.publish(subject.to_string(), wire_payload.as_bytes().to_vec().into()).await {
    Ok(()) => Ok(ToolResult {
        content: format!("Published to '{subject}': {}", &payload[..payload.len().min(200)]),
        is_error: false,
    }),
    ...
}
```

- [ ] **Update `nats/mod.rs`** — parse `x-conversation-id` from inbound payload and use it as thread key:

In the inbound message handler, after `let payload = ...`, add:
```rust
// Check if the payload is a wrapped delegation message.
let (actual_payload, conversation_id_override) = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
    if v.get("x-conversation-id").is_some() {
        let inner = v["payload"].as_str().unwrap_or(&payload).to_string();
        let cid = v["x-conversation-id"].as_str().map(|s| s.to_string());
        (inner, cid)
    } else {
        (payload, None)
    }
} else {
    (payload, None)
};
```

Then when building `ChannelMessage`, use `conversation_id_override` as `thread_id` if present:
```rust
let effective_thread_id = conversation_id_override.unwrap_or_else(|| reply_subject.clone());
let mut channel_msg = ChannelMessage::new(
    CHANNEL_ID,
    effective_thread_id,   // routes response back to originating conversation
    sender,
    Some(actual_payload),
);
```

- [ ] **Build to verify**
```
cargo build -p animus-cortex -p animus-channel 2>&1 | grep -E "error|warning: unused"
```

- [ ] **Run existing NATS tests**
```
cargo test -p animus-tests
```

---

## Task 6: Principal Resolution in `main.rs`

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

- [ ] **Add principal resolution helper function** — add near the bottom of `main.rs`, before the `prune_old_snapshots` function:

```rust
/// Resolve a channel message's sender to a principal ID.
/// Returns the principal ID if found in config, otherwise None.
fn resolve_principal<'a>(
    msg: &animus_channel::message::ChannelMessage,
    principals: &'a [animus_core::config::PrincipalConfig],
) -> Option<&'a str> {
    // Build lookup key: "channel_id:sender_channel_user_id"
    // Special case: terminal input always maps to "terminal" key.
    let lookup_key = if msg.channel_id == "terminal" {
        "terminal".to_string()
    } else {
        format!("{}:{}", msg.channel_id, msg.sender.channel_user_id)
    };
    principals.iter()
        .find(|p| p.channels.iter().any(|c| c == &lookup_key))
        .map(|p| p.id.as_str())
}
```

- [ ] **Change `channel_thread_map` key resolution** — in the `RouteDecision::ExistingThread | RouteDecision::NewThread` arm, before the thread lookup:

Find this code block (around line 795):
```rust
RouteDecision::ExistingThread(ref thread_key)
| RouteDecision::NewThread(ref thread_key) => {
    let thread_id = match channel_thread_map.get(thread_key) {
```

Change to:
```rust
RouteDecision::ExistingThread(ref raw_thread_key)
| RouteDecision::NewThread(ref raw_thread_key) => {
    // Resolve to principal ID if known; fall back to raw channel key.
    let thread_key_owned = resolve_principal(&msg, &config.channels.principals)
        .map(|id| id.to_string())
        .unwrap_or_else(|| raw_thread_key.clone());
    let thread_key = &thread_key_owned;
    let thread_id = match channel_thread_map.get(thread_key) {
```

- [ ] **Build to verify**
```
cargo build -p animus-runtime 2>&1 | grep "^error"
```

---

## Task 7: Situational Awareness Injection

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

- [ ] **Add `SituationalAwareness` to the runtime** — after the `channel_bus` and before the main event loop, add:

```rust
// Situational awareness — tracks all active conversations for peripheral awareness.
// Default 24h recency window; not persisted across restarts.
let mut situational_awareness = animus_cortex::SituationalAwareness::new(24);
```

- [ ] **Update `build_system_prompt` to accept peripheral awareness**:

Change the signature:
```rust
fn build_system_prompt(
    _scheduler: &ThreadScheduler<MmapVectorStore>,
    goals: &GoalManager,
    reconstitution_summary: Option<&str>,
    peripheral_awareness: Option<&str>,
) -> String {
```

Add at the end of the function body, before returning:
```rust
if let Some(awareness) = peripheral_awareness {
    if !awareness.trim().is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(awareness);
    }
}
```

- [ ] **Update all callers of `build_system_prompt`** — the function is called in `run_reasoning_turn`. Update `run_reasoning_turn` to accept and pass through peripheral awareness:

```rust
async fn run_reasoning_turn(
    input: &str,
    _images: Option<&[std::path::PathBuf]>,
    scheduler: &mut ThreadScheduler<MmapVectorStore>,
    engine_registry: &EngineRegistry,
    tool_registry: &ToolRegistry,
    tool_ctx: &ToolContext,
    embedder: &dyn animus_core::EmbeddingService,
    tool_definitions: &[animus_cortex::llm::ToolDefinition],
    goals: &Arc<parking_lot::Mutex<GoalManager>>,
    reconstitution_summary: Option<&str>,
    peripheral_awareness: Option<&str>,   // NEW
) -> animus_core::Result<String> {
    let system = {
        let goals_guard = goals.lock();
        build_system_prompt(scheduler, &goals_guard, reconstitution_summary, peripheral_awareness)
    };
```

- [ ] **Update all `run_reasoning_turn` call sites** — there are two (terminal and channel). Add `None` or the rendered awareness as the last argument:

In the **channel message path** (around line 857):
```rust
// Set active before reasoning
situational_awareness.set_active(thread_key, &msg.channel_id, &input_text[..input_text.len().min(80)]);

let awareness_block = if situational_awareness.active_count() > 1 {
    Some(situational_awareness.render(thread_key, 400))
} else {
    None
};

let response = run_reasoning_turn(
    &input_text,
    None,
    &mut scheduler,
    &engine_registry,
    &tool_registry,
    &tool_ctx,
    &*embedder,
    &tool_definitions,
    &goals,
    reconstitution_summary.as_deref(),
    awareness_block.as_deref(),  // NEW
).await;

// Set idle after response sent (must happen even on error)
situational_awareness.set_idle(thread_key);
```

In the **terminal path**, pass `None` for peripheral awareness.

- [ ] **Build to verify**
```
cargo build -p animus-runtime 2>&1 | grep "^error"
```

- [ ] **Run full test suite**
```
cargo test --workspace 2>&1 | tail -20
```

---

## Task 8: Wire Quality Gate into Runtime

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

- [ ] **Wrap store with quality gate** — find where `store` is created (around line 144):

```rust
let store = Arc::new(MmapVectorStore::open(&vectorfs_dir, dimensionality)?);
```

Add after:
```rust
// Wrap with quality gate to filter noise at write time.
// Keep raw_store reference for health endpoint and snapshot functions.
let raw_store = store.clone();
let gated_store: Arc<dyn animus_vectorfs::VectorStore> =
    Arc::new(animus_vectorfs::MemoryQualityGate::new(
        store.clone() as Arc<dyn animus_vectorfs::VectorStore>,
        config.vectorfs.quality_gate.clone(),
    ));
```

- [ ] **Update the auto-persist code** — in the channel message processing block, change `store.store(seg)` to use `gated_store`:

```rust
if let Ok(embedding) = embedder.embed_text(&record).await {
    let mut seg = Segment::new(
        Content::Text(record),
        embedding,
        Source::Manual {
            description: format!("channel:{channel_id} thread:{thread_id_str}"),
        },
    );
    seg.decay_class = DecayClass::Episodic;
    if let Err(e) = gated_store.store(seg) {
        tracing::warn!("Failed to auto-persist channel exchange: {e}");
    }
}
```

- [ ] **Also update the ToolContext** to use gated_store:
```rust
let tool_ctx = ToolContext {
    ...
    store: gated_store.clone(),
    ...
};
```

- [ ] **Build + full test**
```
cargo build -p animus-runtime 2>&1 | grep "^error"
cargo test --workspace 2>&1 | tail -20
```

---

## Task 9: Integration Test

**Files:**
- Create: `crates/animus-tests/tests/integration/identity.rs`
- Modify: `crates/animus-tests/tests/integration/mod.rs` (if exists) or main test file

- [ ] **Write integration tests:**

```rust
// crates/animus-tests/tests/integration/identity.rs
use animus_core::config::{PrincipalConfig, PrincipalRole};
use animus_cortex::situational_awareness::{ConvStatus, SituationalAwareness};
use animus_vectorfs::{quality_gate::MemoryQualityGate, VectorStore};
use animus_vectorfs::store::MmapVectorStore;
use animus_core::segment::{Content, DecayClass, Source, Segment};
use animus_embed::synthetic::SyntheticEmbedding;
use animus_core::EmbeddingService;
use std::sync::Arc;

fn make_store(dir: &std::path::Path) -> Arc<dyn VectorStore> {
    let store_dir = dir.join("vectorfs");
    std::fs::create_dir_all(&store_dir).unwrap();
    let raw = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
    Arc::new(MemoryQualityGate::new(
        raw as Arc<dyn VectorStore>,
        animus_core::config::QualityGateConfig::default(),
    ))
}

#[test]
fn principal_resolution_cross_channel() {
    let principals = vec![
        PrincipalConfig {
            id: "jared".to_string(),
            role: PrincipalRole::Owner,
            channels: vec!["telegram:8593276557".to_string(), "terminal".to_string()],
        },
        PrincipalConfig {
            id: "claude-code".to_string(),
            role: PrincipalRole::AiAgent,
            channels: vec!["nats:animus.in.claude".to_string()],
        },
    ];

    // Telegram maps to jared
    let key = "telegram:8593276557";
    let found = principals.iter().find(|p| p.channels.iter().any(|c| c == key));
    assert_eq!(found.map(|p| p.id.as_str()), Some("jared"));

    // NATS maps to claude-code
    let key = "nats:animus.in.claude";
    let found = principals.iter().find(|p| p.channels.iter().any(|c| c == key));
    assert_eq!(found.map(|p| p.id.as_str()), Some("claude-code"));

    // Unknown channel falls back to None
    let key = "email:unknown@example.com";
    let found = principals.iter().find(|p| p.channels.iter().any(|c| c == key));
    assert!(found.is_none());
}

#[test]
fn situational_awareness_renders_peripheral() {
    let mut sa = SituationalAwareness::new(24);
    sa.set_active("jared", "telegram", "planning identity system");
    sa.set_active("claude-code", "nats", "implementing memory gate");
    sa.set_waiting("claude-code");

    let output = sa.render("jared", 500);
    assert!(output.contains("## Active Conversations"));
    assert!(output.contains("jared"));
    assert!(output.contains("current focus"));
    assert!(output.contains("claude-code"));
    assert!(output.contains("awaiting response"));
}

#[tokio::test]
async fn quality_gate_blocks_loop_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let emb = SyntheticEmbedding::new(4);

    // Store a "not responding" segment
    let text = "silence — not responding to any more prompts";
    let embedding = emb.embed_text(text).await.unwrap();
    let s1 = Segment::new(
        Content::Text(text.to_string()),
        embedding.clone(),
        Source::Manual { description: "channel:nats thread:jared".to_string() },
    );
    store.store(s1).unwrap();
    assert_eq!(store.count(None), 1);

    // Try to store another "not responding" — should be blocked by null-state cooldown
    let text2 = "silence — keepalive failed";
    let embedding2 = emb.embed_text(text2).await.unwrap();
    let s2 = Segment::new(
        Content::Text(text2.to_string()),
        embedding2,
        Source::Manual { description: "channel:nats thread:jared".to_string() },
    );
    store.store(s2).unwrap();
    assert_eq!(store.count(None), 1); // still 1 — second was blocked
}

#[tokio::test]
async fn ephemeral_decay_class_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let emb = SyntheticEmbedding::new(4);

    let text = "silence — final closure";
    let embedding = emb.embed_text(text).await.unwrap();
    let s = Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual { description: "channel:nats thread:jared".to_string() },
    );
    let id = store.store(s).unwrap();
    let retrieved = store.get(id).unwrap().unwrap();
    assert_eq!(retrieved.decay_class, DecayClass::Ephemeral);
}
```

- [ ] **Register test file** — add to `crates/animus-tests/tests/integration/` and ensure it's included (either via `mod identity;` in `mod.rs` or as a standalone test file).

- [ ] **Run integration tests**
```
cargo test -p animus-tests identity
```

---

## Task 10: Build Image and Deploy

- [ ] **Full workspace build**
```
cargo build --release --bin animus 2>&1 | tail -5
```
Expected: `Finished 'release' profile`

- [ ] **Full test suite**
```
cargo test --workspace 2>&1 | grep -E "^test result|FAILED"
```
Expected: all pass, 0 failures

- [ ] **Build Podman image**
```
podman build --no-cache -t animus:latest -f Dockerfile .
```

- [ ] **Stop old container and start new**
```bash
podman stop animus && podman rm animus
podman run -d \
  --name animus \
  --restart unless-stopped \
  -p 127.0.0.1:8082:8082 \
  --network animus-net \
  --add-host nats:10.89.4.2 \
  -e ANIMUS_DATA_DIR=/home/animus/.animus \
  -e ANIMUS_HEALTH_BIND=0.0.0.0:8082 \
  -e ANIMUS_LOG_LEVEL="animus=debug,animus_cortex=debug" \
  -e CLAUDE_CODE_OAUTH_TOKEN="${CLAUDE_CODE_OAUTH_TOKEN:-}" \
  -e ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-}" \
  -e ANIMUS_MODEL="${ANIMUS_MODEL:-claude-haiku-4-5-20251001}" \
  -e ANIMUS_EMBED_PROVIDER="${ANIMUS_EMBED_PROVIDER:-ollama}" \
  -e ANIMUS_OLLAMA_URL="${ANIMUS_OLLAMA_URL:-http://localhost:11434}" \
  -e ANIMUS_EMBED_MODEL="${ANIMUS_EMBED_MODEL:-mxbai-embed-large}" \
  -e ANIMUS_TELEGRAM_TOKEN="${ANIMUS_TELEGRAM_TOKEN:-}" \
  -e ANIMUS_AUTONOMY_MODE="${ANIMUS_AUTONOMY_MODE:-reactive}" \
  -e ANIMUS_TRUSTED_TELEGRAM_IDS="${ANIMUS_TRUSTED_TELEGRAM_IDS:-}" \
  -e ANIMUS_NATS_URL="${ANIMUS_NATS_URL:-nats://nats:14222}" \
  -e ANIMUS_FEDERATION="${ANIMUS_FEDERATION:-0}" \
  -v animus-data:/home/animus/.animus \
  -v "${HOME}/.claude/.credentials.json:/home/animus/.claude/.credentials.json" \
  -v "${HOME}/animus-comms:/home/animus/comms" \
  localhost/animus:latest
```

- [ ] **Verify health**
```
sleep 8 && curl -sf http://localhost:8082/health
```
Expected: `{"status":"ok",...}`

- [ ] **Add principals to config** — exec into the running container or set via env. Add to `/home/animus/.animus/config.toml`:
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

Then restart the container to pick up the config.

- [ ] **Smoke test — send NATS message, verify it routes to Jared's thread**
```
# From Claude Code terminal (using nuntius):
# Publish to animus.in.claude (should route to "claude-code" principal thread)
# Then ask Animus on Telegram to search memory — it should find the NATS exchange
```

---

## Notes

- The `--add-host nats:10.89.4.2` flag is a workaround for manual container start. The permanent fix is to use `podman compose up` which resolves `nats` via the compose network. The Podman compose bug (silent hang) should be investigated separately.
- All 5 tasks are independent up to Task 6 (runtime wiring). Tasks 1–4 can be done in parallel.
- The `situational_awareness.set_idle()` call in Task 7 must be placed in a way that runs even if `run_reasoning_turn` returns an error — use a `defer`-style pattern or ensure it's called in both `Ok` and `Err` branches.
