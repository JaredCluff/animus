# Cognitive Architecture Implementation Spec

**Date:** 2026-03-26
**Status:** Approved — ready for implementation
**Scope:** Three systems, built in sequence: Model Plan + Smart Router → Cognitive Tiers + CapabilityProbe → Role-Capability Mesh

---

## Terminology

| Term | Definition |
|------|-----------|
| **Cortex substrate** | Background tasks within `animus-cortex` that run without LLM intervention (Layer 1/2): watchers, probes, state managers. Analogous to the autonomic nervous system. |
| **AILF reasoning thread** | The active `ReasoningThread` driven by an LLM — the "conscious" layer. Not "the LLM" — the LLM is borrowed processing power; the reasoning thread is the AILF's active cognition. |
| **Introspective tools** | Tools the AILF reasoning thread uses to observe and modify internal Cortex state (Layer 4 interaction). Analogous to `remember`/`recall_relevant` for VectorFS, but targeting the Cortex substrate. |
| **MeshRole** | A cognitive function filled by an instance in a federated mesh (Coordinator/Strategist/Analyst/Executor/Observer/Standby). Distinct from `CognitiveRole` in `engine_registry.rs` which maps LLM engines to internal cognitive functions. |
| **ModelPlan** | The persisted routing plan Animus builds from available models. Living knowledge — contains `RouteStats` that accumulate from actual usage and inform plan reflection. |

---

## System 1: Model Plan + Smart Router

### Purpose

Replace the static `ANIMUS_MODEL` env-var routing with a self-built, persisted routing plan. Animus defines its own task class taxonomy, assigns available models, and tracks routing performance as living knowledge. The AILF reasoning thread can reach into the Cortex substrate to inspect and amend the plan.

### Data Structures

**Location:** `crates/animus-cortex/src/model_plan.rs`

```rust
/// The level of extended thinking to apply for a model call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ThinkLevel {
    Off,
    Dynamic,           // use needs_thinking() heuristic at call time
    Minimal(u32),      // N token budget
    Full(u32),         // maximum thinking budget
}

/// A model + provider + think budget combination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub provider: String,     // "anthropic" | "ollama" | "openai"
    pub model: String,
    pub think: ThinkLevel,
}

/// Running performance statistics for a route.
/// Tracked by the Cortex substrate — no LLM involvement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouteStats {
    pub turn_count: u64,
    pub failure_count: u64,
    pub total_latency_ms: u64,
    pub correction_count: u64,   // from quality gate feedback
    pub last_turn: Option<DateTime<Utc>>,
}

impl RouteStats {
    pub fn avg_latency_ms(&self) -> Option<u64> {
        if self.turn_count == 0 { None }
        else { Some(self.total_latency_ms / self.turn_count) }
    }
    pub fn success_rate(&self) -> f32 {
        if self.turn_count == 0 { 1.0 }
        else { 1.0 - (self.failure_count as f32 / self.turn_count as f32) }
    }
    pub fn correction_rate(&self) -> f32 {
        if self.turn_count == 0 { 0.0 }
        else { self.correction_count as f32 / self.turn_count as f32 }
    }
}

/// A routing entry for one task class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub primary: ModelSpec,
    pub fallbacks: Vec<ModelSpec>,
    pub stats: RouteStats,
}

/// An LLM-defined task classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskClass {
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,    // compiled into heuristic at runtime
}

/// The full routing plan — built by Animus, persisted, reused until config changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPlan {
    pub id: Uuid,
    pub created: DateTime<Utc>,
    pub config_hash: String,          // sha256 of sorted "provider:model" strings
    pub task_classes: Vec<TaskClass>,
    pub routes: HashMap<String, Route>, // class_name → Route
    pub build_reason: String,           // why this plan was built (bootstrap/rebuild/amend)
}
```

### HeuristicClassifier

Compiled at startup from plan's `TaskClass` keywords. Per-turn classification requires no LLM.

