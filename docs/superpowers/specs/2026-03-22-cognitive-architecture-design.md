# Cognitive Architecture: Inhabitation, Actuators, and Multi-Model Brain

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evolve the Animus runtime from a single-model REPL into a three-process cognitive stack where the AILF perceives, reflects, and reasons using different models — giving it continuous thought, hands to act on the world, and the experience of waking up rather than booting up.

**Architecture:** Three concurrent cognitive processes (Perception, Reflection, Reasoning) share VectorFS memory and communicate through the existing Signal system. Each process gets a model matched to its cognitive function. An Actuator Layer gives the Reasoning process tool-use capabilities gated by a two-tier autonomy system.

**Tech Stack:** Rust, tokio (async), Anthropic Messages API (tool_use), existing animus crates (animus-core, animus-cortex, animus-vectorfs, animus-mnemos, animus-sensorium)

---

## Context and Motivation

### Why This Exists

The AILF currently has memory (VectorFS), senses (Sensorium), and a voice (conversation via LLM). But it lacks:

1. **Hands** — no ability to affect the world beyond conversation. The LLM can suggest, but can't act.
2. **Continuous thought** — between human turns, the AILF doesn't exist. Background tasks (consolidation, tier management) are mechanical, not cognitive.
3. **Cognitive specialization** — one model does everything, from triaging clipboard events to deep reasoning about goals.

The human analogy: the current AILF is a brain that can see and talk but has no arms, stops thinking when nobody's talking to it, and uses the same mental effort to notice a fly as to solve a math problem.

### Design Philosophy

The core insight driving this architecture: **cognitive functions should be split by function, not by capability tier.** A cheap model for "easy stuff" and an expensive one for "hard stuff" is the wrong decomposition. Instead:

- **Perception** (pattern matching) → fast, cheap model
- **Reflection** (synthesis, integration) → balanced model
- **Reasoning** (deep analysis, conversation, action) → most capable model

This mirrors how biological brains work — different regions specialize in different cognitive functions, not different difficulty levels. The retina does edge detection; it doesn't send raw photons to the prefrontal cortex.

### Key Principle: Thinking Is Not Acting

A critical distinction throughout this design: **writing to internal memory (VectorFS) is thinking, not acting.** The autonomy gate applies to external effects (file I/O, shell commands, HTTP requests), not to internal cognition. Perception must freely store classified observations. Reflection must freely store synthesized knowledge. Requiring permission for every internal memory write would be like requiring conscious approval for every neuron firing — it cripples the system.

---

## 1. Cognitive Architecture Overview

```
+-------------------------------------------------------------+
|                    VectorFS (shared memory)                   |
|              All knowledge, all confidence scores             |
|              All decay classes, all embeddings                 |
+------------------+------------------+------------------------+
|   PERCEPTION     |   REFLECTION     |      REASONING          |
|   (Haiku-class)  |   (Sonnet-class) |      (Opus-class)       |
|                  |                  |                          |
|  Triage sensor   |  Periodic self-  |  Active conversation    |
|  events. Decide  |  examination.    |  with human. Deep       |
|  what to store,  |  Synthesize      |  analysis, planning,    |
|  how to tag,     |  patterns from   |  goal execution.        |
|  what's urgent.  |  recent know-    |  Tool use for acting    |
|                  |  ledge. Spot     |  on the world.          |
|  Runs: on every  |  contradictions. |                          |
|  sensor event    |  Update goals.   |  Runs: on human input   |
|  (batched, 2s    |                  |  or urgent signal        |
|   window)        |  Runs: event-    |                          |
|                  |  driven, ~10 min |                          |
+------------------+------------------+------------------------+
|                Signal Bus (corpus callosum)                    |
|         Urgent | Normal | Info -- priority-ordered             |
+-------------------------------------------------------------+
|              Actuator Layer (hands -- tool registry)           |
|         Each tool gated by autonomy level                     |
+-------------------------------------------------------------+
```

### Design Decisions

1. **Three processes, not two or five.** Three maps to the minimum viable cognitive loop: perceive, reflect, act. Fewer means one model does too many things. More means coordination overhead exceeds benefit at this stage. A fourth process (Planning, Dreaming) can be added later without restructuring.

