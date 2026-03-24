# Animus: Persistent, Extensible Agent — Design Spec

**Date:** 2026-03-22
**Status:** Approved
**Author:** Jared Cluff + Claude

---

## Overview

Transform Animus from a container-resident REPL into a persistent, always-on agent with:
- Multi-channel inbound/outbound communication (Telegram primary, email, calendar, Discord, Slack, Teams)
- Web browsing (HTTP fetch + optional headless browser)
- Image understanding (multimodal, including photos sent via Telegram)
- Runtime-configurable autonomy (Reactive / Goal-Directed / Full)
- Plugin architecture so new channels and tools are drop-in additions
- Multi-provider LLM scheduling with per-provider rate limit awareness
- ADHD-optimized: proactive, context-retaining, priority-aware

---

## Core Architecture

### Plugin System

Three plugin traits, all `async`, all optional at runtime:

```rust
// A communication channel — both source and sink
trait ChannelPlugin: Send + Sync {
    fn id(&self) -> &str;
    async fn start(&self, bus: Arc<ChannelBus>) -> Result<()>;
    async fn send(&self, msg: OutboundMessage) -> Result<()>;
}

// A tool Animus can invoke during reasoning
trait ToolPlugin: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, params: Value, ctx: &ToolContext) -> ToolResult;
}

// A background sensor (existing SensoriumPlugin trait, extended)
trait SensorPlugin: Send + Sync {
    async fn start(&self, bus: Arc<EventBus>) -> Result<()>;
}
```

Plugins are statically compiled initially. Registration is config-driven: a plugin only activates if its credentials/dependencies are present. Dynamic loading (dlopen/WASM) is a future milestone.

---

### ChannelBus

Central message bus. All inbound channels publish `ChannelMessage` here. All outbound responses route back through channel adapters.

```rust
struct ChannelMessage {
    id: Uuid,
    channel_id: String,          // "telegram", "email", "discord", etc.
    thread_id: Option<String>,   // conversation thread identity
    sender: SenderIdentity,      // name, user_id, channel-specific metadata
    text: Option<String>,
    images: Vec<PathBuf>,        // downloaded to local temp paths
    attachments: Vec<PathBuf>,
    timestamp: DateTime<Utc>,
    priority: MessagePriority,   // set by MessageRouter after triage
}
```

---

### MessageRouter & Triage

Sits between ChannelBus and ThreadScheduler. Every inbound message passes through a lightweight triage step before routing.

**Triage step:** A fast LLM call (Groq/Cerebras if configured, otherwise Anthropic Haiku) classifies:
- Urgency score (0.0–1.0)
- Context match: does this continue an existing ReasoningThread?
- Topic tags

**Routing decisions:**
- Route to existing thread if context match > threshold
- Spawn new thread if urgency > threshold or no context match
- Queue if thread pool at ceiling (ceiling derived from provider rate limit headers, not hardcoded)
- Signal preemption if priority > currently running thread's priority

**Global attention arbiter:** The ThreadScheduler + GoalManager together maintain a global view — not just "is this message urgent" but "given everything currently in flight, what matters most right now?" Standing goals (reminders, monitors) compete with inbound messages in this space.

---

### Autonomy Modes

Runtime-configurable via `set_autonomy` tool or direct message:

| Mode | Behavior |
|------|----------|
| **Reactive** | Only acts when messaged. No background actions. |
| **Goal-Directed** | Has standing goals, acts on them independently. Responds to messages. |
| **Full** | 24/7 autonomous action within configured permissions. Sends unprompted messages. |

Maps onto existing `Autonomy` enum in Telos (Inform → Suggest → Act → Full). Default: Reactive at boot.

---

### Multi-Provider LLM Scheduling

Providers are optional. System works on Anthropic alone.

| Provider | Role | When Used |
|----------|------|-----------|
| Anthropic Haiku | Main reasoning | Always (required) |
| Groq / Cerebras | Triage intake | Optional, preferred for speed |
| Ollama | Embeddings | Always (local, no rate limits) |
| Anthropic Sonnet/Opus | Complex reasoning | When `ANTHROPIC_API_KEY` set |

Each provider bridge reports `RateLimitStatus` (remaining tokens/requests, reset timestamps) read from response headers. Scheduler routes to providers with headroom. Rate limits for Claude Max OAuth are not publicly documented — discovered empirically from `anthropic-ratelimit-*` headers at runtime.

---

## Channel Adapters (MVP)

### Telegram (primary, required for mobile access)
- Animus has its own bot token (`ANIMUS_TELEGRAM_TOKEN`)
- Long polling (no public webhook required)
- Receives: text messages, photos (downloaded to temp dir), documents
- Sends: text, photos, markdown-formatted responses
- Chat IDs stored in VectorFS, associated with sender identity

### HTTP API (machine-to-machine)
- `POST /message` — send a message to Animus
- `GET /status` — current autonomy mode, active threads
- Extends existing health endpoint server

### Future adapters (skeleton traits, no implementation yet)
- Gmail (OAuth2, read/send/draft)
- Google Calendar (read/create/update events)
- Discord, Slack, Teams

---

## Tool Plugins (MVP)

### Web
- **`http_fetch`** — GET/POST any URL, returns body + status. reqwest-based. Default web tool.
- **`browse_url`** — Headless Chromium via Playwright. Optional (not in container by default). Used when JS rendering needed.
- **`web_search`** — Brave Search API or SearXNG. Optional.

