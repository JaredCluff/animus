# Animus Backlog

Tracks what's been shipped and what's next. Organized by layer from the design spec.

---

## ✅ Shipped

### Foundation (Phase 1–2)
- VectorFS: mmap-backed segments, HNSW index, hot/warm/cold tiering, snapshot/restore
- Mnemos: context assembly, intelligent eviction with summaries, background consolidation, quality gate
- Cortex: reasoning threads, LLM abstraction (Anthropic + OpenAI-compatible), Telos goal system, thread scheduler
- Sensorium: event bus, file watcher, network monitor, segment pressure watcher, sensorium health watcher
- Identity: principal registry, Ed25519 keypair, situational awareness
- Terminal interface
- Runtime: full orchestration, sleep/wake, autonomy modes, API budget tracking, goal manager

### Channels
- Telegram adapter: text, images, voice send/receive, Markdown→HTML, inline voice player (sendVoice)
- NATS adapter: pub/sub, JetStream, reply routing, explicit flush on publish
- ChannelBus: structured PermissionGate, injection scanner, message router
- NATS ↔ Claude Code two-way communication via nuntius MCP server
- NATS debug mirror: mirrors all NATS traffic to Telegram when `ANIMUS_NATS_DEBUG=1`

### Voice
- `macos-stt` repo: standalone macOS STT HTTP service (SFSpeechRecognizer + Swift, Bearer auth)
- `animus-voice` crate: AnimusVoiceService — STT via macos-stt HTTP, TTS via Cartesia (MP3→OGG Opus via ffmpeg)
- Voice toggle: `/voice on|off|status` at runtime without restart; state persisted across restarts
- Spoken-style LLM hint for voice turns (no markdown, no tables, concise)
- `macos-stt` launchd service: `~/Library/LaunchAgents/com.jaredcluff.macos-stt.plist` (auto-start, auto-restart)

### Cortex — Smart Router & Model Plan
- Self-configuring model plan: discovers available models, builds routing plan from capability profiles
- Capability-scored routing: weighted scoring (quality, speed, reasoning, cost) per task class
- HeuristicClassifier: zero-LLM-cost input classification via keyword matching
- 5 task classes: Conversational, Analytical, Technical, ToolExecution, Voice
- Tool group filtering per task class (reduces context size for smaller models)
- Engine cascade with automatic fallback on failure
- Plan persistence: `model_plan.json` on data volume, rebuilt on config hash change
- Multi-provider support: Ollama, Anthropic, Cerebras, NIM, OpenRouter, Groq (all OpenAI-compatible)
- Per-role provider overrides (ANIMUS_{REASONING,REFLECTION,PERCEPTION}_PROVIDER)
- Model health watcher: periodic probing of all endpoints, marks engines healthy/unhealthy
- Provider health-weighted routing: unhealthy engines skipped in cascade
- Rate limit tracking: parses OpenAI-compatible rate limit headers, near-limit warnings
- Budget tracking: per-model cost estimation, monthly budget with pressure tiers

### Cortex — Tool System
- 32+ registered tools across 8 groups (comms, web, filesystem, memory, tasks, routing, federation, system)
- `nats_publish`: proactive NATS publishing with flush, debug mirror integration
- Tool hallucination detection: detects models claiming tool use without actual tool calls, auto-retries with corrective prompt
- Tool catalog auto-generated from registry (prevents prompt/reality drift)
- Autonomy-gated tool execution (Inform/Suggest/Act/Full levels)

### Cortex — Reasoning
- Inter-thread signaling: typed Signal messages with Info/Normal/Urgent priorities
- Reflection loop: background LLM memory synthesis
- Proactive mode: goal deadline watcher + urgent signal forwarding → Telegram; gated by autonomy mode
- Tier 2 attention filter: embedding cosine similarity threshold (configurable, default 0.25)
- External system prompt: preamble and suffix loaded from `system_prompt_preamble.md` / `system_prompt_suffix.md` on data volume (editable without recompile)