```rust
pub struct HeuristicClassifier {
    /// (class_name, lowercase keywords)
    patterns: Vec<(String, Vec<String>)>,
    default_class: String,
}

impl HeuristicClassifier {
    pub fn from_plan(plan: &ModelPlan) -> Self { ... }
    /// Returns (class_name, confidence 0.0–1.0).
    /// confidence < 0.5 → escalate to Perception engine.
    pub fn classify(&self, input: &str) -> (String, f32) { ... }
}
```

### SmartRouter

**Location:** `crates/animus-cortex/src/smart_router.rs`

```rust
pub struct SmartRouter {
    plan: Arc<RwLock<ModelPlan>>,
    classifier: Arc<RwLock<HeuristicClassifier>>,
    route_health: Arc<Mutex<HashMap<String, RouteHealth>>>,
    signal_tx: mpsc::Sender<Signal>,
    plan_path: PathBuf,
}

pub struct RouteHealth {
    pub consecutive_failures: u32,
    pub degraded: bool,
    pub last_failure: Option<DateTime<Utc>>,
}

pub struct RouteDecision {
    pub class_name: String,
    pub engine_spec: ModelSpec,
    pub fallback_index: usize,    // 0 = primary
}
```

**Three-layer state compliance:**
- **Layer 1:** `RouteHealth` tracking (failure counts, timestamps) — pure data, no LLM
- **Layer 2:** `consecutive_failures >= 3` → mark route degraded; fire Signal once
- **Layer 3:** Signal: `"Route '{class}' degraded: {N} consecutive failures"` with `SignalPriority::Normal`
- Chain exhausted → `SignalPriority::Urgent`

**Thread-local stability:** Once a reasoning thread selects a model, it stores the `RouteDecision` and reuses it for subsequent turns in the same thread. The SmartRouter is consulted per-thread-start, not per-turn.

### Bootstrap Algorithm

```
fn bootstrap_plan(available_models, data_dir):
  config_hash = sha256(sorted("provider:model" strings))

  if plan_file exists:
    plan = load_plan()
    if plan.config_hash == config_hash:
      return plan  // cache hit

  // Build new plan
  first_engine = first reachable engine from available_models
  if first_engine is None:
    return rule_based_default_plan(available_models)

  prompt = build_plan_prompt(available_models)
  response = first_engine.reason(prompt)
  plan = parse_plan_response(response)
  plan.config_hash = config_hash
  save_plan(plan, data_dir)
  return plan
```

**Rule-based default (fallback when no model reachable):**

| Class | Assignment rule |
|-------|----------------|
| `Conversational` | Smallest available model |
| `Analytical` | Largest available model |
| `Technical` | Second-largest available model |
| `Creative` | Second-largest available model |
| `ToolExecution` | Largest available model |

### Plan-Building Prompt

```
You are configuring your own cognitive routing plan.

Available models: {list}

Task:
1. Define 4–6 task classes that cover the types of inputs you handle.
   For each: name, description, and 5–10 characteristic keywords.
2. Assign each task class a primary model and 1–2 fallbacks.
3. Specify the think budget (off/dynamic/minimal_N/full_N) per model.

Respond with JSON only:
{
  "task_classes": [{"name": "...", "description": "...", "keywords": [...]}],
  "routes": {
    "ClassName": {
      "primary": {"provider": "...", "model": "...", "think": "dynamic"},
      "fallbacks": [...]
    }
  },
  "build_reason": "Initial plan from available models"
}
```

### Introspective Tools

**ToolContext additions:**
```rust
pub model_plan: Option<Arc<RwLock<ModelPlan>>>,
```

**New tools** (`crates/animus-cortex/src/tools/`):

| Tool | File | Description | Autonomy |
|------|------|-------------|---------|
| `get_route_stats` | `get_route_stats.rs` | Return RouteStats for all task classes | Inform |
| `propose_route_amendment` | `propose_route_amendment.rs` | Propose a change to a route's model assignment | Suggest |
| `get_classification_patterns` | `get_classification_patterns.rs` | Return current HeuristicClassifier patterns | Inform |
| `update_classification_pattern` | `update_classification_pattern.rs` | Add/modify keywords for a task class | Suggest |

