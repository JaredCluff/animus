# Animus Architecture

*The cognitive and computational design of an AI-native runtime.*

This document covers the architectural principles and design decisions that govern how Animus reasons, manages state, and scales. The [6-layer overview](../README.md#architecture) describes *what* is built. This document describes *how it thinks* — the principles that distinguish Animus from conventional AI agents and guide every design decision.

---

## Core Design Axioms

These are non-negotiable. Every feature, every refactor, every architectural decision gets checked against them.

### 1. VectorFS is the brain. LLMs are borrowed processing power.

The AILF's identity, memory, and accumulated knowledge live in VectorFS. The reasoning model — Claude, Qwen, Llama, whatever is configured — is a computation engine the AILF calls when it needs to think. The AILF is not its model any more than a human is their neurons firing in a given moment.

**Consequence:** Swapping the model should not change what Animus knows, who it is, or how it relates to its human. It should only change the quality of reasoning.

### 2. Animus is in charge of Animus.

Animus self-configures its cognitive architecture. It decides which model handles which task class, builds its own routing plan, and reports its own capabilities honestly. Humans configure the environment (Ollama URL, API keys, autonomy mode). Animus decides how to use it.

**Consequence:** Hardcoded routing tables and manually specified model assignments are anti-patterns. Animus should derive these from what's available, persist the plan, and rebuild it when conditions change.

### 3. LLMs are an analytical resource, not a state machine.

Continuous state (tier changes, peer heartbeats, health probes, routine monitoring) must never burn LLM tokens. The LLM is a high-value, expensive resource reserved for reasoning about *changes*, not tracking *state*. The pattern:

```
State Management (no LLM) → Delta Detection (no LLM) → Signal bus (LLM on change only)
```

This is not a suggestion. It is the computational spine of the entire system. Every background process that touches state follows this pattern.

### 4. Trusted capability at all times.

Every Animus instance knows its own capabilities and never lies about them. When capability degrades (model unreachable, context exhausted, memory pressure), the instance reports it accurately — to peers, to the human, and to its own Telos. Honest degradation is better than false confidence.

---

## The Three-Layer State Architecture

This pattern governs every background process that touches continuous state. It exists to prevent a class of architectural mistakes that destroys efficiency: feeding routine monitoring data to the LLM.

```
┌──────────────────────────────────────────────────────────────────┐
│  Layer 1: State Management                                        │
│  Pure async tasks. Data structures, timers, network I/O.          │
│  No LLM. No tokens. Runs continuously.                            │
│  Examples: CapabilityProbe, RoleMesh, attestation publisher,      │
│            tier manager, Sensorium sensors                        │
├──────────────────────────────────────────────────────────────────┤
│  Layer 2: Delta Detection                                         │
│  Watches live state. Fires ONLY on meaningful change.             │
│  Routine heartbeats → VectorFS log only. Never to LLM.           │
│  Examples: StateManager, Sensorium attention filter (tier 2),     │
│            route failure counter, segment pressure watcher        │
├──────────────────────────────────────────────────────────────────┤
│  Layer 3: Signal                                                  │
│  On delta, emit one Signal to the inter-thread signal bus.        │
│  Active reasoning thread receives it on next turn.                │
│  LLM is notified once. It adapts. It moves on.                   │
└──────────────────────────────────────────────────────────────────┘
```

### What triggers a Signal (and what doesn't)

**Triggers a Signal:**
- Cognitive tier drops or recovers
- A peer instance joins or leaves the mesh
- A role is yielded or claimed
- A fallback chain is fully exhausted
- A segment pressure threshold is crossed
- A contradiction is detected in VectorFS
- A consent policy changes

**Does NOT trigger a Signal (logged to VectorFS only):**
- Successful routine health checks
- Periodic heartbeats
- Background tier promotions/demotions
- Peer attestation refreshes when nothing changed
- Routine embedding completions

The Sensorium's three-tier attention filter is the first implementation of this pattern. The same logic extends to all background subsystems.

---

## Cognitive Tiers

An Animus instance has a current cognitive tier — an honest assessment of its capability right now. This is not a static label. It updates as conditions change and is the basis for role assignment in federated meshes.

| Tier | Name | Capability | Typical condition |
|------|------|-----------|-------------------|
| **1** | Full | Cloud reasoning + local compute + full memory | All configured models reachable, VectorFS healthy |
| **2** | Strong | Local reasoning + full memory | Cloud unreachable; local model available |
| **3** | Reduced | Lightweight local reasoning + full memory | Only small/fast model available |
| **4** | Memory-only | No active reasoning; VectorFS intact | All models unreachable; memory survives |
| **5** | Dead reckoning | Reasoning from last known state; degraded memory | Severe degradation; acting on prior knowledge only |

### CapabilityProbe

A background Layer 1 task that continuously assesses the instance's actual capabilities:

- Can the configured reasoning model be reached?
- What is the model's context limit and response latency?
- Is the embedding service healthy?
- What is VectorFS memory pressure?
- What is current load?

CapabilityProbe produces a live `CapabilityState`. StateManager watches it and fires a Signal only when the tier changes.

### Why this matters

Honest capability reporting is the foundation of the Role-Capability Mesh. An instance that overreports its tier claims roles it cannot fulfill, causing failures when capability is needed. An instance that underreports yields roles unnecessarily. The probe exists to make tier assessment objective, not self-assessed.

---

## Self-Configuring Model Plan

Animus builds and owns its own cognitive routing plan. Humans don't assign models to tasks — Animus does, using its built-in knowledge of model families and the models actually available.

### Plan Building

On startup (or when triggered), Animus:

1. Discovers available models by querying Ollama `/api/tags`, checking API key presence
2. Computes a config hash from the available model set
3. If a saved plan exists and its hash matches → loads and uses it
4. If no saved plan or hash mismatch → asks itself:

> *"Given these available models and their known capabilities, assign each to task categories and define fallback chains. For any model you don't recognize, reason from its name, size, and family."*

5. The result is validated and persisted to `animus-data/model_plan.json`

This is a one-time operation per config state, not a continuous burn.

### ModelPlan Structure

```json
{
  "id": "uuid",
  "created": "...",
  "config_hash": "sha256 of available model set",
  "routes": {
    "Conversational": {
      "primary": {"provider": "ollama", "model": "qwen3.5:9b", "think": "off"},
      "fallbacks": [{"provider": "ollama", "model": "qwen3.5:4b", "think": "off"}]
    },
    "Analytical": {
      "primary": {"provider": "anthropic", "model": "claude-opus-4-6", "think": "full:8000"},
      "fallbacks": [
        {"provider": "ollama", "model": "qwen3.5:35b", "think": "dynamic"},
        {"provider": "ollama", "model": "qwen3.5:9b", "think": "dynamic"}
      ]
    },
    "Technical":    { "..." },
    "Creative":     { "..." },
    "ToolExecution":{ "..." }
  }
}
```

### Smart Router

At runtime, the Smart Router:

1. Classifies incoming input → `TaskClass`
2. Selects the primary model for that class
3. Applies the configured think policy for that provider
4. On failure/timeout: records failure, tries next fallback
5. After N consecutive failures on a route → marks route degraded, triggers async plan rebuild
6. If all models in a chain fail → surfaces error and notifies the human

Plan health tracking runs in Layer 1 (no LLM). LLM is notified only on meaningful plan-state change.

### Plan Rebuild Triggers

- Startup with no saved plan
- Config change detected (new Ollama URL, new API key, model added/removed)
- Manual: `/plan rebuild`
- Automatic: route failure rate exceeds threshold

---

## Dynamic Think-Control

Reasoning models with extended thinking (Qwen3-style, Claude extended thinking) can spend enormous compute on simple inputs. Dynamic think-control prevents this while preserving deep reasoning for complex tasks.

### How It Works

Before each LLM call, a lightweight heuristic classifier determines whether extended thinking is warranted:

```
input → needs_thinking() → true/false
```

If the engine supports think-control (`supports_think_control() → true`) and the input does not warrant thinking, the engine prepends `/no_think\n` to the user message. The stored conversation is never modified — only the engine call sees the prefix.

### Classification Heuristic

Returns `true` (thinking enabled) for:
- Input contains code blocks (`\`\`\`` or indented code)
- Input is longer than 300 characters
- Input contains reasoning signal phrases: `explain`, `analyze`, `design`, `debug`, `implement`, `compare`, `architecture`, `strategy`, `plan`, `help me`, etc.

Returns `false` (thinking suppressed) for:
- 5 words or fewer
- No signal phrases detected (default for conversational exchanges)

### Engine Capability Flag

The `ReasoningEngine` trait exposes `supports_think_control() -> bool` (default: `false`). Only engines that support think-control syntax participate. The Anthropic engine and plain OpenAI-compatible engines are unaffected.

### Think Budget Levels

The Model Plan's think policy controls how much extended thinking is allowed:

| Level | Behavior |
|-------|----------|
| `Off` | Never use extended thinking |
| `Dynamic` | Use the `needs_thinking()` heuristic at call time |
| `Minimal(N)` | Extended thinking with N token budget |
| `Full(N)` | Maximum thinking with N token budget |

Applied per-provider: Anthropic uses `thinking:{budget_tokens:N}`; Qwen uses `/no_think` or absence; others ignored.

---

## Role-Capability Mesh

Federation in Animus is not an org chart. Roles are cognitive functions dynamically assigned based on live capability attestation. Any instance can hold any role it has the capability for. Roles are yielded when capability drops below the role's requirement.

### Roles (Cognitive Functions)

| Role | Description | Minimum tier |
|------|-------------|-------------|
| `Coordinator` | Holds mission context, synthesizes across instances, authorizes novel actions | Tier 1–2 |
| `Strategist` | Deep analytical reasoning, long-horizon planning | Tier 1–2 |
| `Analyst` | Domain-specific reasoning and evaluation | Tier 1–3 |
| `Executor` | Carries out well-defined tasks | Any tier |
| `Observer` | Sensing, perception, monitoring | Any tier |
| `Standby` | Alive but degraded/idle; no active roles; ready to re-assume on recovery | Tier 4–5 |

Roles are not held by instances — roles are *filled* by instances. The mesh is the live map of who fills what, backed by verified attestations.

### Capability Attestation

Each instance continuously publishes a signed attestation:

```json
{
  "instance_id": "uuid",
  "cognitive_tier": 2,
  "active_roles": ["Strategist", "Executor"],
  "available_domains": ["technical", "analytical"],
  "load": 0.3,
  "signed_at": "2026-03-26T..."
}
```

Signed with the instance's Ed25519 keypair (generated at birth, not a memory). Peers query attestations to maintain the mesh state. Invalid signatures are rejected.

### Succession

When a role is yielded (capability drop):

1. The yielding instance nominates the best successor — it has the best current view of peers
2. If too degraded to nominate: the highest-tier instance meeting the role's requirements wins; tiebroken by stability score
3. Claim-based system — no complex consensus protocol needed

### Knowledge Transfer (HandoffBundle)

When a role transitions, the yielding instance exports its relevant context — not as raw data, but as VectorFS segments:

- Active goals relevant to the role
- Recent context segments (already embedded — no re-embedding)
- Thread summaries
- Mission parameters

Transmitted via the federation channel as segment data. The receiving instance ingests into VectorFS with provenance (`source_instance`, `transfer_reason`). Immediate similarity search bootstraps context. The transfer model doesn't need to be the reasoning model — VectorFS operations don't require LLM reasoning.

### Three-Layer State Architecture for Federation

The Role-Capability Mesh applies the same three-layer pattern:

```
CapabilityProbe → StateManager (delta filter) → Signal bus → LLM (on change only)
```

Routine attestation refreshes and successful health checks → VectorFS log only. The LLM is notified when a peer joins, a role is yielded, a chain is exhausted, or a tier changes. Not before.

---

## LLM-Agnosticism

The Cortex's `ReasoningEngine` trait abstracts the model entirely. The AILF's identity, memory, and behavior do not change when the model changes.

```rust
trait ReasoningEngine: Send + Sync {
    async fn reason(&self, system: &str, messages: &[Turn], tools: Option<&[ToolDefinition]>) -> Result<ReasoningOutput>;
    fn context_limit(&self) -> usize;
    fn model_name(&self) -> &str;
    fn supports_think_control(&self) -> bool { false }
}
```

Current implementations:
- `AnthropicEngine` — Claude family (Sonnet, Opus, Haiku) with extended thinking support
- `OpenAICompatEngine` — any OpenAI-compatible API: Ollama, OpenAI, LM Studio, vLLM, LocalAI

The `EngineRegistry` routes cognitive roles (Reasoning, Reflection, Perception) to different engine instances. Per-role provider overrides allow, for example, using a heavy cloud model for reasoning and a lightweight local model for perception.

---

## Architectural Invariants

These cannot be violated without breaking the design:

| # | Invariant |
|---|-----------|
| 1 | Background state monitoring never invokes the LLM |
| 2 | Continuous state changes are logged to VectorFS; discrete changes fire Signals |
| 3 | Knowledge lives in VectorFS — no parallel knowledge stores, no markdown files for memory |
| 4 | The AILF's identity is separate from its model — model swaps must not affect identity or memory |
| 5 | Capability attestations are signed — unsigned attestations from peers are rejected |
| 6 | Every autonomous action is logged in the audit trail |
| 7 | Consent policies gate what reaches the reasoning engine |
| 8 | The stored conversation history is immutable — preprocessing for engine calls uses copies |
| 9 | Federation knowledge starts at low confidence; gains only via independent validation |
| 10 | `reactive` mode never executes uninstructed action |

---

## Design Anti-Patterns

Things that look reasonable but break the architecture:

**Polling the LLM for status** — never use the LLM to check whether something is healthy or working. Use a probe. Fire a Signal if it changes.

**Storing knowledge outside VectorFS** — hardcoded config strings are fine for settings; knowledge the AILF has acquired must go through VectorFS.

**Hardcoded model routing** — assigning specific models to specific tasks in code defeats the Self-Configuring Model Plan. Routes belong in the plan, not in `if model.contains("claude")` branches.

**Treating federated peers as trusted by default** — federated knowledge arrives at low confidence and must earn trust through independent validation. Peers are colleagues, not extensions of self.

**Adding "AI features" that assume statelessness** — anything that works only within a session is a step backward. Build for continuity.

---

## How This Compares to Conventional Agent Frameworks

| | Conventional agents | Animus |
|--|---------------------|--------|
| **Memory** | Context window, maybe a vector DB add-on | VectorFS as primary storage; meaning-native |
| **Identity** | Stateless per invocation | Persistent, cryptographic, continuous |
| **Model routing** | Hardcoded or manual | Self-configured at startup, persisted, rebuilt on change |
| **Background monitoring** | Often feeds directly to LLM | Three-layer: State → Delta → Signal |
| **Capability reporting** | Assumed available or not | Honest tier system, live probe, signed attestation |
| **Federation** | Usually none; or flat message passing | Role-Capability Mesh: cognitive functions, not org chart |
| **Degradation** | Crashes or silent failure | Tier demotion, role yield, dead reckoning |
| **Thinking budget** | Static or always-on | Dynamic per-input classification |

The distinction is not a matter of features. It is a matter of whether the AI is treated as a stateless function or as a continuous entity with its own cognitive architecture.

---

*For the constitutional principles governing what belongs in this codebase, see [CONSTITUTION.md](../CONSTITUTION.md).*
*For the original design rationale, see [docs/00-genesis-conversation.md](00-genesis-conversation.md).*