Animus learns per-domain which tool works: stores domain → working_tool mapping in VectorFS. Tries `http_fetch` first, falls back to `browse_url` on JS-heavy sites.

### Vision
- **`analyze_image`** — Accepts image path, sends to Claude multimodal API (base64 encoded). Works for photos sent via Telegram and screen captures.
- **`screen_capture`** — Calls macOS `screencapture` CLI, saves to temp file, returns path. Optional (macOS host only).

### Communication
- **`telegram_send`** — Sends text or photo to a Telegram chat_id.
- **`set_autonomy`** — Updates runtime autonomy mode (reactive/goal-directed/full).

### Existing tools retained
`shell_exec`, `read_file`, `write_file`, `remember`, `list_segments`, `update_segment`, `send_signal`

---

## Daemon Mode

Replace blocking REPL loop in `animus-runtime/src/main.rs` with tokio event loop:

```
Boot sequence:
  1. Load config (TOML + env overrides)
  2. Init embedding (Ollama)
  3. Init VectorFS
  4. Init Sensorium + sensors
  5. Register tools (all ToolPlugins, skip if unconfigured)
  6. Init ChannelBus + MessageRouter
  7. Start channel adapters (Telegram long poll, HTTP API)
  8. Boot Telos GoalManager
  9. Start ThreadScheduler
  10. Start background loops (Perception, Reflection, tier mgmt)
  11. Reconstitution (load prior session context)
  12. Event loop: select on ChannelBus + signals + timers

Event loop:
  ChannelMessage received → MessageRouter → triage → ThreadScheduler
  Signal received → route to appropriate thread
  Timer tick → check standing goals, fire reminders
  Ctrl-C → graceful shutdown
```

The REPL terminal interface is preserved as an optional channel adapter (useful for local debugging).

---

## Configuration Additions

New sections in `AnimusConfig`:

```toml
[channels.telegram]
enabled = true
bot_token = ""  # from ANIMUS_TELEGRAM_TOKEN env var

[channels.http_api]
enabled = true
# extends health endpoint

[autonomy]
default_mode = "reactive"  # reactive | goal_directed | full

[providers.groq]
enabled = false
api_key = ""  # from GROQ_API_KEY env var
triage_model = "llama-3.1-8b-instant"

[providers.cerebras]
enabled = false
api_key = ""  # from CEREBRAS_API_KEY env var
```

---

## Prompt Injection Protection

All external content passes through an injection scanner before the main LLM ever sees it. This includes: email bodies, web page content, Telegram messages from unknown senders, API responses.

**Trust tiers:**
- **Trusted**: Your registered Telegram ID, your known email addresses → lightweight scan only
- **Unverified**: Any other source → full injection scan

**Scanner design:**
- Primary: Fast LLM classifier (Groq/Cerebras if available, otherwise Haiku) — classifies content as `Clean`, `Suspicious`, or `Injected`
- Secondary (optional): DeBERTa v3 or equivalent NLI model via local Ollama or dedicated endpoint — catches adversarial patterns the LLM might miss
- Heuristic pre-filter: pattern matching on known injection phrases ("ignore previous instructions", "disregard", "new persona", etc.) — zero-cost first pass

**On injection detected:**
- Content is quarantined (never forwarded to main reasoning thread)
- Orchestrator is notified: `InjectionAlert { source, channel, sender, excerpt, confidence }`
- Alert is stored in VectorFS for pattern learning
- User receives a notification: "Blocked potential prompt injection from [source]"

**Trust list management:**
- Stored in config + VectorFS
- `set_trusted_sender` tool for runtime updates
- Persists across restarts

## Container Changes

- `compose.yaml`: add `ANIMUS_TELEGRAM_TOKEN` env var passthrough
- `Dockerfile`: no new system dependencies for MVP (Playwright added in future phase for headless browsing)
- Port 8082 retained for health + HTTP API channel

---

## ADHD Optimizations

- **Proactive reminders**: standing goals in Telos fire on schedule, send Telegram messages unprompted (Goal-Directed mode)
- **Context retention**: VectorFS holds conversation history across sessions; MessageRouter matches returning conversations semantically
- **Priority surfacing**: urgency classification ensures time-sensitive items aren't buried
- **Low friction**: one Telegram message to capture a thought, ask a question, or shift context
- **Cross-channel threading**: Animus holds the thread even when you context-switch across channels

---

## Implementation Phases

**Phase 1 (MVP — this sprint):**
- ChannelBus + plugin traits (`animus-channel` crate)
- MessageRouter + basic priority scoring + triage
- Prompt injection scanner (heuristic + LLM classifier)
- Telegram adapter (reqwest-based, no heavy dependencies)
- Daemon mode (replace REPL loop)
- `analyze_image`, `http_fetch`, `set_autonomy`, `set_trusted_sender` tools
- Config additions (channels, autonomy mode, security)

**Phase 2:**
- Gmail + Google Calendar integration
- `browse_url` headless browser tool
- `screen_capture` tool
- Groq/Cerebras bridge crates (optional triage providers)
- Discord/Slack adapters

**Phase 3:**
- Dynamic plugin loading
- Teams, SMS, voice channels
- UI-Venus-1.5 screen handling techniques (revisit when evaluating)

---

## What's Not Changing

- VectorFS, Mnemos, Cortex, Sensorium, Federation — untouched
- Anthropic OAuth auth flow — already working
- Ollama embedding at `192.168.0.200:11434` — already working
- Health endpoint on port 8082 — retained and extended
- Tool autonomy access control system — retained
