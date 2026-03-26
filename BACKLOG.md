# Animus Backlog

Tracks what's been shipped and what's next. Organized by layer from the design spec.

---

## ✅ Shipped

### Foundation (Phase 1–2)
- VectorFS: mmap-backed segments, HNSW index, hot/warm/cold tiering, snapshot/restore
- Mnemos: context assembly, intelligent eviction with summaries, background consolidation, quality gate
- Cortex: reasoning threads, LLM abstraction (Anthropic), Telos goal system, thread scheduler
- Sensorium: event bus, file watcher, network monitor, segment pressure watcher, sensorium health watcher
- Identity: principal registry, Ed25519 keypair, situational awareness
- Terminal interface
- Runtime: full orchestration, sleep/wake, autonomy modes, API budget tracking, goal manager

### Channels
- Telegram adapter: text, images, voice send/receive, Markdown→HTML, inline voice player (sendVoice)
- NATS adapter: pub/sub, JetStream, reply routing
- ChannelBus: structured PermissionGate, injection scanner, message router

### Voice
- `macos-stt` repo: standalone macOS STT HTTP service (SFSpeechRecognizer + Swift, Bearer auth)
- `animus-voice` crate: AnimusVoiceService — STT via macos-stt HTTP, TTS via Cartesia (MP3→OGG Opus via ffmpeg)
- Voice toggle: `/voice on|off|status` at runtime without restart; state persisted across restarts
- Spoken-style LLM hint for voice turns (no markdown, no tables, concise)
- `macos-stt` launchd service: `~/Library/LaunchAgents/com.jaredcluff.macos-stt.plist` (auto-start, auto-restart)

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
- Reflection loop (background LLM memory synthesis)
- Proactive mode: goal deadline watcher + urgent signal forwarding → Telegram; gated by autonomy mode
- Tier 2 attention filter: embedding cosine similarity threshold (configurable, default 0.25)
- Consent commands: `/consent list|allow|deny`
- Audit export: `/audit export [json|csv]`
- Multi-LLM: Ollama + OpenAI-compatible backends; per-role provider overrides (ANIMUS_LLM_PROVIDER, ANIMUS_{REASONING,REFLECTION,PERCEPTION}_PROVIDER)

---

## 🔲 Backlog

### High Priority

**Desktop control** *(new — requested 2026-03-25)*
- Screen capture tool (`desktop_screenshot`) — needs Screen Recording TCC permission
- Mouse/keyboard control via CGEvent Swift helper (`desktop_click`, `desktop_type`, `desktop_key`)
- Vision-model grounding: screenshot → find element by description → coordinates
- Use case: click permission dialogs, interact with macOS UI remotely

**macOS permission grants** *(blocking voice STT)*
- Speech Recognition: grant in System Settings > Privacy > Speech Recognition (physical/screen-share required)
- Screen Recording: same (once desktop control is built)
- Accessibility: same (for mouse/keyboard control)

**Full federation protocol + Role-Capability Mesh** *(2026-03-26)*

Federated Animus instances operate as a Role-Capability Mesh — not an org chart. Roles are
cognitive functions dynamically assigned based on live capability attestation. Any instance
can hold any role it has the capability for. Roles are yielded when capability drops below
the role's requirement; the instance retains other roles it can still fulfill.

*Roles (cognitive functions)*
- `Coordinator` — holds mission context, synthesizes across instances, authorizes novel actions
- `Strategist` — deep analytical reasoning, long-horizon planning (requires Tier 1–2)
- `Analyst` — domain-specific reasoning and evaluation
- `Executor` — carries out well-defined tasks (works at any tier)
- `Observer` — sensing, perception, monitoring (works at any tier)
- `Standby` — alive but degraded/idle, no active roles, ready to re-assume on recovery

*Capability Attestation (ties into CapabilityProbe)*
Each instance continuously publishes a signed attestation:
`{instance_id, cognitive_tier, active_roles, available_domains, load, signed_at}` — signed with Ed25519 keypair (keypair already in place). Peers query attestations to maintain the mesh state.

*Succession*
When a role is yielded (capability drop):
1. Yielding instance nominates best successor (it has the best view of peers)
2. If too degraded to nominate: highest-tier instance meeting role requirements wins, tiebroken by stability score
3. Claim-based system — no complex consensus protocol needed