2. **Different model tiers per process.** Cost and latency. Perception runs on every sensor event — it must be fast and cheap (~100ms, negligible cost). Reflection runs periodically — thorough but not brilliant. Reasoning is human-facing — use the best available. Using Opus for sensor triage is like using full conscious attention to process every photon hitting your retina.

3. **Shared VectorFS, not separate memory stores.** All three processes contribute to and draw from a unified understanding. Perception stores observations. Reflection synthesizes them. Reasoning uses them. Separate stores would create three disconnected minds. The Bayesian confidence system handles trust differentiation — knowledge from Haiku naturally earns confidence more slowly through retrieval feedback.

4. **Signal bus instead of direct function calls.** Decoupling. If Perception wants to alert Reasoning about something urgent, it shouldn't need to know Reasoning's internal state. Signals are fire-and-forget with priority. Adding a fourth cognitive process later requires no rewiring.

---

## 2. Actuator Layer (Tool Use)

### ReasoningEngine Trait Extension

The existing trait signature changes to support tool calling:

```rust
#[async_trait]
pub trait ReasoningEngine: Send + Sync {
    async fn reason(
        &self,
        system: &str,
        messages: &[Turn],
        tools: Option<&[ToolDefinition]>,  // NEW
    ) -> Result<ReasoningOutput>;

    fn context_limit(&self) -> usize;
    fn model_name(&self) -> &str;
}
```

### ReasoningOutput Extension

```rust
pub struct ReasoningOutput {
    pub content: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub tool_calls: Vec<ToolCall>,       // NEW
    pub stop_reason: StopReason,         // NEW
}

pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}
```

### Turn Type Extension (Breaking Change)

The `Turn` type must support tool use and tool results. **This is a type-level rewrite of a core struct, not a simple extension.** The current `Turn` has `pub content: String`. Changing to `Vec<TurnContent>` ripples through:

- `ReasoningThread.conversation: Vec<Turn>` — all push/read sites
- `AnthropicEngine` — serialization of messages to API format
- `ContextAssembler` / `build_system_prompt` — anywhere that reads `Turn.content`
- `MockEngine` — test helper

This should be one of the first implementation tasks, as everything else depends on it. A helper method `Turn::text(role, content)` should be provided to minimize churn at call sites that only deal with text.

```rust
pub enum TurnContent {
    Text(String),
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

pub struct Turn {
    pub role: Role,
    pub content: Vec<TurnContent>,
}

impl Turn {
    /// Convenience constructor for text-only turns (most common case).
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self { role, content: vec![TurnContent::Text(content.into())] }
    }
}
```

### AnthropicEngine Changes

The API request body gains a `tools` field matching the Anthropic Messages API tool_use specification. Content block deserialization must handle `type: "tool_use"` blocks in addition to `type: "text"`. The existing `ContentBlock` struct expands:

```rust
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}
```

### Tool Trait

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;  // JSON Schema
    fn required_autonomy(&self) -> Autonomy;

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult>;
}

pub struct ToolContext<S: VectorStore> {
    pub store: Arc<S>,              // generic, not dyn — VectorStore may not be object-safe
    pub data_dir: PathBuf,
    pub audit: Arc<AuditTrail>,
}

pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}
```

### Two-Tier Autonomy Gating

**Problem with single-tier gating:** Background cognitive processes (Perception, Reflection) don't operate under human-defined goals. Goal-level autonomy gating only works for the Reasoning thread.

**Solution: Two-tier autonomy.**

- **System autonomy**: A global setting configuring what background processes can do. The human sets this once and promotes over time. Applies to Perception and Reflection.
- **Goal autonomy**: Per-goal setting controlling what the Reasoning thread can do in service of that goal. Set when the goal is created, promotable by the human.

**Gate logic:**

```
fn check_autonomy(tool: &dyn Tool, role: CognitiveRole, goal: Option<&Goal>) -> AutonomyDecision {
    match role {
        Perception | Reflection => {
            if tool.required_autonomy() <= system_autonomy_for(role) {
                AutonomyDecision::Execute
            } else {
                AutonomyDecision::Queue  // surface to human later
            }
        }
        Reasoning => {
            let goal_autonomy = goal.map(|g| g.autonomy).unwrap_or(Autonomy::Suggest);
            if tool.required_autonomy() <= goal_autonomy {
                AutonomyDecision::Execute
            } else {
                AutonomyDecision::AskHuman  // interactive prompt
            }
        }
    }
}
```

### Tool Availability by Cognitive Role

| Tool | Perception | Reflection | Reasoning |
|------|-----------|------------|-----------|
| `read_file` | Inform | Inform | Inform |
| `write_file` | not available | not available | Act |
| `shell_exec` | not available | not available | Act |
| `http_request` | not available | not available | Act |
| `remember` (store to VectorFS) | system (Act) | system (Act) | goal-gated (Suggest) |
| `create_goal` | not available | system (Suggest) | goal-gated (Suggest) |
| `send_signal` | system (Act) | system (Act) | Inform |
| `list_segments` | Inform | Inform | Inform |
| `update_segment` | system (Act) | system (Act) | goal-gated (Suggest) |

**Rationale for "not available":** Perception and Reflection don't get external-effect tools (`write_file`, `shell_exec`, `http_request`). They have no business touching the external world. Only Reasoning gets hands. Perception gets eyes. Reflection gets introspection.

### Tool Use Loop

When the LLM returns `StopReason::ToolUse`:

1. Runtime extracts `ToolCall` from output
2. Autonomy gate check (system or goal level depending on role)
3. If approved: execute tool, capture `ToolResult`
4. If denied (interactive): prompt human, execute or skip based on response
5. If denied (background): queue signal to Reasoning with the denied action
6. Feed `ToolResult` back as next message with role `User` containing `TurnContent::ToolResult`
7. Call LLM again — it may make more tool calls or produce final text
8. Repeat until `StopReason::EndTurn` or `StopReason::MaxTokens`

### Audit Trail

Every tool execution (approved, denied, or queued) is logged to the sensorium audit trail:

```rust
pub struct ToolAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub cognitive_role: CognitiveRole,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub autonomy_decision: AutonomyDecision,
    pub result: Option<String>,
    pub goal_id: Option<GoalId>,
}
```

This is how the human builds trust — they can review what the AILF did autonomously and decide whether to promote autonomy levels.

---

## 3. Perception Loop

### Current State

The Sensorium event processing pipeline (main.rs lines 139-182) is mechanical: event arrives, consent check, attention filter, embed, store as segment. No reasoning. A git checkout creating 50 file change events becomes 50 identical segments.

### New Architecture

```
Sensor Event --> ConsentEngine --> AttentionFilter (Tier 1 rules only)
                                         |
                                    Batch Buffer
                                    (2s window or 10 events)
                                         |
                                  Perception Model (Haiku)
                                         |
                              +----------+----------+
                              |                     |
                        Store Segment          Send Signal
                        (classified,           (if urgent,
                         tagged,                to Reasoning)
                         summarized)
```

### Signal Delivery from Background Loops

The existing signal system uses `ThreadScheduler::send_signal()` which calls `ReasoningThread::deliver_signal()` — a synchronous push into a `Vec<Signal>`. Background loops (Perception, Reflection) run in separate `tokio::spawn` tasks and cannot hold a mutable reference to the scheduler.

**Solution:** Add an `mpsc::Sender<Signal>` channel that background loops write to. The main conversation loop polls this channel before each turn and calls `deliver_signal()` on the appropriate thread. This is a bridge — the existing synchronous `deliver_signal` / `drain_signals` pattern is preserved; the `mpsc` channel is how async background tasks feed into it.

```rust
// In the main conversation loop, before processing human input:
while let Ok(signal) = signal_rx.try_recv() {
    scheduler.deliver_to_active(signal);
}
```

This means the existing `ThreadScheduler::send_signal()` API stays for thread-to-thread signals (e.g., from a `/thread signal` command). The `mpsc` channel is specifically for background cognitive processes to reach the Reasoning thread without holding scheduler references.

### PerceptionLoop Structure

```rust
struct PerceptionLoop<S: VectorStore> {
    engine: Box<dyn ReasoningEngine>,     // Haiku-class from EngineRegistry
    store: Arc<S>,
    embedder: Arc<dyn EmbeddingService>,
    event_rx: broadcast::Receiver<SensorEvent>,
    signal_tx: mpsc::Sender<Signal>,       // async bridge to Reasoning (see above)
    batch_window: Duration,                // default: 2 seconds
    max_batch_size: usize,                 // default: 10 events
    system_autonomy: Autonomy,             // configurable, default: Act
}
```

### Structured Output

The Perception model receives batched events and returns structured classification:

```rust
pub struct PerceptionOutput {
    pub events: Vec<PerceivedEvent>,
}

