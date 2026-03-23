# Contributing to Animus

Welcome. Before diving in, please read [CONSTITUTION.md](CONSTITUTION.md) — it explains what Animus is, what it isn't, and what kinds of contributions fit the project. It will save you from building the wrong thing.

## Getting Started

### Prerequisites

- Rust 1.75+
- [Ollama](https://ollama.ai/) with `mxbai-embed-large` pulled (`ollama pull mxbai-embed-large`)
- Docker or [Podman](https://podman.io/) for container builds
- A Claude API key or Claude Max OAuth credentials (see [DEPLOYMENT.md](DEPLOYMENT.md))

### Build

```bash
git clone https://github.com/JaredCluff/animus
cd animus
cargo build
cargo test
```

### Run locally (interactive mode)

```bash
export ANTHROPIC_API_KEY=your_key_here
export ANIMUS_OLLAMA_URL=http://localhost:11434
cargo run --bin animus
```

### Run as a container (production mode)

```bash
cp .env.example .env
# edit .env with your tokens
podman compose --env-file .env up -d
podman compose --env-file .env logs -f
```

See [DEPLOYMENT.md](DEPLOYMENT.md) for full configuration reference.

## Architecture Overview

Animus is a Rust workspace with layered crates:

```
animus-core         → shared types, traits, config (no dependencies on other crates)
animus-vectorfs     → Layer 1: semantic storage, HNSW index, tier management
animus-mnemos       → Layer 2: context assembly, eviction, consolidation
animus-embed        → embedding service abstraction
animus-cortex       → Layer 4: reasoning engine, LLM integration, tools, goals
animus-sensorium    → Layer 3: sensors, event bus, consent, audit
animus-channel      → Layer 5: ChannelBus, channel adapters, injection scanner
animus-interface    → terminal interaction
animus-federation   → peer discovery, knowledge sharing
animus-runtime      → AILF lifecycle, daemon entry point
animus-tests        → integration tests
```

The dependency direction is generally inward: runtime depends on everything; core depends on nothing. Do not introduce circular dependencies.

## Code Style

- Standard `cargo fmt` and `cargo clippy` — run both before submitting
- Errors use `animus_core::AnimusError` via the `?` operator; avoid `unwrap()` in production paths
- Async-first: use `tokio` for concurrency; avoid blocking in async contexts (`spawn_blocking` for blocking I/O)
- Log with `tracing::{info, debug, warn, error}` — not `println!`
- Keep crates focused: if a new capability doesn't fit cleanly in an existing crate, propose a new one

## How to Contribute

1. **Open an issue first** for anything non-trivial — describe what you want to build and why it fits the project. This saves wasted effort.
2. Fork and create a feature branch: `git checkout -b feat/my-feature`
3. Write tests. Integration tests go in `animus-tests`. Unit tests live alongside the code they test.
4. Run `cargo test --workspace` and `cargo clippy --workspace` before opening a PR.
5. Open a PR with a clear description of what it does and why.

---

## Open Areas for Contribution

These are the highest-priority gaps. Each item links to relevant context where available.

### Phase 2 Features (High Priority)

#### `browse_url` — Headless Browser Tool
**Crate:** `animus-cortex/src/tools/`
**What:** A tool that uses a headless browser (e.g., via `chromiumoxide` or `playwright`) to fetch JavaScript-rendered pages. `http_fetch` handles static pages well; React/Vue/Next apps need a real browser.
**Why:** Animus learns which sites need which tool and routes accordingly. The goal is adaptive: try `http_fetch` first, fall back to `browse_url` if the response is empty or just a JS root.
**Prior art:** The design calls for per-domain learning stored in VectorFS so Animus remembers which sites need the headless path.

#### Gmail Channel Adapter
**Crate:** `animus-channel/src/gmail/`
**What:** A `ChannelPlugin` implementation for Gmail. Polls for new emails, applies injection scanning, routes to reasoning threads. Can send replies.
**Why:** Email is a critical communication surface. Jared's use case includes Animus reading Gmail and surfacing important messages proactively.
**Notes:** OAuth2 via Google; careful injection scanning is mandatory (email is a high-risk injection vector).

#### Google Calendar Integration
**Crate:** `animus-cortex/src/tools/` or `animus-channel/src/calendar/`
**What:** Tools to read upcoming events and create new ones via the Google Calendar API.
**Why:** ADHD management — Animus should be aware of Jared's schedule and proactively surface conflicts, approaching deadlines, and needed preparation.

#### Discord Channel Adapter
**Crate:** `animus-channel/src/discord/`
**What:** A `ChannelPlugin` for Discord. Responds in DMs and designated channels.
**Notes:** Use the Discord REST API directly (no heavyweight bot framework needed, consistent with the Telegram adapter pattern).

#### Slack Channel Adapter
**Crate:** `animus-channel/src/slack/`
**What:** A `ChannelPlugin` for Slack via the Events API.

#### Groq/Cerebras Fast Triage Bridge
**Crate:** `animus-cortex/src/llm/`
**What:** Optional `ReasoningEngine` implementations for Groq and Cerebras. These providers offer sub-100ms inference, making them ideal for the triage/perception role where classification speed matters more than depth.
**Why:** The `EngineRegistry` has a `Perception` role specifically for fast classification. Currently it falls back to Haiku. A Groq engine here would be significantly faster.
**Notes:** Optional — the system works without it. Set `ANIMUS_PERCEPTION_MODEL` to a Groq model to activate.

#### Screen Capture Tool
**Crate:** `animus-cortex/src/tools/`
**What:** A tool that captures a screenshot on the host Mac and returns the image path for analysis by `analyze_image`.
**Notes:** macOS-specific initially (uses `screencapture`). Investigate UI-Venus-1.5 for screen handling techniques. Must respect consent policies.

### Security (High Priority)

#### DeBERTa v3 Injection Detection
**Crate:** `animus-channel/src/scanner.rs`
**What:** Upgrade `InjectionScanner` to use a locally-running DeBERTa v3 NLI model as a third classification tier, after heuristics and LLM scanning.
**Why:** Neural injection detection is significantly more robust against adversarial phrasing than keyword matching. Runs locally so it doesn't add latency from network calls.
**Current state:** The scanner has a `Suspicious` threshold that triggers optional LLM classification. DeBERTa would replace or supplement the LLM path.

### Runtime Improvements (Medium Priority)

#### Thread Preemption (Priority-Based)
**Crate:** `animus-cortex/src/scheduler.rs`
**What:** When a Critical or High priority message arrives while a Normal priority thread is reasoning, preempt the current reasoning round and handle the high-priority message first.
**Current state:** The `ThreadScheduler` routes messages to threads but doesn't preempt mid-reasoning. Threads complete their current turn before yielding.
**Notes:** Preemption must be cooperative — Anthropic API calls cannot be interrupted mid-stream. The right hook is between reasoning rounds in the tool-use loop.

#### Rate Limit Tracking
**Crate:** `animus-cortex/src/llm/anthropic.rs`
**What:** Parse `anthropic-ratelimit-*` response headers and expose current limit state to the `EngineRegistry`. When near a limit, the scheduler should prefer a different provider or back off.
**Why:** Prevents hard 429 errors. Makes Animus a good citizen to its providers.

#### Temp File Cleanup
**Crate:** `animus-channel/src/telegram/`
**What:** Auto-cleanup of downloaded photo files in `/tmp/animus-downloads` after they've been processed by `analyze_image`.
**Current state:** Files are downloaded and analyzed but never cleaned up.

#### OpenAI Provider Bridge
**Crate:** `animus-cortex/src/llm/`
**What:** A `ReasoningEngine` implementation for the OpenAI API (GPT-4o, etc.).
**Why:** LLM-agnostic by design. OpenAI compatibility also enables Ollama's OpenAI-compatible endpoint for fully local reasoning.

### Long-Term Vision

#### Voice Interface
**What:** A microphone/speaker interface so Animus can participate in voice conversations.
**Notes:** Should use speech-to-text (Whisper) and TTS locally where possible. Out of scope until channel infrastructure is solid.

#### AILF Fork/Clone UX
**What:** The `/fork` command that creates a new AILF instance from a VectorFS snapshot, with divergent identity from that point.
**Why:** Enables experimental branches of an AILF's development, specialized instances, and posthumous knowledge federation.

#### Federation Improvements
**Crate:** `animus-federation/`
**What:** The federation layer is implemented but lightly tested in production. Work needed: trust tier enforcement, knowledge expiration across federation peers, conflict resolution when federated segments contradict local knowledge.

#### Ollama as Reasoning Backend
**What:** Use the Ollama API (OpenAI-compatible endpoint) as a fully local `ReasoningEngine` — no cloud API needed. Enables air-gapped or cost-free deployments.

---

## Adding a New Tool

1. Create `crates/animus-cortex/src/tools/my_tool.rs`
2. Implement `Tool` trait: `name()`, `description()`, `parameters_schema()`, `required_autonomy()`, `execute()`
3. Add `pub mod my_tool;` to `crates/animus-cortex/src/tools/mod.rs`
4. Register in `crates/animus-runtime/src/main.rs`: `reg.register(Box::new(my_tool::MyTool));`
5. Update the bootstrap entry in `crates/animus-runtime/src/bootstrap.rs` to describe the new tool
6. Update the tools table in `README.md`

The `required_autonomy()` level controls when the tool is available:
- `Inform` — always available; purely informational, no side effects
- `Suggest` — available in Suggest mode and above; may store data, no external writes
- `Act` — requires Act or Full autonomy; writes files, makes HTTP requests, sends messages

## Adding a New Channel Adapter

1. Create `crates/animus-channel/src/my_channel/mod.rs`
2. Implement `ChannelPlugin` trait: `id()`, `name()`, `start(bus)`, `send(msg)`, `is_configured()`
3. Wire into `start_all()` in `bus.rs` or register from `main.rs`
4. All inbound messages must go through `MessageRouter` and `InjectionScanner`
5. Respect the priority system — map channel-native urgency signals to `MessagePriority`

## Questions?

Open an issue. If you're unsure whether something fits the project, ask before building — the [CONSTITUTION.md](CONSTITUTION.md) is the first reference, but the maintainers are happy to clarify.