### Federation (Phase 5 — partial)
- `federate_segment` tool: push segments to remote AILF instances
- K2K broadcast channel integration
- PermissionGate: structured permission request/grant flow via NATS

### Ops
- Docker/Podman multi-stage build, compose.yaml
- Health endpoint (`GET /health`)
- Periodic snapshots with pruning
- Claude Code OAuth + ANTHROPIC_API_KEY auth
- Embedding preservation on provider change
- Multi-instance discovery (PR #37)
- Consent commands: `/consent list|allow|deny`
- Audit export: `/audit export [json|csv]`
- OpenTelemetry → Langfuse tracing

---

## 🔲 Backlog

### High Priority

**Desktop control** *(requested 2026-03-25)*
- Screen capture tool (`desktop_screenshot`) — needs Screen Recording TCC permission
- Mouse/keyboard control via CGEvent Swift helper (`desktop_click`, `desktop_type`, `desktop_key`)
- Vision-model grounding: screenshot → find element by description → coordinates
- Use case: click permission dialogs, interact with macOS UI remotely

**macOS permission grants** *(needed for desktop control only — STT now uses Groq Whisper)*
- Screen Recording: needed for `desktop_screenshot` tool
- Accessibility: needed for mouse/keyboard control via CGEvent

**Improve tool-use reliability for free-tier models**
- Some models (qwen-3-235b on Cerebras, qwen3-coder-480b on NIM) hallucinate tool calls
- Current mitigations: RULE 4 in system prompt, hallucination detection + retry, ToolExecution routing
- Next steps: model-level tool-use reliability scoring, deprioritize unreliable models for tool-heavy tasks
- Consider `tool_choice: "required"` when input clearly needs a tool call

**Full federation protocol + Role-Capability Mesh** *(2026-03-26)*

Federated Animus instances operate as a Role-Capability Mesh — not an org chart. Roles are
cognitive functions dynamically assigned based on live capability attestation.

*What remains to build:*
- `RoleRegistry`: role definitions with min capability requirements per role
- `CapabilityAttestation`: live state, signed, published to peers (Layer 1 — no LLM)
- `StateManager`: delta detector, filters continuous state → discrete change events (Layer 2)
- `HandoffBundle`: VectorFS export/import for role transitions
- `SuccessionPolicy`: per-role nomination/election rules
- DNS-SD discovery of peer instances on LAN
- Ed25519 signature verification on federated segments
- Trust model: federated knowledge starts at low confidence, gains via independent validation

### Medium Priority

**Web/HTTP channel adapter**
- REST API for programmatic access (beyond Telegram)
- Useful for integrating with other tools, n8n, webhooks

**Image generation tool**
- `generate_image` Cortex tool via DALL-E or Stable Diffusion
- Send generated images via Telegram

**Calendar / email sensors**
- Sensorium sensors for calendar events, email arrival
- Triggers proactive mode: "you have a meeting in 15 minutes"

**Multi-user Telegram support**
- Different trusted users with different permission levels
- Currently: single trusted user ID list, all-or-nothing

### Lower Priority

**Config hot-reload**
- Apply config changes (env var overrides) without container restart
- System prompt files are already external (hot-reload on restart)
- Currently other config requires `podman stop/start`

**VectorFS block-level storage**
- Replace mmap backing with custom block layout optimized for vector access patterns
- Long-term goal from spec; current mmap implementation is stable and sufficient

---

## 🔧 Known Issues

- **Free-tier model tool hallucination** — some models claim to call tools without actually invoking them; mitigated by hallucination detection + retry (now with `tool_choice:required`) but not fully solved

## ✅ Recently Resolved

- **Reflection output parse errors** — fixed in PR #74: lenient deserialization for `Contradiction`/`GoalUpdate` skips non-UUID entries rather than failing the entire parse
- **VectorFS bincode deserialization warnings** — fixed in PR #74: V0/V1 migration on load, unrecoverable files quarantined (never deleted)
- **Speech Recognition TCC permission** — not blocking; STT chain tries Groq Whisper first (`GROQ_API_KEY` is set, 282ms, free tier). macOS STT is last-resort fallback only.