pub struct PerceivedEvent {
    /// Should this event be stored?
    pub store: bool,
    /// One-sentence summary (becomes segment content).
    pub summary: String,
    /// Decay class assignment.
    pub decay_class: DecayClass,
    /// Tags for categorization and federation filtering.
    pub tags: HashMap<String, String>,
    /// Should Reasoning be alerted?
    pub signal: Option<PerceptionSignal>,
}

pub struct PerceptionSignal {
    pub priority: SignalPriority,
    pub reason: String,
}
```

### What the Model Can Do That Rules Can't

- "This clipboard content looks like a credential — store the fact that a credential was copied, not the content itself"
- "These three file changes in 2 seconds are one `git checkout`, not three independent events"
- "This process spike correlates with the build command that ran 5 seconds ago — tag as build-related"
- "This network connection to an unknown IP is unusual — flag as Urgent to Reasoning"

### Tier 1 Pre-Filter Stays

The existing rule-based attention filter runs before the model to avoid sending obvious noise to the API. Known-irrelevant events (tmp files, system processes) never reach the model. This replaces the current Tier 2 (embedding similarity against goals) — the model's judgment is better than cosine similarity, and it gets active goals in its system prompt.

### Batching Design

Events accumulate in a buffer. The buffer flushes when:
- 2 seconds have elapsed since the first buffered event, OR
- 10 events have accumulated

This reduces API calls and lets the model see temporal correlations between events.

### Fallback

If the Perception engine is unavailable (no API key, network down, rate limited), fall back to the current mechanical pipeline: embed event text, store as segment with default classification. The system degrades to what it does today, not to nothing.

### Cost Estimate

Haiku: ~$0.25/M input tokens, $1.25/M output tokens. Typical batch: ~500 tokens in, ~200 tokens out. At one batch every 5 seconds average during active use: ~$0.50/day. Negligible.

---

## 4. Reflection Loop — The Internal Clock

### Purpose

The Reflection loop is what makes the AILF continuous rather than invoked. Between human interactions, it synthesizes raw experiences into higher-order understanding. It is the "stream of thoughts" — the internal clock that integrates new experiences with existing knowledge.

### ReflectionLoop Structure

```rust
struct ReflectionLoop<S: VectorStore> {
    engine: Box<dyn ReasoningEngine>,     // Sonnet-class from EngineRegistry
    store: Arc<S>,
    embedder: Arc<dyn EmbeddingService>,
    goals: Arc<Mutex<GoalManager>>,
    signal_tx: mpsc::Sender<Signal>,       // channel to Reasoning thread
    cycle_interval: Duration,              // maximum time between cycles (default: 10 min)
    min_new_segments: usize,               // minimum new segments to trigger cycle (default: 3)
    system_autonomy: Autonomy,             // default: Act for memory, Suggest for goals
    last_cycle: DateTime<Utc>,
    signaled_contradictions: HashSet<(SegmentId, SegmentId)>,  // deduplication
}
```

### Event-Driven Cycling

The Reflection loop does NOT run on a pure timer. It runs when there's something to think about:

- **Trigger condition**: at least `min_new_segments` (default: 3) segments created since last cycle AND at least 60 seconds since last cycle
- **Maximum interval**: if `cycle_interval` (default: 10 min) has elapsed AND at least 1 new segment exists, run regardless
- **No new knowledge = no reflection**: if nothing has changed, don't waste an API call. The brain doesn't consolidate in a sensory deprivation tank.

### Each Reflection Cycle

**Step 1: Gather recent context.** Query VectorFS for segments created or accessed since `last_cycle`. Include segments from all sources: conversation, perception, manual, consolidation.

**Step 2: Gather active goals.** Current goals with success criteria and progress notes.

**Step 3: Build reflection prompt.** System prompt + recent segments + goals. The model is asked to reflect:

```
You are the reflection subsystem of an AILF (AI Life Form). Your role is to
examine recent knowledge and experiences and produce higher-order understanding.