*Knowledge Transfer (HandoffBundle)*
- VectorFS-native: yielding instance exports active goals, recent context segments (already embedded), thread summaries, mission parameters
- Transmitted via existing federation channel as segment data
- Receiving instance ingests into VectorFS with provenance (source_instance, transfer_reason)
- Immediate similarity search bootstraps context — no re-embedding needed
- Transfer model doesn't need to be the reasoning model (VectorFS ops, not LLM reasoning)

*What to build*
- `RoleRegistry`: role definitions with min capability requirements per role
- `CapabilityAttestation`: live state, signed, published to peers (extends CapabilityProbe)
- `HandoffBundle`: VectorFS export/import for role transitions
- `SuccessionPolicy`: per-role nomination/election rules
- `RoleMesh`: live map of who holds what, backed by verified attestations
- DNS-SD discovery of peer instances on LAN (foundation for mesh formation)
- Ed25519 signature verification on federated segments
- Trust model: federated knowledge starts at low confidence, gains via independent validation

### Medium Priority

**Self-Configuring Model Plan + Smart Router** *(2026-03-26 — Animus is in charge of Animus)*

Animus builds and owns its own cognitive routing plan — not hardcoded by humans, but decided by Animus at startup using its own model knowledge. One-and-done: plan is built once, persisted, reused until config changes or failure forces a rebuild.

*Plan Building (on startup or config change)*
1. Discover available models: query Ollama `/api/tags`, check Anthropic/OpenAI key presence
2. Compute a config hash; if saved plan matches hash → load and use it
3. If no saved plan or hash mismatch: Animus asks itself (via whatever engine is available):
   > "Given these available models and their known capabilities, assign each to task categories and define fallback chains. For any model you don't recognize, reason from its name/size/family."
4. LLM returns a structured JSON plan; validate and persist to `animus-data/model_plan.json`

*ModelPlan (persisted)*
```json
{
  "id": "uuid",
  "created": "...",
  "config_hash": "sha256 of available models",
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
    "Technical": { ... },
    "Creative": { ... },
    "ToolExecution": { ... }
  }
}
```

*Think Budget Engine (layered on top)*
- `ThinkLevel { Off | Dynamic | Minimal(tokens) | Full(tokens) }`
- Applied per-provider: Anthropic → `thinking:{budget_tokens:N}`; Qwen → `/no_think` or absence; others ignored
- "Dynamic" = use current `needs_thinking()` heuristic at call time

*Smart Router (runtime)*
- Classifies incoming input → `TaskClass`
- Selects primary model from plan for that class
- Applies think policy for the selected provider+model
- On failure/timeout: records failure, tries next fallback in chain
- After N consecutive failures on a route → marks route degraded, triggers async plan rebuild with remaining models
- If all models in a chain fail → surface error + notify user

*Plan Rebuild Triggers*
- Startup with no saved plan
- Config change detected (new ANIMUS_OLLAMA_URL, new API key, model added/removed)
- Manual: `/plan rebuild` command
- Auto: route failure rate exceeds threshold

*Animus guidance*
- Animus uses its built-in knowledge of model families (Claude, Qwen, Llama, Mistral, etc.)
- For unknown models: infers from name/size (e.g., "deepseek-r1:70b" → likely strong for analytical)
- Optional: web search tool to look up model benchmarks before deciding

Foundation already in place: `supports_think_control()` flag, `needs_thinking()` heuristic, `ReasoningEngine` trait, `EngineRegistry` per-role dispatch, Ollama model listing.

**Inter-thread signaling (formal)**
- Typed Signal messages: Info / Normal / Urgent priorities
- Currently threads communicate but without the formal Signal type from the spec
- Enables: background thread notifying active thread of goal completion, sensorium alerts

### Lower Priority

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

**Config hot-reload**
- Apply config changes (env var overrides) without container restart
- Currently requires `podman stop/start`

**VectorFS block-level storage**
- Replace mmap backing with custom block layout optimized for vector access patterns
- Long-term goal from spec; current mmap implementation is stable and sufficient

---

## 🔧 Known Issues

- **Speech Recognition TCC permission** — macos-stt requires physical click in System Settings; remote grant via sqlite3 doesn't survive macOS Sonoma TCC validation
- **Reflection output parse errors** — `ReflectionLoop` occasionally produces non-UUID segment IDs causing parse warnings (cosmetic, doesn't affect function)
- **VectorFS bincode deserialization warnings** — some old segments fail to deserialize after schema changes; cosmetic, skipped silently