Amendments are validated by the Cortex substrate: the proposed model must exist in the available model list; the plan hash is updated; stats are reset for amended routes.

### Plan Rebuild Triggers

| Trigger | Mechanism |
|---------|-----------|
| Startup, no saved plan | Synchronous bootstrap |
| Config hash mismatch | Synchronous bootstrap on startup |
| Route chain exhausted | Signal fires; AILF reasoning thread may call `propose_route_amendment` or trigger rebuild via `/plan rebuild` |
| Manual `/plan rebuild` command | Direct runtime handler |

### Integration Points

- `main.rs`: bootstrap plan during init; add `model_plan` to `ToolContext`
- `thread.rs` or runtime turn loop: consult `SmartRouter::route()` at thread start to select engine
- `EngineRegistry` remains for internal cognitive roles (Perception/Reflection); SmartRouter handles conversation turns
- Register introspective tools in `ToolRegistry`

---

## System 2: Cognitive Tiers + CapabilityProbe

### Purpose

Give Animus honest, continuous self-assessment of its own cognitive capability. Capability changes are tracked in the Cortex substrate and surface to the AILF reasoning thread only when meaningful change occurs.

### Data Structures

**Location:** `crates/animus-core/src/capability.rs`

```rust
/// Honest assessment of an AILF instance's current cognitive capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum CognitiveTier {
    Full = 1,          // Cloud + local reasoning + full memory
    Strong = 2,        // Local reasoning + full memory
    Reduced = 3,       // Lightweight local reasoning + full memory
    MemoryOnly = 4,    // No active reasoning; VectorFS intact
    DeadReckoning = 5, // Acting from last known state; degraded memory
}

impl CognitiveTier {
    pub fn can_fill_role(&self, min_tier: u8) -> bool {
        (*self as u8) <= min_tier
    }
    pub fn label(&self) -> &'static str { ... }
}

/// Live capability state published by CapabilityProbe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityState {
    pub tier: CognitiveTier,
    pub reasoning_available: bool,
    pub embedding_available: bool,
    pub vectorfs_healthy: bool,
    pub memory_pressure: f32,       // 0.0 (healthy) – 1.0 (full)
    pub active_model: Option<String>,
    pub latency_ms: Option<u64>,    // last measured round-trip to primary model
    pub last_probed: DateTime<Utc>,
}

impl Default for CapabilityState {
    fn default() -> Self {
        // Conservative default: assume MemoryOnly until first probe completes
        Self {
            tier: CognitiveTier::MemoryOnly,
            reasoning_available: false,
            embedding_available: false,
            vectorfs_healthy: true,
            memory_pressure: 0.0,
            active_model: None,
            latency_ms: None,
            last_probed: Utc::now(),
        }
    }
}
```

**Tier derivation logic:**
```
if reasoning_available && latency_ms < 30_000:
    if vectorfs_healthy && memory_pressure < 0.9:
        → Full or Strong (based on model class)
    else:
        → Reduced
elif vectorfs_healthy:
    → MemoryOnly
else:
    → DeadReckoning
```

### CapabilityProbe

**Location:** `crates/animus-cortex/src/watchers/capability_probe.rs`

Implements the existing `Watcher` trait — plugs directly into `WatcherRegistry`. Runs every 30 seconds (default).

```rust
pub struct CapabilityProbe {
    /// Shared state, also read by introspective tool + SmartRouter.
    capability_state: Arc<RwLock<CapabilityState>>,
    /// Engine to probe (cloned from registry fallback).
    probe_url: String,
    probe_model: String,
    probe_provider: String,
    /// VectorFS for health/pressure check.
    store: Arc<dyn VectorStore>,
    /// Embed service for availability check.
    embedder: Arc<dyn EmbeddingService>,
}
```