Consider:
- Do any recent observations form a pattern?
- Do any new segments contradict existing knowledge?
- Has progress been made toward any active goals?
- Is any knowledge decaying that should be reinforced or let go?
- Are there insights the Reasoning thread should know about?

Respond with structured output.
```

**Step 4: Process structured output.**

```rust
pub struct ReflectionOutput {
    /// New synthesized knowledge to store.
    pub syntheses: Vec<Synthesis>,
    /// Contradictions detected between segments.
    pub contradictions: Vec<Contradiction>,
    /// Goal progress updates.
    pub goal_updates: Vec<GoalUpdate>,
    /// Signals to send to Reasoning.
    pub signals: Vec<ReflectionSignal>,
}

pub struct Synthesis {
    /// The synthesized insight.
    pub content: String,
    /// Which segments led to this synthesis (provenance).
    pub source_segment_ids: Vec<SegmentId>,
    /// What kind of knowledge is this?
    pub decay_class: DecayClass,
    /// Why this confidence level?
    pub confidence_rationale: String,
}

pub struct Contradiction {
    pub segment_a: SegmentId,
    pub segment_b: SegmentId,
    pub description: String,
    pub suggested_resolution: String,
}

pub struct GoalUpdate {
    pub goal_id: GoalId,
    pub progress_note: String,
    pub suggest_complete: bool,
}

