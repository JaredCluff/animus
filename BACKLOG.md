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

**Full federation protocol**
- DNS-SD discovery of peer AILF instances on LAN
- Ed25519 signature verification on federated segments (identity keypair is present, signing is not)
- Trust model: federated knowledge starts at low confidence, gains via independent validation
- Federated goals: organizational coordination across instances

### Medium Priority

**Multi-LLM support**
- Ollama as reasoning backend (for fully local operation — currently Ollama is embeddings only)
- OpenAI-compatible endpoint support
- Per-role model selection: reasoning vs. reflection vs. perception can use different models

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