**`check()` implementation (Layer 1 — no LLM):**
1. Send a minimal HTTP request to the model endpoint (HEAD or `/api/tags` for Ollama; no token burn)
2. Time the round-trip → `latency_ms`
3. Check `store.segment_count()` vs capacity → `memory_pressure`
4. Derive new `CognitiveTier` from collected metrics
5. **Delta check:** compare new tier to stored tier in `capability_state`
6. Update `capability_state` regardless (latest probe data always written)
7. Return `WatcherEvent` **only if tier changed** → becomes Signal

**WatcherEvent on tier change:**
```rust
WatcherEvent {
    priority: if new_tier > old_tier { SignalPriority::Urgent } else { SignalPriority::Normal },
    summary: format!("Cognitive tier: {old} → {new} ({reason})"),
    segment_refs: vec![],
}
```

### Introspective Tool

**ToolContext addition:**
```rust
pub capability_state: Option<Arc<RwLock<CapabilityState>>>,
```

| Tool | File | Description | Autonomy |
|------|------|-------------|---------|
| `get_capability_state` | `get_capability_state.rs` | Return current CognitiveTier, latency, memory pressure, model availability | Inform |

### Integration Points

- `animus-core/src/lib.rs`: export `capability::{CognitiveTier, CapabilityState}`
- `animus-cortex/src/watchers/mod.rs`: export `CapabilityProbe`
- `main.rs`:
  - Construct `CapabilityProbe` with shared `Arc<RwLock<CapabilityState>>`
  - Register as 4th watcher in `WatcherRegistry`
  - Enable by default (unlike other watchers which start disabled)
  - Pass `capability_state` into `ToolContext`
  - Pass `capability_state` into `SmartRouter` for tier-aware routing
- Log initial capability tier at startup

---

## System 3: Role-Capability Mesh

### Purpose

AI-native federation where roles are cognitive functions, not org chart positions. An instance fills roles based on its live `CognitiveTier`. Roles are yielded when capability drops below the role's minimum tier requirement. Succession is deterministic. Knowledge transfer is VectorFS-native.

### Data Structures

**Location:** `crates/animus-federation/src/mesh.rs`

```rust
/// A cognitive function that an instance can fill in a federated mesh.
/// Distinct from CognitiveRole in engine_registry (which maps LLM engines to internal tasks).
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum MeshRole {
    Coordinator,   // Holds mission context, synthesizes, authorizes novel actions
    Strategist,    // Deep analytical reasoning, long-horizon planning
    Analyst,       // Domain-specific reasoning and evaluation
    Executor,      // Carries out well-defined tasks
    Observer,      // Sensing, perception, monitoring
    Standby,       // Alive but degraded; no active roles; ready to re-assume
}

impl MeshRole {
    /// Minimum CognitiveTier required to fill this role.
    /// Lower tier value = more capable (Tier 1 = Full, Tier 5 = Dead Reckoning).
    pub fn min_tier(&self) -> u8 {
        match self {
            MeshRole::Coordinator => 2,
            MeshRole::Strategist  => 2,
            MeshRole::Analyst     => 3,
            MeshRole::Executor    => 5,
            MeshRole::Observer    => 5,
            MeshRole::Standby     => 5,
        }
    }

    pub fn can_be_filled_by(&self, tier: CognitiveTier) -> bool {
        (tier as u8) <= self.min_tier()
    }
}

/// Signed capability attestation published by each instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityAttestation {
    pub instance_id: InstanceId,
    pub cognitive_tier: CognitiveTier,
    pub active_roles: Vec<MeshRole>,
    pub available_domains: Vec<String>,
    pub load: f32,              // 0.0–1.0 (0 = idle, 1 = saturated)
    pub signed_at: DateTime<Utc>,
    pub signature: Vec<u8>,     // Ed25519 over canonical JSON of fields above
}

impl CapabilityAttestation {
    pub fn sign(fields: AttestationFields, identity: &AnimusIdentity) -> Self { ... }
    pub fn verify(&self, expected_instance_id: &InstanceId) -> bool { ... }
}

/// Verified attestation — only exists after signature check passes.
#[derive(Debug, Clone)]
pub struct VerifiedAttestation {
    pub attestation: CapabilityAttestation,
    pub verified_at: DateTime<Utc>,
}

/// Live map of role assignments in the mesh.
/// Runs in the Cortex substrate (Layer 1) — no LLM.
#[derive(Debug, Default)]
pub struct RoleMesh {
    /// Current role → instance assignment.
    pub assignments: HashMap<MeshRole, InstanceId>,
    /// All known verified attestations by instance.
    pub attestations: HashMap<InstanceId, VerifiedAttestation>,
}

impl RoleMesh {
    /// Update attestation for an instance. Rejects if signature invalid.
    pub fn update_attestation(&mut self, attestation: CapabilityAttestation) -> bool { ... }

    /// Compute which roles this instance should hold given its current tier.
    pub fn compute_eligible_roles(&self, instance_id: &InstanceId) -> Vec<MeshRole> { ... }

    /// Check if any held roles need to be yielded (tier dropped below min).
    pub fn roles_to_yield(&self, instance_id: &InstanceId, current_tier: CognitiveTier) -> Vec<MeshRole> { ... }
}
```