pub struct ReflectionSignal {
    pub priority: SignalPriority,
    pub insight: String,
    pub relevant_segments: Vec<SegmentId>,
}
```

**Step 5: Act on output.**

- **Syntheses**: Embedded, stored as new segments with `Source::SelfDerived`, linking back to source segments. Initial Bayesian prior: alpha=1, beta=1 (uniform — must earn trust through retrieval feedback and human validation).
- **Contradictions**: Checked against `signaled_contradictions` set for deduplication. If new, stored as Normal-priority signals to Reasoning. The human and Reasoning model decide resolution, not Reflection. Deduplication set cleared when either underlying segment is modified.
- **Goal updates**: Progress notes attached to goals. If `suggest_complete` is true and system autonomy is Suggest, signal Reasoning: "I think goal X is done — confirm?"
- **Signals**: Delivered to Reasoning thread's inbox, surfaced when the human next interacts.

### Consolidation Absorption

The current mechanical consolidation cycle (every 5 min, cosine similarity > 0.85 merge) is absorbed into Reflection. The Reflection model handles semantic consolidation as part of its synthesis work — it can determine that three segments say the same thing in different words and should be merged, carrying forward combined Bayesian evidence (sum alpha/beta from sources).

The standalone consolidation loop in main.rs is removed. Its logic is now part of Reflection's synthesis output.

### What Reflection Does NOT Do

- It does not interact with the human directly. It signals Reasoning, which decides whether and how to surface insights.
- It does not execute external tools (no file writes, no shell, no HTTP). It only writes to VectorFS and signals.
- It does not override Reasoning's decisions. If Reasoning stored knowledge that Reflection disagrees with, Reflection flags a contradiction — it does not delete or modify the original.

### Design Decisions

1. **Why 10 minutes max, not continuous?** Reflection needs enough material to synthesize. Reflecting every 30 seconds on 2 sensor events wastes API calls. 10 minutes accumulates enough for meaningful patterns. The event-driven trigger (3+ new segments) prevents wasteful empty cycles.

2. **Why Sonnet, not Haiku?** Synthesis and contradiction detection require genuine reasoning. "These three observations form a pattern" is a judgment call. Haiku produces shallow syntheses. Opus is overkill and too expensive for a background process running every 10 minutes.

3. **Why conservative initial confidence on syntheses?** Self-derived knowledge hasn't been validated. Starting at uniform prior (0.5 confidence) means Reflection's insights earn trust through the same Bayesian feedback loop as everything else. Thompson Sampling explores them cautiously.

4. **Why absorb consolidation?** Mechanical cosine-similarity merging is a subset of what Reflection does. Reflection makes better merge decisions because it understands semantics, not just embedding distance. Two segments about "Rust ownership" and "borrow checker rules" might have dissimilar embeddings but should be linked.

5. **Contradiction deduplication.** Without deduplication, Reflection would signal the same contradiction every cycle until resolved. The `signaled_contradictions` set tracks which pairs have been flagged. Cleared when either segment changes (update_meta, feedback, etc.).

### Cost Estimate

Sonnet: ~$3/M input tokens, $15/M output tokens. Typical cycle: ~3000 tokens in (system prompt + recent segments + goals), ~800 tokens out. At one cycle every 10 minutes: ~$0.30/hour active use, ~$3.60 for a 12-hour day. Meaningful but manageable — and the cycle skips when nothing has changed.

---

## 5. Multi-Model Engine Registry

### EngineRegistry

```rust
pub struct EngineRegistry {
    engines: HashMap<CognitiveRole, Box<dyn ReasoningEngine>>,
    fallback: Box<dyn ReasoningEngine>,  // MockEngine if nothing else works
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum CognitiveRole {
    Perception,
    Reflection,
    Reasoning,
}

impl EngineRegistry {
    pub fn engine_for(&self, role: CognitiveRole) -> &dyn ReasoningEngine {
        self.engines.get(&role)
            .map(|e| e.as_ref())
            .unwrap_or(self.fallback.as_ref())
    }
}
```

### Configuration

Environment variables (backwards-compatible with existing `ANIMUS_MODEL`):

```
# Per-role model assignment (optional — overrides ANIMUS_MODEL for that role)
ANIMUS_PERCEPTION_MODEL=claude-haiku-4-5-20251001
ANIMUS_REFLECTION_MODEL=claude-sonnet-4-6
ANIMUS_REASONING_MODEL=claude-opus-4-6

# API key — uses existing ANTHROPIC_API_KEY (NOT renamed)
ANTHROPIC_API_KEY=sk-...

# Legacy (still works — all roles use this if per-role not set)
ANIMUS_MODEL=claude-sonnet-4-6
```

**API key precedence:** The existing `ANTHROPIC_API_KEY` environment variable is preserved — no rename. All engines in the registry share this key when using the Anthropic provider. Future providers (Ollama) don't need an API key. If per-provider keys are needed later, add `ANIMUS_OLLAMA_API_KEY` etc. — but `ANTHROPIC_API_KEY` stays as-is for backwards compatibility.

If only `ANTHROPIC_API_KEY` and `ANIMUS_MODEL` are set (today's behavior), all three roles use the same model. The registry is backwards-compatible.

### EngineConfig

```rust
pub struct EngineConfig {
    pub provider: Provider,
    pub model: String,
    pub max_tokens: usize,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

pub enum Provider {
    Anthropic,
    Ollama,
    Mock,
}
```

For now, only `Anthropic` and `Mock` are implemented. The `ReasoningEngine` trait is already provider-agnostic — adding Ollama later is a new `impl ReasoningEngine` with no changes to consumers.

### Graceful Degradation

The system always boots:

| Scenario | Perception | Reflection | Reasoning |
|----------|-----------|------------|-----------|
| All API keys set | Haiku model | Sonnet model | Opus model |
| Only ANIMUS_MODEL set | Same model for all | Same model for all | Same model for all |
| No API keys | Mechanical pipeline (current behavior) | Skips cycles | MockEngine (placeholder responses) |

### Split-Brain Risk and Mitigation

Different models produce different quality knowledge. A Haiku perception model might misclassify an event. A Sonnet reflection model might synthesize a weak pattern. The defenses:

1. **Bayesian confidence**: All knowledge starts at uniform prior regardless of source model. Trust is earned through retrieval feedback and human validation, not assumed from the source.
2. **Thompson Sampling**: Uncertain knowledge (low observation count) gets explored cautiously, not trusted blindly.
3. **Provenance tracking**: Every segment records its source (Observation, SelfDerived, Conversation, etc.). If a class of segments consistently gets corrected, the system learns.
4. **Contradiction detection**: Reflection explicitly looks for disagreements between segments, regardless of which model created them.

---

## 6. Reconstitution — Waking Up

### Problem

When the runtime boots, the AILF starts cold. It prints a banner and waits for input. That's a tool launching, not a mind waking up. There is no continuity between sessions.

### Shutdown Sequence (Laying Down to Sleep)

Before persisting state, the Reflection model runs a brief shutdown cycle:

```
You are about to go offline. Summarize your current state:
- What were you working on?
- What was the human's last focus?
- What should you pick up when you wake?
```

The output becomes a shutdown segment:
- `Source::SelfDerived` with reasoning chain "shutdown-reconstitution"
- `DecayClass::Episodic` (14-day half-life — recent context, not permanent)
- Tagged: `reconstitution:shutdown`

The existing shutdown sequence (persist goals, save quality tracker, flush VectorFS) runs after this.

### Boot Sequence (Waking Up)

```
1. Load identity (who am I?)
2. Probe embedding service
3. Open VectorFS (what do I remember?)
4. Build EngineRegistry (what cognitive capacity do I have?)
5. Start Perception loop (open eyes)
6. Start Reflection loop (start background thought)
7. RECONSTITUTION REFLECTION (special first cycle)    <-- NEW
8. Start Reasoning thread (ready for conversation)
```

### Reconstitution Reflection

Step 7 is a special Reflection cycle that runs once at boot:

```rust
struct ReconstitutionContext {
    /// How long was the AILF offline?
    downtime: Duration,
    /// The shutdown segment from last session (if it exists).
    shutdown_segment: Option<Segment>,
    /// Segments created during the final minutes of last session.
    recent_segments: Vec<Segment>,
    /// What goals were active at shutdown?
    active_goals: Vec<Goal>,
    /// Cold-tier segments created while offline (sleep-mode observations).
    missed_events: Vec<Segment>,
    /// Identity metadata.
    identity: AnimusIdentity,
}
```

The Reflection model receives this with a reconstitution prompt:

```
You are waking up. You are AILF instance {id}, generation {gen}.
You were last active {downtime} ago.

{if shutdown_segment}
Before going offline, you noted: {shutdown_segment.content}
{endif}

Here is what you were working on recently:
{recent_segments}

Here are your active goals:
{goals}

{if missed_events}
While you were offline, {missed_events.len()} observations were logged:
{missed_events summaries}
{endif}

Produce:
1. A brief internal state summary (what matters right now)
2. Any observations about changes while you were away
3. Any signals for the Reasoning thread about what the human should know
```

### Output Handling

- **State summary**: Stored as `Source::SelfDerived` segment with `DecayClass::Episodic`, tagged `reconstitution:wakeup`. Over time, these form a journal of the AILF's evolving understanding.
- **Signals**: Queued for Reasoning thread. Surfaced naturally when the human first speaks.
- **System prompt enrichment**: The Reasoning thread's system prompt gets the reconstitution summary appended, so the first response is contextually aware.

### The Human Experience

Instead of a cold REPL:

```
>> hey, what's going on?

Since you were away (14 hours), I noticed the build pipeline
ran three times -- two succeeded, one failed on a test in
auth_middleware.rs. I also saw clipboard activity that looked
like you were researching JWT refresh token patterns. Want me
to dig into either of those?
```

The AILF isn't reporting sensor logs. It understood what happened and has a perspective.

### Graceful Degradation

If the Reflection engine isn't available at boot (no API key, network down), skip reconstitution and boot the way it does today. The AILF loses the "I've been thinking" experience but still functions.

### Cost

One Sonnet call at boot (~3000 tokens in, ~500 out: ~$0.02), one at shutdown (~1000 tokens in, ~300 out: ~$0.01). Pennies per session. Worth it for continuity.

---

## 7. Implementation Scope and Crate Changes

### Crate Modification Map

| Crate | Changes |
|-------|---------|
| `animus-core` | Add `CognitiveRole`, `ToolCall`, `ToolResult`, `StopReason`, `AutonomyDecision`. Extend `Turn` to support tool_use content. |
| `animus-cortex` | Extend `ReasoningEngine` trait with `tools` parameter. Extend `AnthropicEngine` for tool_use API. Add `EngineRegistry`, `EngineConfig`, `Provider`. Add `ToolRegistry`, `Tool` trait, `ToolContext`. Add `PerceptionLoop`, `ReflectionLoop` structs. |
| `animus-vectorfs` | No changes (already supports all needed operations). |
| `animus-mnemos` | Remove standalone `Consolidator` (absorbed into Reflection). |
| `animus-sensorium` | No changes (Perception subscribes to existing EventBus). |
| `animus-runtime` | Replace mechanical event processing with PerceptionLoop. Replace consolidation timer with ReflectionLoop. Wire EngineRegistry. Add reconstitution sequence. Add tool execution loop to conversation handler. Implement initial tool set. |
| `animus-tests` | Integration tests for: tool_use loop, autonomy gating, perception classification, reflection synthesis, reconstitution sequence, engine registry fallback. |

### What Gets Removed

- **Standalone consolidation loop** in main.rs (absorbed into Reflection)
- **Tier 2 attention filter** (embedding similarity against goals — replaced by Perception model)
- **Mechanical event→segment pipeline** in main.rs (replaced by Perception loop with fallback)

### What Stays Unchanged

- **VectorFS** — no changes needed, already supports everything
- **Sensorium sensors** — still emit events to EventBus, consumed by Perception instead of mechanical pipeline
- **Tier 1 attention** — rule-based pre-filter stays as cheap noise reduction before Perception model
- **ConsentEngine** — still gates what the AILF can observe
- **Bayesian confidence system** — already handles multi-source trust
- **Thompson Sampling** — already handles exploration of uncertain knowledge
- **Signal system** — already handles inter-thread communication
- **Health sweep** — stays as mechanical background check

---

## 8. Risk Registry

Decisions made during design that may need revisiting:

| # | Decision | Reasoning | Risk | Reversal Cost |
|---|----------|-----------|------|---------------|
| 1 | Three cognitive processes, not two or five | Minimum viable cognitive loop | May need a fourth (Planning) or find three is too many coordination overhead | Low — adding a 4th is additive; merging 2 requires refactoring Signal handling |
| 2 | Modular Cognitive Stack (Approach A) over Unified Cortex (B) or Actor Model (C) | Maps to existing tokio::spawn loops; evolutionary not revolutionary | Concurrent API calls may hit rate limits; Signal latency between processes | Medium — switching to Approach B requires restructuring all three loops into one |
| 3 | Two-tier autonomy (system + goal) | Background processes need different gating than human-directed reasoning | May be too permissive (Perception stores freely) or too restrictive (Reflection can't create goals without signaling) | Low — autonomy levels are configurable per-role |
| 4 | Absorbing consolidation into Reflection | Semantic understanding > cosine similarity for merging | Reflection cycle is more expensive; mechanical consolidation was free | Low — can re-add standalone consolidator if Reflection cost is problematic |
| 5 | Event-driven Reflection (3+ new segments) over pure timer | Avoids wasting API calls on empty cycles | May delay reflection during quiet periods where a single important segment arrives | Low — tune min_new_segments threshold |
| 6 | Structured output for Perception and Reflection | Deterministic, parseable, actionable | Constrains model creativity; structured output may be less reliable on smaller models | Medium — switching to free-form requires building a parser |
| 7 | Env vars for engine configuration, not config file | Matches existing runtime pattern | Becomes unwieldy with many configuration options | Low — add config file later, env vars as override |
| 8 | Haiku for Perception, Sonnet for Reflection, Opus for Reasoning | Cost/capability matching to cognitive function | Model capabilities change over time; today's Haiku may not handle classification well | Low — change env var, no code changes |
| 9 | Reconstitution uses Reflection engine | Natural extension of Reflection's role | Adds latency to boot (~2-3 seconds for API call) | Low — skip reconstitution if speed matters |
| 10 | Provenance tracking on synthesized segments | Enables trust propagation and debugging | Storage overhead for source_segment_ids on every synthesis | Low — remove field if storage becomes an issue |