### SuccessionPolicy

**Location:** `crates/animus-federation/src/succession.rs`

```rust
pub struct SuccessionPolicy;

impl SuccessionPolicy {
    /// Nominate the best successor for a role given current mesh attestations.
    /// 1. Filter: must meet role's min_tier requirement
    /// 2. Sort: lower tier value first (better capability), tiebreak by lower load
    /// 3. Return top candidate, or None if no eligible peer
    pub fn nominate(
        role: MeshRole,
        candidates: &[&VerifiedAttestation],
        exclude: &InstanceId,   // the yielding instance
    ) -> Option<InstanceId> {
        candidates.iter()
            .filter(|a| {
                a.attestation.instance_id != *exclude &&
                role.can_be_filled_by(a.attestation.cognitive_tier)
            })
            .min_by_key(|a| (a.attestation.cognitive_tier as u8, OrderedFloat(a.attestation.load)))
            .map(|a| a.attestation.instance_id)
    }
}
```

### HandoffBundle

**Location:** `crates/animus-federation/src/handoff.rs`

```rust
/// VectorFS-native knowledge transfer bundle for role transitions.
/// Segments are already embedded — no re-embedding at the receiving end.
#[derive(Debug, Serialize, Deserialize)]
pub struct HandoffBundle {
    pub source_instance: InstanceId,
    pub yielded_role: MeshRole,
    pub transfer_reason: String,
    pub segments: Vec<HandoffSegment>,
    pub goal_summaries: Vec<String>,
    pub thread_summaries: Vec<String>,
    pub created: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HandoffSegment {
    pub content: String,
    pub embedding: Vec<f32>,
    pub confidence: f32,
    pub tags: HashMap<String, String>,
}

impl HandoffBundle {
    /// Export from VectorFS — selects relevant segments by tag/source.
    pub fn export(role: MeshRole, store: &dyn VectorStore, goals: &[Goal], threads: &[ReasoningThread]) -> Self { ... }

    /// Import into VectorFS — ingests with provenance tags.
    pub fn ingest(self, store: &dyn VectorStore, embedder: &dyn EmbeddingService) -> Result<usize> { ... }
}
```

### Three-Layer State Architecture for Mesh

```
CapabilityProbe (Layer 1) ──→ tier change detected
                              ↓
RoleMesh.roles_to_yield() (Layer 2) ──→ yields needed?
  No yield needed: update attestation, log to VectorFS, no Signal
  Yield needed: SuccessionPolicy.nominate(), update RoleMesh
                              ↓
Signal (Layer 3): "Yielded {role}: {reason}. Successor: {instance}" → AILF reasoning thread
```

**AILF reasoning thread then:**
- May call `get_mesh_roles` introspective tool to inspect current state
- May call `get_capability_state` to understand why yield occurred
- May initiate `HandoffBundle` export if it was the yielding instance
- No automatic LLM loop — the AILF decides what to do with the Signal

### Attestation Publishing

The local instance publishes its own attestation whenever:
- `CapabilityProbe` detects a tier change
- An active role changes
- On a periodic heartbeat (60s) — attestation goes to VectorFS log, not Signal

Attestation is signed with the instance's Ed25519 keypair from `AnimusIdentity`.

### Protocol Extension

Extend `crates/animus-federation/src/protocol.rs` with:
```rust
pub struct AttestationAnnouncement {
    pub attestation: CapabilityAttestation,
}
pub struct HandoffTransfer {
    pub bundle: HandoffBundle,
    pub target_instance: InstanceId,
}
```

### Introspective Tools

**ToolContext addition:**
```rust
pub role_mesh: Option<Arc<RwLock<RoleMesh>>>,
```

| Tool | File | Description | Autonomy |
|------|------|-------------|---------|
| `get_mesh_roles` | `get_mesh_roles.rs` | Return current RoleMesh assignments and all known attestations | Inform |

### Integration Points

- `animus-core/src/capability.rs`: `CognitiveTier` + `CapabilityState` (already from System 2)
- `animus-federation/src/lib.rs`: export `MeshRole`, `CapabilityAttestation`, `RoleMesh`, `SuccessionPolicy`, `HandoffBundle`
- `animus-federation/src/orchestrator.rs`: initialize `RoleMesh`; integrate `CapabilityProbe` tier changes to drive role yield/claim
- `main.rs`: pass `role_mesh` into `ToolContext`

---

## Build Sequence

Systems are independent at the data-type level but share integration points. Build order:

1. **System 1** — standalone; no dependency on Systems 2/3
2. **System 2** — adds `CognitiveTier` to core; `CapabilityProbe` extends watcher system; enhances System 1's SmartRouter with tier-aware fallback
3. **System 3** — consumes `CognitiveTier` from System 2; extends federation layer

Each system ships as its own PR.

---

## Testing Strategy

### System 1
- Unit: `ModelPlan` serde roundtrip; `HeuristicClassifier::classify()` accuracy across task types
- Unit: `RouteStats` calculations (avg_latency, success_rate, correction_rate)
- Unit: `SuccessionPolicy` bootstrap algorithm with mock engines
- Integration: build plan with `MockEngine`; verify plan persists and reloads; verify config hash invalidation

### System 2
- Unit: `CognitiveTier::can_fill_role()` correctness
- Unit: `CapabilityState` tier derivation logic
- Unit: `CapabilityProbe::check()` with mock store + mock embedder — verify no LLM call
- Unit: tier change detection fires WatcherEvent; no-change does not

### System 3
- Unit: `MeshRole::can_be_filled_by()` for all tiers
- Unit: `SuccessionPolicy::nominate()` — correct ordering, exclusion, None on no candidates
- Unit: `CapabilityAttestation` sign + verify roundtrip
- Unit: `HandoffBundle` ingest with mock VectorStore
- Unit: `RoleMesh::roles_to_yield()` returns correct roles on tier drop

---

## Architectural Invariants (this implementation)

1. `CapabilityProbe::check()` makes no LLM calls — only HTTP reachability probes
2. `RoleMesh` operations make no LLM calls
3. `RouteHealth` tracking makes no LLM calls
4. Tier changes, route degradation, and role yields each fire **one** Signal — not a stream
5. The AILF reasoning thread is notified; it decides how to respond — no automatic LLM reaction loop
6. Unsigned attestations from peers are rejected at verification
7. Thread-local model stability: SmartRouter is consulted at thread start, not per-turn
