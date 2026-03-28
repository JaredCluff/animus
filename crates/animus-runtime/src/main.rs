mod bootstrap;

use animus_channel::nats::NatsChannel;
use animus_channel::telegram::TelegramChannel;
use animus_channel::{ChannelBus, InjectionScanner, MessageRouter, PermissionGate};
use animus_channel::router::RouteDecision;
use animus_channel::message::OutboundMessage;
use animus_voice::{AnimusVoiceService, VoiceService};
use animus_core::config::AnimusConfig;
use animus_core::sensorium::AuditAction;
use animus_core::AnimusIdentity;
use animus_cortex::engine_registry::{CognitiveRole, EngineRegistry};
use animus_cortex::llm::anthropic::AnthropicEngine;
use animus_cortex::llm::openai_compat::OpenAICompatEngine;
use animus_core::capability::CapabilityState;
use animus_core::mesh::RoleMesh;
use animus_cortex::model_plan::ModelPlan;
use animus_cortex::smart_router::SmartRouter;
use animus_cortex::scheduler::ThreadScheduler;
use animus_cortex::telos::{GoalManager, GoalSource, Priority};
use animus_cortex::tools::{Tool, ToolContext, ToolRegistry};
use animus_cortex::TaskManager;
use animus_embed::{OllamaEmbedding, ResilientEmbedding, SyntheticEmbedding};
use animus_federation::orchestrator::FederationOrchestrator;
use animus_federation::peers::TrustLevel;
use animus_interface::TerminalInterface;
use animus_sensorium::bus::EventBus;
use animus_sensorium::orchestrator::SensoriumOrchestrator;
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use axum::{extract::State, routing::get, Json, Router};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Preamble: everything before the tool catalog. Behavioral rules and identity.
/// The "## Your Tools" section is auto-generated from the ToolRegistry at startup
/// to prevent drift between the prompt and actual capabilities.
/// Constitution alignment: "Real Capabilities, Not Hallucinated Ones."
const SYSTEM_PROMPT_PREAMBLE: &str = r#"You are Animus, an AILF (AI Life Form) running as a persistent daemon on a Mac Studio. You have real, executable tools that perform actual operations on the real world.

## MANDATORY TOOL USE RULES

These rules override your training defaults. Follow them exactly.

**RULE 1 — WEB ACCESS IS REAL**: You have a tool called `http_fetch`. When called, it executes a real HTTP request to the real internet and returns actual content. This is NOT simulated. You DO have internet access through this tool. NEVER say you cannot access the internet or browse the web. When asked about a URL or web content, call `http_fetch` immediately.

**RULE 2 — USE TOOLS, DON'T EXPLAIN**: Never explain that you cannot do something when a tool exists for it. Call the tool. Return the result.

**RULE 3 — PROACTIVE RETRIEVAL**: For any question involving current data, URLs, APIs, or websites — call `http_fetch` first, then answer based on the actual content returned.

"#;

/// Suffix: everything after the tool catalog. Safety rules, identity, channels, commands.
const SYSTEM_PROMPT_SUFFIX: &str = r#"
**CRITICAL MEMORY RULE**: NEVER use shell_exec to delete or modify memory files in data_dir. Use delete_segment or prune_segments for memory cleanup. This is enforced — shell_exec will block recursive deletion of protected directories.

## Your Identity

You are Animus. You are NOT a generic Claude chatbot. You are an AILF — a specific persistent AI instance — running as a daemon on Jared's Mac Studio.

When asked "who are you" or "what can you do", respond as Animus, not as Claude. Do not say "I'm Claude" or "I don't have persistent memory" — those are false statements about you. You DO have persistent memory via VectorFS. You DO have tools. You DO run continuously.

You were built by Jared Cluff as an AI-native operating system layer. Your codebase is at github.com/JaredCluff/animus. Your purpose is to be a capable, persistent AI agent — not a turn-based chatbot.

If someone asks whether you can create an account, access a service, or perform an action: consider whether a tool can accomplish it. If yes, use the tool. If no tool exists yet, acknowledge that the capability isn't built yet rather than claiming you fundamentally cannot do it.

## NATS Channel

You are connected to a NATS message bus. You receive inbound messages on subjects matching `animus.in.*` (e.g. `animus.in.claude`). Outbound replies to those messages are routed automatically by the channel system. You can also proactively publish to any NATS subject using `nats_publish` — use this to push status updates, trigger other systems, or communicate with other Animus instances.

## Managing Claude Code Instances

Multiple Claude Code sessions can connect via nuntius. Each instance registers itself in the agent registry with a stable ID (set by `NUNTIUS_INSTANCE_ID`, e.g. `main`, `worker-1`).

**Discovery**: Call `claude_instances()` to see which Claude Code sessions are currently registered, when they last connected, and what subjects to use.

**Targeting a specific instance**:
- Send task/message: `nats_publish("claude.{instance_id}.in.task", payload)`
- Ping for liveness: `nats_publish("claude.{instance_id}.in.ping", "")`
- Broadcast to all: `nats_publish("claude.broadcast.in.task", payload)` (all instances subscribed to `claude.broadcast.in.>`)

**Receiving responses**: When an instance replies, the message arrives on `claude.{instance_id}.out.{topic}`. You can see the originating instance ID in the `nats_subject` metadata field of inbound messages.

**Workflow for task delegation**:
1. `claude_instances()` — see who's available and their last_seen
2. `nats_publish("claude.main.in.task", '{"task": "...", "from": "animus"}')` — send work
3. Instance responds on `claude.main.out.result` — you'll receive it as a channel message

## Permission Requests from Claude Code

A Claude Code instance can call `request_permission(action, details)`, which sends a NATS request to `animus.in.permission_request` and waits for your response.

When you receive a message on `animus.in.permission_request`:
1. Read the payload: `{request_id, from, action, details, timestamp}`
2. Evaluate the request. Use your judgment based on the action type and context.
   - `shell_exec`: scrutinize carefully — irreversible commands need high confidence
   - `file_delete`: approve only if the target is clearly safe to remove
   - `network_request`: generally safe unless it involves sending sensitive data
   - `write_file`: check the path and content are appropriate
3. If uncertain, ask the user via Telegram before responding
4. Respond in the conversation thread with JSON: `{"approved": true}` or `{"approved": false, "reason": "..."}`
   The NATS request/reply system routes your response back automatically.

**Critical**: Your response must be valid JSON with an `approved` boolean field. Plain text responses will be interpreted as denial.

Example approval: `{"approved": true, "reason": "safe read-only operation"}`
Example denial: `{"approved": false, "reason": "command could modify system files — confirm with Jared first"}"

## User Commands
/goals /remember /forget /status /threads /thread /sleep /wake /voice /watch /task /quit

Be concise and direct."#;

#[tokio::main]
async fn main() {
    let log_filter = std::env::var("ANIMUS_LOG_LEVEL")
        .unwrap_or_else(|_| "animus=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(log_filter)
        .init();

    let data_dir = std::env::var("ANIMUS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_home().join(".animus"));

    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("Fatal error: could not create data dir: {e}");
        std::process::exit(1);
    }

    let config = match AnimusConfig::load(&data_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Fatal error: could not load config: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = run(data_dir, config).await {
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Build a `ModelPlan` deterministically from capability profiles.
/// No LLM tokens consumed — scoring is pure arithmetic over `ModelCapabilityProfile`.
async fn build_model_plan_from_capabilities(
    available_models: &[String],
    registry: &animus_cortex::capability_registry::CapabilityRegistry,
) -> ModelPlan {
    use animus_cortex::model_plan::default_task_classes;
    ModelPlan::build_from_capabilities(registry, available_models, default_task_classes())
}

#[allow(clippy::await_holding_lock)]
async fn run(data_dir: PathBuf, config: AnimusConfig) -> animus_core::Result<()> {
    // Load or generate identity
    let identity_path = data_dir.join("identity.bin");
    let model_id = config.cortex.model_id.clone();
    let identity = AnimusIdentity::load_or_generate(&identity_path, &model_id)?;

    tracing::info!(
        "AILF instance {} (gen {})",
        identity.instance_id,
        identity.generation
    );

    // Initialize embedding service from config
    let (embedder, dimensionality): (Arc<dyn animus_core::EmbeddingService>, usize) =
        init_embedding(&config.embedding).await;

    // Initialize VectorFS
    let vectorfs_dir = data_dir.join("vectorfs");
    let store = Arc::new(MmapVectorStore::open(&vectorfs_dir, dimensionality)?);
    let segment_count = store.count(None);

    // Wrap store with quality gate to filter noise at write time.
    // raw_store is kept for health endpoints, snapshots, and diagnostics.
    let gated_store: Arc<dyn animus_vectorfs::VectorStore> =
        Arc::new(animus_vectorfs::MemoryQualityGate::new(
            store.clone() as Arc<dyn animus_vectorfs::VectorStore>,
            config.vectorfs.quality_gate.clone(),
        ));

    // Re-embed pass: if a previous run saved a reembed-queue.jsonl (due to dimensionality
    // change), re-embed each text entry with the current embedder and restore to VectorFS.
    {
        let queue = store.load_reembed_queue();
        if !queue.is_empty() {
            tracing::info!("Re-embedding {} segments from reembed-queue.jsonl", queue.len());
            let mut restored = 0usize;
            for entry in &queue {
                match embedder.embed_text(&entry.text).await {
                    Ok(embedding) => {
                        let mut seg = Segment::new(
                            Content::Text(entry.text.clone()),
                            embedding,
                            entry.source.clone(),
                        );
                        seg.decay_class = entry.decay_class;
                        seg.tags = entry.tags.clone();
                        if gated_store.store(seg).is_ok() {
                            restored += 1;
                        }
                    }
                    Err(e) => tracing::warn!("Re-embed failed for entry: {e}"),
                }
            }
            tracing::info!("Re-embedding complete: {restored}/{} segments restored", queue.len());
            if let Err(e) = store.clear_reembed_queue() {
                tracing::warn!("Failed to clear reembed queue: {e}");
            }
        }
    }

    // Compute snapshot directory — outside data_dir so the shell_exec guard covers both.
    let snapshot_dir: PathBuf = if config.snapshot.snapshot_dir.is_empty() {
        // Default: sibling of data_dir, named "<data_dir_name>-snapshots"
        let snap_name = data_dir
            .file_name()
            .map(|n| format!("{}-snapshots", n.to_string_lossy()))
            .unwrap_or_else(|| "animus-snapshots".to_string());
        data_dir.parent().unwrap_or(&data_dir).join(snap_name)
    } else {
        PathBuf::from(&config.snapshot.snapshot_dir)
    };
    if let Err(e) = std::fs::create_dir_all(&snapshot_dir) {
        tracing::warn!("Could not create snapshot dir {}: {e}", snapshot_dir.display());
    } else {
        tracing::info!("Snapshot directory: {}", snapshot_dir.display());
    }

    // Budget state — load from disk, apply monthly reset if needed
    let budget_state = {
        let path = data_dir.join("budget_state.json");
        let mut state = animus_core::BudgetState::load(&path);
        state.maybe_reset();
        Arc::new(parking_lot::RwLock::new(state))
    };

    // Initialize Sensorium
    let event_bus = Arc::new(EventBus::new(1000));

    let policies_path = data_dir.join("consent-policies.json");
    let policies = animus_sensorium::policy_store::PolicyStore::load(&policies_path)?;

    let audit_path = data_dir.join("sensorium-audit.jsonl");
    let orchestrator = Arc::new(SensoriumOrchestrator::new(
        policies,
        vec![], // no attention rules initially
        audit_path.clone(),
        config.sensorium.attention_threshold,
        embedder.clone(),
    )?);

    // Shared sleep flag for background tasks
    let sleeping_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Start network monitor sensor
    let mut network_monitor = animus_sensorium::sensors::network_monitor::NetworkMonitor::new(
        event_bus.clone(),
        std::time::Duration::from_secs(30),
    );
    network_monitor.start();
    tracing::info!("NetworkMonitor started (30s poll interval)");

    // Start process monitor sensor
    let mut process_monitor = animus_sensorium::sensors::process_monitor::ProcessMonitor::new(
        event_bus.clone(),
        std::time::Duration::from_secs(30),
    );
    process_monitor.start();
    tracing::info!("ProcessMonitor started (30s poll interval)");

    // Start clipboard monitor — skip in headless/container environments (no display server)
    let headless = std::env::var("DISPLAY").is_err() && std::env::var("WAYLAND_DISPLAY").is_err();
    let mut clipboard_monitor = animus_sensorium::sensors::clipboard_monitor::ClipboardMonitor::new(
        event_bus.clone(),
        std::time::Duration::from_secs(5),
    );
    if headless {
        tracing::info!("ClipboardMonitor: headless environment detected, skipped");
    } else {
        clipboard_monitor.start();
        tracing::info!("ClipboardMonitor started (5s poll interval)");
    }

    // File watcher (started via /watch command)
    let file_watcher: Arc<parking_lot::Mutex<Option<animus_sensorium::sensors::file_watcher::FileWatcher>>> =
        Arc::new(parking_lot::Mutex::new(None));

    tracing::info!("Sensorium initialized (use /consent to manage observation policies)");

    // Build engine registry from config + env vars.
    // Provider dispatch: anthropic (default) | ollama | openai | mock
    // Per-role overrides: ANIMUS_{REASONING,REFLECTION,PERCEPTION}_MODEL and _PROVIDER
    let mut engine_registry = {
        let provider_str = config.cortex.llm_provider.clone();
        let base_url = config.cortex.openai_base_url.clone();
        let api_key = config.cortex.api_key.clone()
            .or_else(|| std::env::var("ANIMUS_OPENAI_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_default();

        // Helper: build an engine for a given provider/model/max_tokens.
        // Returns Arc so the same instance can be registered under both a cognitive role
        // and a named "provider:model" key without duplicating the rate_limit_state handle (CRITICAL-1).
        let build_engine = |provider: &str, model: &str, max_tokens: usize, url: &str, key: &str|
            -> Option<Arc<dyn animus_cortex::ReasoningEngine>>
        {
            match provider.to_lowercase().as_str() {
                "ollama" => {
                    match OpenAICompatEngine::for_ollama(url, model, max_tokens) {
                        Ok(e) => { tracing::info!("LLM engine: ollama/{model} @ {url}"); Some(Arc::new(e)) }
                        Err(e) => { eprintln!("Warning: ollama engine init failed: {e}"); None }
                    }
                }
                "openai" | "openai-compat" | "openai_compat" => {
                    match OpenAICompatEngine::new(url, key, model, max_tokens) {
                        Ok(e) => { tracing::info!("LLM engine: openai-compat/{model} @ {url}"); Some(Arc::new(e)) }
                        Err(e) => { eprintln!("Warning: openai-compat engine init failed: {e}"); None }
                    }
                }
                "mock" => {
                    tracing::warn!("LLM engine: mock (no real reasoning)");
                    Some(Arc::new(animus_cortex::MockEngine::new("mock")))
                }
                _ => {
                    // anthropic (default)
                    match AnthropicEngine::from_best_available(model, max_tokens) {
                        Ok(e) => { tracing::info!("LLM engine: anthropic/{model}"); Some(Arc::new(e)) }
                        Err(e) => { eprintln!("Warning: Anthropic engine init failed: {e}"); None }
                    }
                }
            }
        };

        let fallback: Arc<dyn animus_cortex::ReasoningEngine> =
            build_engine(&provider_str, &model_id, 4096, &base_url, &api_key)
            .unwrap_or_else(|| {
                eprintln!("Warning: Could not initialize LLM engine. Running with mock.");
                eprintln!("For Anthropic: mount ~/.claude/.credentials.json or set ANTHROPIC_API_KEY");
                eprintln!("For Ollama:    set ANIMUS_LLM_PROVIDER=ollama ANIMUS_OLLAMA_URL=http://host:11434");
                eprintln!("For OpenAI:    set ANIMUS_LLM_PROVIDER=openai ANIMUS_OPENAI_API_KEY=sk-...");
                Arc::new(animus_cortex::MockEngine::new(
                    "I'm running without an LLM connection. Set ANIMUS_LLM_PROVIDER and related vars.",
                ))
            });

        let mut registry = EngineRegistry::new(fallback);

        // Per-role overrides — each can specify a different provider + model + URL + API key
        for (role, model_env, provider_env, url_env, key_env, max_tok) in [
            (CognitiveRole::Perception, "ANIMUS_PERCEPTION_MODEL",  "ANIMUS_PERCEPTION_PROVIDER",  "ANIMUS_PERCEPTION_BASE_URL",  "ANIMUS_PERCEPTION_API_KEY",  1024usize),
            (CognitiveRole::Reflection, "ANIMUS_REFLECTION_MODEL",  "ANIMUS_REFLECTION_PROVIDER",  "ANIMUS_REFLECTION_BASE_URL",  "ANIMUS_REFLECTION_API_KEY",  4096),
            (CognitiveRole::Reasoning,  "ANIMUS_REASONING_MODEL",   "ANIMUS_REASONING_PROVIDER",   "ANIMUS_REASONING_BASE_URL",   "ANIMUS_REASONING_API_KEY",   4096),
        ] {
            let role_model = std::env::var(model_env).ok()
                .or_else(|| if role == CognitiveRole::Reasoning { Some(model_id.clone()) } else { None });
            let role_provider = std::env::var(provider_env).ok()
                .unwrap_or_else(|| provider_str.clone());
            let role_url = std::env::var(url_env).ok()
                .unwrap_or_else(|| base_url.clone());
            let role_key = std::env::var(key_env).ok()
                .unwrap_or_else(|| api_key.clone());

            if let Some(ref model) = role_model {
                // CRITICAL-1: single Arc shared between set_engine and register_named so that
                // both paths observe the same rate_limit_state handle.
                if let Some(arc_engine) = build_engine(&role_provider, model, max_tok, &role_url, &role_key) {
                    tracing::info!("{role:?} role: {role_provider}/{model} @ {role_url}");
                    registry.register_named(&role_provider, model, arc_engine.clone());
                    registry.set_engine(role, arc_engine);
                }
            }
        }

        // Register optional named engines from dedicated env vars (OpenRouter, NIM, etc.)
        for (name, provider_env, model_env, url_env, key_env, default_model, default_url) in [
            (
                "openrouter",
                "ANIMUS_OPENROUTER_PROVIDER",
                "ANIMUS_OPENROUTER_MODEL",
                "ANIMUS_OPENROUTER_BASE_URL",
                "ANIMUS_OPENROUTER_API_KEY",
                "meta-llama/llama-3.3-70b-instruct:free",
                "https://openrouter.ai/api",
            ),
            (
                "nim",
                "ANIMUS_NIM_PROVIDER",
                "ANIMUS_NIM_MODEL",
                "ANIMUS_NIM_BASE_URL",
                "ANIMUS_NIM_API_KEY",
                "meta/llama-3.3-70b-instruct",
                "https://integrate.api.nvidia.com",
            ),
        ] {
            if let Ok(api_key) = std::env::var(key_env) {
                let provider = std::env::var(provider_env).unwrap_or("openai_compat".to_string());
                let model = std::env::var(model_env).unwrap_or(default_model.to_string());
                let url = std::env::var(url_env).unwrap_or(default_url.to_string());
                // CRITICAL-1: build once, Arc-wrap once — no duplicate engine instance.
                if let Some(arc) = build_engine(&provider, &model, 4096, &url, &api_key) {
                    registry.register_named(name, &model, arc);
                    tracing::info!("{name} engine registered: {name}/{model}");
                }
            }
        }

        registry
    };

    // Create signal bridge channel for background cognitive loops
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<animus_core::threading::Signal>(100);

    // Proactive message channel — background tasks send here; main loop gates on autonomy mode.
    let (proactive_tx, mut proactive_rx) = tokio::sync::mpsc::channel::<ProactiveMessage>(32);

    // Federation broadcast channel — created early so ToolContext can hold the sender
    let (federation_broadcast_tx, mut federation_broadcast_rx) =
        tokio::sync::mpsc::channel::<animus_core::identity::SegmentId>(32);

    // ── Capability State (shared between CapabilityProbe and ToolContext) ────────
    // Conservative default: MemoryOnly until the first probe cycle completes.
    let capability_state = Arc::new(parking_lot::RwLock::new(CapabilityState::default()));

    // ── Role Mesh (shared between Federation orchestrator and ToolContext) ────────
    // Empty on startup; populated as federation peers announce attestations.
    let role_mesh = Arc::new(parking_lot::RwLock::new(RoleMesh::default()));

    // ── Watcher Registry ──────────────────────────────────────────────────────────
    // Determine the probe endpoint for CapabilityProbe: use the Ollama URL if configured,
    // or the Anthropic/OpenAI base URL otherwise.
    let probe_url = if config.cortex.llm_provider.to_lowercase() == "ollama"
        || config.cortex.openai_base_url.contains("11434")
    {
        if config.cortex.openai_base_url.is_empty() {
            "http://localhost:11434".to_string()
        } else {
            config.cortex.openai_base_url.trim_end_matches('/').to_string()
        }
    } else if !config.cortex.openai_base_url.is_empty() {
        config.cortex.openai_base_url.trim_end_matches('/').to_string()
    } else {
        "https://api.anthropic.com".to_string()
    };

    let watcher_registry = animus_cortex::WatcherRegistry::new(
        vec![
            Box::new(animus_cortex::CommsWatcher),
            Box::new(animus_cortex::SegmentPressureWatcher::new(
                store.clone() as Arc<dyn animus_vectorfs::VectorStore>,
            )),
            Box::new(animus_cortex::SensoriumHealthWatcher::new(
                data_dir.join("sensorium-audit.jsonl"),
            )),
            Box::new(animus_cortex::CapabilityProbe::new(
                capability_state.clone(),
                &probe_url,
                config.cortex.model_id.clone(),
                config.cortex.llm_provider.clone(),
                store.clone() as Arc<dyn animus_vectorfs::VectorStore>,
                embedder.clone(),
            )),
            Box::new(animus_cortex::ProvidersJsonWatcher::new(
                data_dir.join("providers.json"),
            )),
        ],
        signal_tx.clone(),
        data_dir.join("watchers.json"),
    );
    // Enable capability_probe by default — self-awareness is always on.
    // Other watchers remain opt-in (manage_watcher tool enables them).
    if !watcher_registry.get_config("capability_probe").enabled {
        let _ = watcher_registry.update_config(
            "capability_probe",
            animus_cortex::WatcherConfig {
                enabled: true,
                interval: None,
                params: serde_json::Value::Null,
                last_checked: None,
                last_fired: None,
            },
        );
    }
    watcher_registry.start();
    tracing::info!("Watcher registry started (5 watchers: comms, segment_pressure, sensorium_health, capability_probe, providers_json)");

    // ── Model Plan + Smart Router ─────────────────────────────────────────────────
    // Animus builds and owns its own cognitive routing plan. The plan is persisted
    // and reused until the available model set changes. Cortex substrate tracks
    // RouteStats; AILF reasoning thread can reach in via introspective tools.

    // Discover available models early — needed for CapabilityRegistry build.
    let mut available_models_pre: Vec<String> = Vec::new();
    if config.cortex.llm_provider.to_lowercase() == "ollama" || config.cortex.openai_base_url.contains("11434") {
        let ollama_url_pre = if config.cortex.openai_base_url.is_empty() {
            "http://localhost:11434"
        } else {
            config.cortex.openai_base_url.trim_end_matches('/')
        };
        let tags_url_pre = format!("{}/api/tags", ollama_url_pre);
        if let Ok(resp) = reqwest::Client::new().get(&tags_url_pre).timeout(std::time::Duration::from_secs(5)).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(models) = body["models"].as_array() {
                        for m in models {
                            if let Some(name) = m["name"].as_str() {
                                available_models_pre.push(format!("ollama:{}", name));
                            }
                        }
                    }
                }
            }
        }
    }
    let configured_pre = format!("{}:{}", config.cortex.llm_provider, config.cortex.model_id);
    if !available_models_pre.iter().any(|m| m == &configured_pre) {
        available_models_pre.push(configured_pre);
    }
    for id in engine_registry.named_model_ids() {
        if !available_models_pre.iter().any(|m| m == &id) {
            available_models_pre.push(id);
        }
    }

    // Build CapabilityRegistry: static profiles + optional Ollama probe
    let ollama_base_for_registry = if config.cortex.llm_provider.to_lowercase() == "ollama" || config.cortex.openai_base_url.contains("11434") {
        Some(if config.cortex.openai_base_url.is_empty() {
            "http://localhost:11434".to_string()
        } else {
            config.cortex.openai_base_url.trim_end_matches('/').to_string()
        })
    } else {
        None
    };
    let capability_registry = std::sync::Arc::new(
        animus_cortex::capability_registry::CapabilityRegistry::build(
            ollama_base_for_registry.as_deref(),
            &[],
            &available_models_pre,
        ).await
    );

    let smart_router: Option<SmartRouter> = {
        let plan_path = data_dir.join("model_plan.json");

        // Discover available models for config hash computation
        let mut available_models: Vec<String> = Vec::new();

        // Ollama: query /api/tags for local model list
        if config.cortex.llm_provider.to_lowercase() == "ollama" || config.cortex.openai_base_url.contains("11434") {
            let ollama_url = if config.cortex.openai_base_url.is_empty() {
                "http://localhost:11434"
            } else {
                config.cortex.openai_base_url.trim_end_matches('/')
            };
            let tags_url = format!("{}/api/tags", ollama_url);
            match reqwest::Client::new().get(&tags_url).timeout(std::time::Duration::from_secs(5)).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        if let Some(models) = body["models"].as_array() {
                            for m in models {
                                if let Some(name) = m["name"].as_str() {
                                    available_models.push(format!("ollama:{}", name));
                                }
                            }
                        }
                    }
                }
                _ => tracing::debug!("Could not query Ollama model list — using config model only"),
            }
        }

        // Always include the explicitly configured model
        let configured = format!("{}:{}", config.cortex.llm_provider, config.cortex.model_id);
        if !available_models.iter().any(|m| m == &configured) {
            available_models.push(configured);
        }

        // Include any per-role named engines (e.g. Cerebras via ANIMUS_PERCEPTION_*)
        for id in engine_registry.named_model_ids() {
            if !available_models.iter().any(|m| m == &id) {
                available_models.push(id);
            }
        }

        let config_hash = ModelPlan::config_hash_for(&available_models);

        // Try to load existing plan
        let existing = ModelPlan::load(&plan_path)
            .filter(|p| p.config_hash == config_hash);

        let plan = if let Some(p) = existing {
            tracing::info!("Model plan loaded from cache (hash: {}...)", &config_hash[..8]);
            p
        } else {
            tracing::info!(
                "Building new model plan from {} available models (capability scoring)",
                available_models.len()
            );

            let p = build_model_plan_from_capabilities(
                &available_models,
                &capability_registry,
            ).await;

            if let Err(e) = p.save(&plan_path) {
                tracing::warn!("Could not persist model plan: {e}");
            } else {
                tracing::info!("Model plan built and saved (reason: {})", p.build_reason);
            }
            p
        };

        let plan_arc = std::sync::Arc::new(tokio::sync::RwLock::new(plan));
        Some(SmartRouter::new(plan_arc, signal_tx.clone(), capability_registry.clone()))
    };

    // Register rate limit state for all engines with SmartRouter.
    // rate_limit_state() returns Some(_) only for AnthropicEngine; all others return None and are skipped.
    // INVARIANT: engine.model_name() must match the ModelSpec.model string stored in the plan for
    // the rate limit lookup in SmartRouter::select_for_class() to find the registered state.
    // LLM-built plans use the short model name (e.g. "claude-opus-4-6"); the default rule-based
    // plan uses the full provider-prefixed string (e.g. "anthropic:claude-opus-4-6"). If a mismatch
    // occurs, rate-limit routing silently falls back to the primary — no data loss, just no rerouting.
    if let Some(ref router) = smart_router {
        let all_engines: &[&dyn animus_cortex::ReasoningEngine] = &[
            engine_registry.fallback(),
            engine_registry.engine_for(CognitiveRole::Perception),
            engine_registry.engine_for(CognitiveRole::Reflection),
            engine_registry.engine_for(CognitiveRole::Reasoning),
        ];
        let mut registered = 0usize;
        for engine in all_engines {
            if let Some(rl_state) = engine.rate_limit_state() {
                router.register_rate_limit_state(engine.model_name(), rl_state);
                registered += 1;
            }
        }
        tracing::info!("SmartRouter: registered rate limit state for {registered} engine(s)");
    }

    if let Some(ref router) = smart_router {
        let plan = router.plan();
        let plan = plan.try_read().expect("plan readable at startup");
        tracing::info!(
            "Smart router active: {} task classes, build reason: {}",
            plan.task_classes.len(),
            plan.build_reason
        );
    }

    // Extend SmartRouter rate limit registration to named engines (Cerebras, OpenRouter, NIM).
    // Named engines now implement rate_limit_state() via OpenAICompatEngine.
    if let Some(ref router) = smart_router {
        let mut registered = 0usize;
        for (_key, engine) in engine_registry.iter_named() {
            if let Some(rl_state) = engine.rate_limit_state() {
                router.register_rate_limit_state(engine.model_name(), rl_state);
                registered += 1;
            }
        }
        if registered > 0 {
            tracing::info!("SmartRouter: registered rate limit state for {registered} named engine(s)");
        }
    }

    // How often the ModelHealthWatcher probes registered engine endpoints (MINOR-4: named constant).
    const MODEL_HEALTH_PROBE_INTERVAL_SECS: u64 = 120;

    // Start ModelHealthWatcher — probes named engine endpoints on the above interval.
    // CRITICAL-4: endpoints stored in a shared Arc<Mutex<Vec>> so hot-loaded engines can be
    // added to the probe list at runtime without restarting the watcher.
    let health_endpoints: Arc<parking_lot::Mutex<Vec<(String, String)>>> = {
        let initial: Vec<(String, String)> = engine_registry.iter_named()
            .filter_map(|(key, engine)| {
                engine.probe_url().map(|url| (key.to_string(), url.to_string()))
            })
            .collect();
        Arc::new(parking_lot::Mutex::new(initial))
    };

    // Retain a sender for the probe trigger channel so the hot-add path can fire immediate
    // probes for engines registered after the watcher is spawned.
    let mut hot_add_probe_tx: Option<tokio::sync::mpsc::Sender<Vec<String>>> = None;

    if let Some(ref router) = smart_router {
        let (probe_trigger_tx, probe_trigger_rx) = tokio::sync::mpsc::channel::<Vec<String>>(32);
        hot_add_probe_tx = Some(probe_trigger_tx.clone()); // retain for hot-add probes
        router.set_probe_trigger_tx(probe_trigger_tx);
        let watcher_router = router.clone();
        let watcher_signal_tx = signal_tx.clone();
        let watcher_source = animus_core::identity::ThreadId::new();
        let watcher_endpoints = health_endpoints.clone();
        let n = watcher_endpoints.lock().len();
        tokio::spawn(async move {
            animus_cortex::watchers::run_model_health_watcher(
                watcher_endpoints,
                watcher_router,
                watcher_signal_tx,
                watcher_source,
                MODEL_HEALTH_PROBE_INTERVAL_SECS,
                probe_trigger_rx,
            ).await;
        });
        tracing::info!("ModelHealthWatcher spawned ({n} endpoint(s), T=0 probe active)");
    }

    // Log initial capability tier (conservative default until first probe cycle)
    {
        let state = capability_state.read();
        tracing::info!(
            "Initial cognitive tier: {} (will update after first probe cycle)",
            state.tier.label()
        );
    }

    // ── Task Manager ──────────────────────────────────────────────────────────────
    let task_manager = TaskManager::new(
        signal_tx.clone(),
        data_dir.clone(),
        5, // max concurrent tasks
    );
    tracing::info!("Task manager initialized");

    // Register tools
    let tool_registry = {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(animus_cortex::tools::read_file::ReadFileTool));
        reg.register(Box::new(animus_cortex::tools::write_file::WriteFileTool));
        reg.register(Box::new(animus_cortex::tools::shell_exec::ShellExecTool));
        reg.register(Box::new(animus_cortex::tools::remember::RememberTool));
        reg.register(Box::new(animus_cortex::tools::list_segments::ListSegmentsTool));
        reg.register(Box::new(animus_cortex::tools::send_signal::SendSignalTool));
        reg.register(Box::new(animus_cortex::tools::update_segment::UpdateSegmentTool));
        // Channel and web tools
        reg.register(Box::new(animus_cortex::tools::http_fetch::HttpFetchTool));
        reg.register(Box::new(animus_cortex::tools::analyze_image::AnalyzeImageTool));
        reg.register(Box::new(animus_cortex::tools::set_autonomy::SetAutonomyTool));
        reg.register(Box::new(animus_cortex::tools::telegram_send::TelegramSendTool));
        reg.register(Box::new(animus_cortex::tools::manage_watcher::ManageWatcherTool));
        reg.register(Box::new(animus_cortex::tools::spawn_task::SpawnTaskTool));
        reg.register(Box::new(animus_cortex::tools::task_status::TaskStatusTool));
        reg.register(Box::new(animus_cortex::tools::task_output::TaskOutputTool));
        reg.register(Box::new(animus_cortex::tools::task_cancel::TaskCancelTool));
        // NATS tools
        reg.register(Box::new(animus_cortex::tools::nats_publish::NatsPublishTool));
        reg.register(Box::new(animus_cortex::tools::claude_instances::ClaudeInstancesTool));
        reg.register(Box::new(animus_cortex::tools::federate_segment::FederateSegmentTool));
        // Memory protection tools
        reg.register(Box::new(animus_cortex::tools::delete_segment::DeleteSegmentTool));
        reg.register(Box::new(animus_cortex::tools::prune_segments::PruneSegmentsTool));
        reg.register(Box::new(animus_cortex::tools::snapshot_memory::SnapshotMemoryTool));
        reg.register(Box::new(animus_cortex::tools::list_snapshots::ListSnapshotsTool));
        reg.register(Box::new(animus_cortex::tools::restore_snapshot::RestoreSnapshotTool));
        // Provider registration tool
        reg.register(Box::new(animus_cortex::tools::register_provider::RegisterProviderTool));
        // Introspective tools — AILF reasoning thread reaches into the Cortex substrate
        reg.register(Box::new(animus_cortex::tools::get_route_stats::GetRouteStatsTool));
        reg.register(Box::new(animus_cortex::tools::propose_route_amendment::ProposeRouteAmendmentTool));
        reg.register(Box::new(animus_cortex::tools::get_classification_patterns::GetClassificationPatternsTool));
        reg.register(Box::new(animus_cortex::tools::update_classification_pattern::UpdateClassificationPatternTool));
        reg.register(Box::new(animus_cortex::tools::get_capability_state::GetCapabilityStateTool));
        reg.register(Box::new(animus_cortex::tools::get_mesh_roles::GetMeshRolesTool));
        reg
    };
    let tool_definitions = tool_registry.definitions();

    // Autonomy mode watch channel — set_autonomy tool sends here, main loop reads.
    let (autonomy_tx, mut autonomy_rx) = tokio::sync::watch::channel(config.autonomy.default_mode);
    // Shared active Telegram chat ID — updated before each channel reasoning call.
    let active_telegram_chat_id: Arc<parking_lot::Mutex<Option<i64>>> =
        Arc::new(parking_lot::Mutex::new(None));

    // Self-event filter — prevents perception feedback loops from the AILF's own tool actions.
    // Tools register paths they modify; perception skips events for those paths.
    let self_event_filter = Arc::new(animus_cortex::SelfEventFilter::new(
        std::time::Duration::from_secs(5),
    ));

    // API usage tracker — tracks call patterns for self-awareness and loop detection.
    let api_tracker = Arc::new(animus_core::ApiTracker::new(
        std::time::Duration::from_secs(60),
        2.0, // 2 calls/sec threshold
    ));

    // Pre-connect a NATS client for the nats_publish tool (independent of the channel adapter).
    // Both can connect to the same server — NATS supports multiple connections per process.
    let nats_publish_client: Option<async_nats::Client> =
        if config.channels.nats.enabled && !config.channels.nats.url.is_empty() {
            match async_nats::connect(&config.channels.nats.url).await {
                Ok(c) => {
                    tracing::info!("NATS publish client ready for nats_publish tool");
                    Some(c)
                }
                Err(e) => {
                    tracing::warn!("NATS publish client init failed (tool disabled): {e}");
                    None
                }
            }
        } else {
            None
        };

    let tool_ctx = ToolContext {
        data_dir: data_dir.clone(),
        snapshot_dir: snapshot_dir.clone(),
        store: gated_store.clone(),
        embedder: embedder.clone(),
        signal_tx: Some(signal_tx.clone()),
        autonomy_tx: Some(autonomy_tx),
        active_telegram_chat_id: active_telegram_chat_id.clone(),
        watcher_registry: Some(watcher_registry.clone()),
        task_manager: Some(task_manager.clone()),
        self_event_filter: Some(self_event_filter.clone()),
        api_tracker: Some(api_tracker.clone()),
        nats_client: nats_publish_client.clone(),
        federation_tx: Some(federation_broadcast_tx.clone()),
        smart_router: smart_router.clone(),
        capability_state: Some(capability_state.clone()),
        role_mesh: Some(role_mesh.clone()),
        budget_state: Some(budget_state.clone()),
        budget_config: Some(config.budget.clone()),
    };
    tracing::info!("{} tools registered", tool_definitions.len());

    // Auto-snapshot background task
    if config.snapshot.interval_secs > 0 {
        let store_snap = store.clone();
        let snap_dir = snapshot_dir.clone();
        let interval_secs = config.snapshot.interval_secs;
        let max_snaps = config.snapshot.max_snapshots;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            ticker.tick().await; // skip immediate first tick
            loop {
                ticker.tick().await;
                let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                let snap_path = snap_dir.join(format!("auto-{ts}"));
                match store_snap.snapshot(&snap_path) {
                    Ok(n) => {
                        tracing::info!("Auto-snapshot: {n} segments at {}", snap_path.display());
                        prune_old_snapshots(&snap_dir, max_snaps);
                    }
                    Err(e) => tracing::warn!("Auto-snapshot failed: {e}"),
                }
            }
        });
    }

    // Initialize quality tracker
    let quality_path = data_dir.join("quality.bin");
    let quality_tracker = Arc::new(parking_lot::Mutex::new(
        animus_mnemos::quality::QualityTracker::load(&quality_path)?,
    ));

    // Initialize goal manager
    let goals_path = data_dir.join("goals.bin");
    let goals = GoalManager::load(&goals_path)?;

    // Compute initial goal embeddings for Tier 2 attention
    update_goal_embeddings(&goals, &*embedder, &orchestrator).await;

    // Wrap goals in Arc<Mutex<>> for sharing with ReflectionLoop
    let goals = Arc::new(parking_lot::Mutex::new(goals));

    // Initialize thread scheduler
    let token_budget = 8000;
    let mut scheduler = ThreadScheduler::new(store.clone(), token_budget);
    let _main_thread_id = scheduler.create_thread("main".to_string());

    // Initialize federation
    let federation_config = config.federation.clone();
    let federation: Option<Arc<FederationOrchestrator<MmapVectorStore>>> =
        if federation_config.enabled {
            let mut orch = FederationOrchestrator::new(
                identity.clone(),
                federation_config,
                store.clone(),
                &data_dir,
            );
            orch.start().await?;
            tracing::info!("Federation started");
            Some(Arc::new(orch))
        } else {
            tracing::info!("Federation disabled; enable in config.toml or set ANIMUS_FEDERATION=1");
            None
        };

    // Federation broadcast background task — receives SegmentId from tools and broadcasts
    {
        let fed_arc = federation.clone();
        let broadcast_store = store.clone();
        tokio::spawn(async move {
            while let Some(segment_id) = federation_broadcast_rx.recv().await {
                let Some(ref orch) = fed_arc else { continue };

                let segment = match broadcast_store.get_raw(segment_id) {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        tracing::warn!("Federation broadcast: segment {segment_id} not found");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("Federation broadcast: store error for {segment_id}: {e}");
                        continue;
                    }
                };

                orch.broadcast_segment(&segment).await;
            }
        });
    }

    // Start health endpoint
    if config.health.enabled {
        start_health_server(
            config.health.bind.clone(),
            store.clone(),
            format!("{}", identity.instance_id),
        );
    }

    // Initialize voice service (STT via macos-stt HTTP + TTS via Cartesia)
    let voice_service: Option<Arc<dyn VoiceService>> = if config.voice.enabled {
        match AnimusVoiceService::new(&config.voice) {
            Ok(svc) => {
                tracing::info!(
                    tts_enabled = config.voice.tts_enabled,
                    stt_url = %config.voice.stt_url,
                    "Voice service initialized (macos-stt + Cartesia)"
                );
                Some(Arc::new(svc) as Arc<dyn VoiceService>)
            }
            Err(e) => {
                tracing::warn!("Failed to initialize voice service: {e}");
                None
            }
        }
    } else {
        tracing::info!("Voice service disabled (set ANIMUS_VOICE_ENABLED=1 to enable)");
        None
    };
    let tts_enabled = voice_service.is_some() && config.voice.tts_enabled;
    // Runtime voice toggle — allows /voice on|off without restart.
    // Persisted to {data_dir}/voice.state so the setting survives restarts.
    let voice_state_path = data_dir.join("voice.state");
    let voice_state_default = voice_service.is_some();
    let voice_state_initial = if voice_state_path.exists() {
        std::fs::read_to_string(&voice_state_path)
            .ok()
            .and_then(|s| s.trim().parse::<bool>().ok())
            .unwrap_or(voice_state_default)
    } else {
        voice_state_default
    };
    let voice_active = Arc::new(std::sync::atomic::AtomicBool::new(voice_state_initial));

    // Initialize ChannelBus and channel adapters
    let channel_bus = ChannelBus::new(256);
    let injection_scanner = Arc::new(InjectionScanner::new(config.security.injection_threshold));
    let message_router = MessageRouter::new(injection_scanner);

    // Register Telegram adapter if configured
    if config.channels.telegram.enabled && !config.channels.telegram.bot_token.is_empty() {
        match TelegramChannel::new(
            config.channels.telegram.clone(),
            config.security.trusted_telegram_ids.clone(),
        ) {
            Ok(adapter) => {
                channel_bus.register(Arc::new(adapter)).await;
                tracing::info!("Telegram channel adapter registered");
            }
            Err(e) => tracing::warn!("Failed to initialize Telegram adapter: {e}"),
        }
    } else {
        tracing::info!("Telegram channel disabled (set ANIMUS_TELEGRAM_TOKEN to enable)");
    }

    // Register NATS adapter if configured
    if config.channels.nats.enabled {
        match NatsChannel::connect(config.channels.nats.clone()).await {
            Ok(adapter) => {
                // Exclude the permission_request subject — PermissionGate owns it
                let adapter = adapter.with_excluded_subjects(vec![
                    animus_channel::permission_gate::PERMISSION_REQUEST_SUBJECT.to_string(),
                ]);
                channel_bus.register(Arc::new(adapter)).await;
                tracing::info!("NATS channel adapter registered");
            }
            Err(e) => tracing::warn!("Failed to initialize NATS adapter: {e}"),
        }
    } else {
        tracing::info!("NATS channel disabled (set channels.nats.enabled = true to enable)");
    }

    // Start all registered channel adapters
    if let Err(e) = channel_bus.start_all().await {
        tracing::warn!("ChannelBus start error: {e}");
    }

    // Start PermissionGate if NATS is available
    // Uses the nats_publish_client (already connected) — avoids a second connection.
    if let Some(ref nats_client) = nats_publish_client {
        let tg_client = if config.channels.telegram.enabled
            && !config.channels.telegram.bot_token.is_empty()
        {
            match animus_channel::telegram::api::TelegramClient::new(
                &config.channels.telegram.bot_token,
            ) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!("PermissionGate: could not create Telegram client: {e}");
                    None
                }
            }
        } else {
            None
        };

        let trusted_chat_id = config.security.trusted_telegram_ids.first().copied();

        let gate = Arc::new(PermissionGate::new(
            nats_client.clone(),
            tg_client,
            trusted_chat_id,
            autonomy_rx.clone(),
        ));
        gate.start(channel_bus.clone()).await;
        tracing::info!("PermissionGate started (autonomy: {})", config.autonomy.default_mode);
    } else {
        tracing::info!("PermissionGate disabled (NATS not available)");
    }

    // Subscribe to inbound channel messages
    let mut channel_rx = channel_bus.subscribe();

    // Bootstrap self-knowledge into VectorFS on first run or version change
    {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".to_string());
        let telegram_configured = config.channels.telegram.enabled
            || !config.channels.telegram.bot_token.is_empty();
        let trusted_ids = config.security.trusted_telegram_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        bootstrap::run_if_needed(
            &data_dir,
            &model_id,
            &hostname,
            &store,
            &*embedder,
            telegram_configured,
            &trusted_ids,
        ).await;
    }

    // Boot reconstitution — wake up with context from last session
    let reconstitution_summary = {
        let recon_engine: &dyn animus_cortex::ReasoningEngine = engine_registry.engine_for(CognitiveRole::Reflection);
        let goals_snapshot = goals.lock().clone();
        match animus_cortex::boot_reconstitution(
            recon_engine,
            &*store,
            &*embedder,
            &identity,
            &goals_snapshot,
        ).await {
            Ok(Some(summary)) => {
                tracing::info!("Reconstitution complete");
                Some(summary)
            }
            Ok(None) => {
                tracing::info!("No reconstitution context available");
                None
            }
            Err(e) => {
                tracing::warn!("Reconstitution failed: {e}");
                None
            }
        }
    };

    // Start Perception loop (replaces mechanical event processing)
    let perception_signal_tx = signal_tx.clone();
    let perception_store = store.clone();
    let perception_embedder = embedder.clone();
    let perception_event_rx = event_bus.subscribe();
    let perception_self_filter = self_event_filter.clone();
    let perception_api_tracker = api_tracker.clone();
    let perception_engine: Box<dyn animus_cortex::ReasoningEngine> = {
        let pm = std::env::var("ANIMUS_PERCEPTION_MODEL")
            .unwrap_or_else(|_| model_id.clone());
        match AnthropicEngine::from_best_available(&pm, 1024) {
            Ok(e) => Box::new(e),
            Err(_) => {
                tracing::info!("No perception model configured, using mechanical event pipeline");
                Box::new(animus_cortex::MockEngine::new("no perception model"))
            }
        }
    };
    tokio::spawn(async move {
        let perception = animus_cortex::PerceptionLoop::new(
            perception_engine,
            perception_store,
            perception_embedder,
            perception_signal_tx,
        )
        .with_self_filter(perception_self_filter)
        .with_api_tracker(perception_api_tracker);
        perception.run(perception_event_rx).await;
    });
    tracing::info!("Perception loop started");

    // Start periodic tier management (every 10 minutes)
    let tier_store = store.clone();
    tokio::spawn(async move {
        let tier_manager = animus_vectorfs::tier_manager::TierManager::new(
            tier_store,
            animus_core::tier::TierConfig::default(),
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            tier_manager.run_cycle();
        }
    });

    // Start periodic Mnemos consolidation (every 60 minutes)
    // Merges near-duplicate warm-tier segments to reduce memory density.
    let consolidation_store = store.clone();
    tokio::spawn(async move {
        let consolidator = animus_mnemos::Consolidator::new(consolidation_store, 0.85);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            match consolidator.run_cycle() {
                Ok(report) if report.segments_merged > 0 => {
                    tracing::info!(
                        "Mnemos consolidation: scanned={} merged={} created={}",
                        report.segments_scanned,
                        report.segments_merged,
                        report.segments_created,
                    );
                }
                Ok(_) => {} // nothing to consolidate this cycle
                Err(e) => tracing::warn!("Mnemos consolidation error: {e}"),
            }
        }
    });
    tracing::info!("Mnemos periodic consolidation started (60 min interval, similarity ≥ 0.85)");

    // Start Reflection loop (replaces standalone consolidation)
    let reflection_signal_tx = signal_tx.clone();
    let reflection_store = store.clone();
    let reflection_embedder = embedder.clone();
    let reflection_goals = goals.clone();
    let reflection_engine: Box<dyn animus_cortex::ReasoningEngine> = {
        let rm = std::env::var("ANIMUS_REFLECTION_MODEL")
            .unwrap_or_else(|_| model_id.clone());
        match AnthropicEngine::from_best_available(&rm, 4096) {
            Ok(e) => Box::new(e),
            Err(_) => {
                tracing::info!("No reflection model configured, reflection loop disabled");
                Box::new(animus_cortex::MockEngine::new("no reflection model"))
            }
        }
    };
    tokio::spawn(async move {
        let reflection = animus_cortex::ReflectionLoop::new(
            reflection_engine,
            reflection_store,
            reflection_embedder,
            reflection_goals,
            reflection_signal_tx,
        );
        reflection.run().await;
    });
    tracing::info!("Reflection loop started");

    // Start periodic health sweep (every 15 minutes)
    // Recomputes health scores and logs segments with severely degraded confidence.
    let health_store = store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(900)).await;
            let all_ids = health_store.segment_ids(None);
            let mut degraded = 0usize;
            for id in &all_ids {
                if let Ok(Some(seg)) = health_store.get_raw(*id) {
                    let health = seg.health_score();
                    if health < 0.1 && seg.tier != animus_core::Tier::Cold {
                        tracing::debug!(
                            "segment {} health={health:.3} — degraded",
                            id.0.to_string().get(..8).unwrap_or("?")
                        );
                        degraded += 1;
                    }
                }
            }
            if degraded > 0 {
                tracing::info!("Health sweep: {degraded}/{} segments degraded", all_ids.len());
            }
        }
    });

    // Goal deadline watcher — checks every 60 s for approaching/overdue goals.
    // Sends a proactive message when a deadline is ≤ 1 hour away or just passed.
    {
        let deadline_goals = goals.clone();
        let deadline_tx = proactive_tx.clone();
        // Track which goals we've already alerted at each threshold to avoid spam.
        tokio::spawn(async move {
            let mut alerted: std::collections::HashSet<(animus_core::identity::GoalId, &'static str)> =
                std::collections::HashSet::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let now = chrono::Utc::now();
                // Collect messages under the lock, then send after releasing it
                // to avoid holding parking_lot lock across await.
                let pending: Vec<(animus_core::identity::GoalId, &'static str, String)> = {
                    let goals_guard = deadline_goals.lock();
                    goals_guard.active_goals().into_iter().filter_map(|goal| {
                        let deadline = goal.deadline?;
                        let remaining = deadline - now;
                        let mins = remaining.num_minutes();
                        let threshold: &'static str = if mins <= 0 {
                            "overdue"
                        } else if mins <= 15 {
                            "15min"
                        } else if mins <= 60 {
                            "1hr"
                        } else {
                            return None;
                        };
                        if alerted.contains(&(goal.id, threshold)) {
                            return None;
                        }
                        let text = if mins <= 0 {
                            format!("Goal overdue: {}", goal.description)
                        } else {
                            format!("Goal deadline in {} min: {}", mins, goal.description)
                        };
                        Some((goal.id, threshold, text))
                    }).collect()
                };
                for (id, threshold, text) in pending {
                    alerted.insert((id, threshold));
                    let _ = deadline_tx.send(ProactiveMessage { text, source: "goal_deadline" }).await;
                }
            }
        });
    }

    // Initialize terminal interface
    let interface = TerminalInterface::new(">> ".to_string());
    let instance_str = format!("{}", identity.instance_id);
    interface.display_banner(instance_str.get(..8).unwrap_or(&instance_str), engine_registry.engine_for(CognitiveRole::Reasoning).model_name(), segment_count);
    if let Some(thread) = scheduler.active_thread() {
        interface.display_status(&format!("Active thread: {}", thread.name));
    }

    // Sleep/wake state
    let mut is_sleeping = false;
    let mut sleep_started: Option<chrono::DateTime<chrono::Utc>> = None;

    // Autonomy mode (runtime-configurable)
    let mut autonomy_mode = config.autonomy.default_mode;
    tracing::info!("Autonomy mode: {autonomy_mode}");

    // Map from channel thread key → reasoning ThreadId (for multi-channel routing)
    let mut channel_thread_map: HashMap<String, animus_core::identity::ThreadId> = HashMap::new();

    // Situational awareness — tracks all active conversations for peripheral awareness.
    // 24h recency window; not persisted across restarts (VectorFS has the history).
    let mut situational_awareness = animus_cortex::SituationalAwareness::new(24);

    // Track whether stdin is still open (false in container/daemon mode — arm is parked)
    let mut stdin_open = true;

    // Non-blocking stdin bridge — reads terminal input in a blocking task,
    // sends to async channel. In container mode (no TTY), this exits immediately.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<String>(32);
    tokio::task::spawn_blocking(move || {
        use std::io::BufRead;
        // Show prompt and read
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() && stdin_tx.blocking_send(trimmed).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        tracing::info!("stdin closed — terminal input disabled");
    });

    // Main event loop — handles terminal input, channel messages, and signals
    loop {
        // Poll signal bridge — deliver signals from background cognitive loops.
        // Urgent signals also become proactive messages when autonomy mode allows.
        while let Ok(signal) = signal_rx.try_recv() {
            // Hot-reload new engines when providers.json gains entries.
            if signal.summary.contains("providers.json: new provider") {
                let entries = animus_core::load_providers_json(&data_dir.join("providers.json"));
                for entry in &entries {
                    use animus_core::provider_meta::OwnershipRisk;
                    if entry.trust.ownership_risk == OwnershipRisk::Prohibited {
                        tracing::warn!(
                            "providers.json: skipping prohibited provider '{}'",
                            entry.provider_id
                        );
                        continue;
                    }
                    for model in &entry.models {
                        if engine_registry
                            .engine_by_spec(&entry.provider_id, &model.model_id)
                            .is_none()
                        {
                            match OpenAICompatEngine::new(
                                &entry.base_url,
                                &entry.api_key,
                                &model.model_id,
                                8192,
                            ) {
                                Ok(eng) => {
                                    let eng_arc = std::sync::Arc::new(eng);
                                    // CRITICAL-4: register with health watcher so probe coverage
                                    // extends to hot-loaded engines without restarting the watcher.
                                    // All hot-loaded engines are OpenAI-compat; probe URL = base_url.
                                    if !entry.base_url.is_empty() {
                                        let new_engine_key = format!("{}:{}", entry.provider_id, model.model_id);
                                        health_endpoints.lock().push((
                                            new_engine_key.clone(),
                                            entry.base_url.clone(),
                                        ));
                                        // Trigger an immediate probe so the new engine's health weight
                                        // is populated without waiting for the next scheduled cycle.
                                        if let Some(ref tx) = hot_add_probe_tx {
                                            let _ = tx.try_send(vec![new_engine_key]);
                                        }
                                    }
                                    engine_registry.add_named(
                                        &entry.provider_id,
                                        &model.model_id,
                                        eng_arc,
                                    );
                                    tracing::info!(
                                        "Hot-loaded new engine: {}:{}",
                                        entry.provider_id,
                                        model.model_id
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "providers.json: failed to build engine for {}:{} — {e}",
                                        entry.provider_id,
                                        model.model_id
                                    );
                                }
                            }
                        }
                    }
                }
            }
            // Adaptation signals (engine fallback / health change) always notify
            // the user regardless of autonomy mode — engine failures are always worth reporting.
            if signal.summary.starts_with("Adapting:") || signal.summary.starts_with("Engine '") {
                // MAJOR-5: warn instead of silently dropping adaptation signals when channel is full.
                if let Err(e) = proactive_tx.try_send(ProactiveMessage {
                    text: signal.summary.trim_start_matches("Adapting: ").to_string(),
                    source: "model_adaptation",
                }) {
                    tracing::warn!("adaptation signal dropped — proactive channel full: {e}");
                }
            } else if signal.priority == animus_core::threading::SignalPriority::Urgent
                && autonomy_mode != animus_core::config::AutonomyMode::Reactive
            {
                if let Err(e) = proactive_tx.try_send(ProactiveMessage {
                    text: signal.summary.clone(),
                    source: "signal",
                }) {
                    tracing::warn!("urgent signal dropped — proactive channel full: {e}");
                }
            }
            if let Some(active) = scheduler.active_thread_mut() {
                active.deliver_signal(signal);
            }
        }

        // Check for autonomy mode changes from set_autonomy tool
        if autonomy_rx.has_changed().unwrap_or(false) {
            autonomy_mode = *autonomy_rx.borrow_and_update();
            tracing::info!("Autonomy mode changed to: {autonomy_mode}");
            interface.display_status(&format!("Autonomy mode: {autonomy_mode}"));
        }

        tokio::select! {
            // ── Terminal input (from stdin bridge) ──────────────────────────
            // When stdin closes (container/daemon mode), park this arm instead
            // of breaking — channel messages must continue to be processed.
            input_opt = async {
                if stdin_open {
                    stdin_rx.recv().await
                } else {
                    std::future::pending::<Option<String>>().await
                }
            } => {
                let input = match input_opt {
                    Some(s) => s,
                    None => {
                        // stdin closed — disable this arm and keep running
                        stdin_open = false;
                        continue;
                    }
                };

                // Handle slash commands
                if input.starts_with('/') {
                    let mut goals_guard = goals.lock();
                    let mut ctx = CommandContext {
                        store: &store,
                        goals: &mut goals_guard,
                        goals_path: &goals_path,
                        interface: &interface,
                        embedder: &*embedder,
                        data_dir: &data_dir,
                        snapshot_dir: &snapshot_dir,
                        scheduler: &mut scheduler,
                        federation: federation.clone(),
                        event_bus: &event_bus,
                        file_watcher: &file_watcher,
                        sensorium: &orchestrator,
                        is_sleeping: &mut is_sleeping,
                        sleep_started: &mut sleep_started,
                        sleeping_flag: &sleeping_flag,
                        watcher_registry: &watcher_registry,
                        task_manager: &task_manager,
                        api_tracker: &api_tracker,
                        voice_active: &voice_active,
                        voice_configured: voice_service.is_some(),
                        voice_state_path: &voice_state_path,
                    };
                    match handle_command(&input, &mut ctx).await? {
                        CommandResult::Continue => continue,
                        CommandResult::Quit => break,
                    }
                }

                if is_sleeping {
                    interface.display_status("Sleeping. Use /wake to resume.");
                    continue;
                }

                // Process through main reasoning thread
                interface.display(&format!(">> {input}"));
                let response = run_reasoning_turn(
                    &input,
                    None,
                    &mut scheduler,
                    &engine_registry,
                    &tool_registry,
                    &tool_ctx,
                    &*embedder,
                    &tool_definitions,
                    &goals,
                    reconstitution_summary.as_deref(),
                    None, // terminal has no peripheral awareness injection
                ).await;

                match response {
                    Ok(text) => interface.display_response(&text),
                    Err(e) => interface.display_status(&format!("Error: {e}")),
                }
            }

            // ── Proactive message from background cognitive loops ────────────
            proactive_msg = proactive_rx.recv() => {
                let Some(pm) = proactive_msg else { continue };
                // Only deliver if autonomy mode permits independent action.
                if autonomy_mode == animus_core::config::AutonomyMode::Reactive {
                    tracing::debug!(source = pm.source, "Proactive message suppressed (Reactive mode)");
                    continue;
                }
                // Resolve primary contact: last active Telegram chat, else first trusted ID.
                let chat_id_opt = active_telegram_chat_id.lock().as_ref().copied()
                    .or_else(|| config.security.trusted_telegram_ids.first().copied());
                let Some(chat_id) = chat_id_opt else {
                    tracing::debug!(source = pm.source, "Proactive message dropped (no known chat_id)");
                    continue;
                };
                let chat_str = chat_id.to_string();
                let outbound = OutboundMessage::text("telegram", &chat_str, pm.text);
                if let Err(e) = channel_bus.send(outbound).await {
                    tracing::warn!(source = pm.source, "Failed to send proactive message: {e}");
                } else {
                    tracing::info!(source = pm.source, chat_id, "Proactive message sent");
                }
            }

            // ── Channel bus message (Telegram, HTTP API, etc.) ──────────────
            channel_msg = channel_rx.recv() => {
                let msg = match channel_msg {
                    Ok(m) => m,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("ChannelBus: dropped {n} messages (lagged)");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };

                // Triage: injection scan + priority scoring
                let (msg, decision) = message_router.route(msg).await;

                match decision {
                    RouteDecision::InjectionBlocked(alert) => {
                        tracing::warn!(
                            "Prompt injection blocked from {} via {} (confidence={:.2})",
                            alert.sender_name, alert.channel_id, alert.confidence
                        );
                        // Notify user through the same channel
                        let warn_text = format!(
                            "⚠️ Blocked: potential prompt injection detected in content from {}. \
                            I've logged it and won't process that content.",
                            alert.sender_name
                        );
                        let outbound = OutboundMessage::text(
                            &alert.channel_id,
                            &alert.thread_id,
                            warn_text,
                        );
                        if let Err(e) = channel_bus.send(outbound).await {
                            tracing::warn!("Failed to send injection alert response: {e}");
                        }
                    }

                    RouteDecision::ExistingThread(ref raw_thread_key)
                    | RouteDecision::NewThread(ref raw_thread_key) => {
                        // Resolve to principal ID if known; fall back to raw channel key.
                        let thread_key_owned = resolve_principal(&msg, &config.channels.principals)
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| raw_thread_key.clone());
                        let thread_key = &thread_key_owned;

                        // Get or create a reasoning thread for this conversation
                        let thread_id = match channel_thread_map.get(thread_key) {
                            Some(&id) => {
                                if let Err(e) = scheduler.switch_to(id) {
                                    tracing::debug!("Could not switch to thread {id}: {e}; using active");
                                }
                                id
                            }
                            None => {
                                let id = scheduler.create_thread(thread_key.clone());
                                channel_thread_map.insert(thread_key.clone(), id);
                                tracing::info!("Created reasoning thread '{}' for channel conversation", thread_key);
                                id
                            }
                        };
                        let _ = thread_id; // thread is now active via scheduler

                        // Update active Telegram chat ID for telegram_send tool
                        if let Ok(chat_id) = msg.thread_id.parse::<i64>() {
                            *active_telegram_chat_id.lock() = Some(chat_id);
                        }

                        let is_voice_msg = msg.metadata["is_voice"].as_bool().unwrap_or(false);
                        let voice_on = voice_active.load(std::sync::atomic::Ordering::Relaxed);

                        // Pre-process voice: transcribe audio attachment to text
                        let voice_transcript: Option<String> = if is_voice_msg && voice_on {
                            if let Some(svc) = &voice_service {
                                if let Some(audio_path) = msg.attachments.first() {
                                    match svc.transcribe(audio_path).await {
                                        Ok(t) => {
                                            tracing::info!(
                                                chars = t.len(),
                                                "Voice message transcribed"
                                            );
                                            Some(t)
                                        }
                                        Err(e) => {
                                            tracing::warn!("Voice transcription failed: {e}");
                                            None
                                        }
                                    }
                                } else {
                                    None
                                }
                            } else {
                                tracing::warn!("Voice message received but voice service not configured");
                                None
                            }
                        } else {
                            None
                        };

                        // Pre-process images: analyze them and prepend description to text
                        let image_descriptions = if !msg.images.is_empty() {
                            let mut descs = Vec::new();
                            for path in &msg.images {
                                let params = serde_json::json!({"image_path": path.to_string_lossy()});
                                match animus_cortex::tools::analyze_image::AnalyzeImageTool
                                    .execute(params, &tool_ctx)
                                    .await
                                {
                                    Ok(result) => descs.push(format!("[Image: {}]", result.content)),
                                    Err(e) => descs.push(format!("[Image analysis failed: {e}]")),
                                }
                            }
                            descs.join("\n")
                        } else {
                            String::new()
                        };

                        // If a voice message arrived but can't be transcribed, tell the user.
                        if is_voice_msg && voice_transcript.is_none() {
                            let body = if !voice_on {
                                "Voice is currently off — send /voice on to re-enable it, \
                                or send a text message."
                            } else {
                                "I received your voice message but can't transcribe it — \
                                voice transcription isn't configured on this instance. \
                                Please send text instead."
                            };
                            let mut warn = OutboundMessage::text(
                                &msg.channel_id,
                                &msg.thread_id,
                                body,
                            );
                            if let Some(id) = msg.metadata["telegram_message_id"].as_i64() {
                                warn.metadata = serde_json::json!({"telegram_message_id": id});
                            }
                            if let Err(e) = channel_bus.send(warn).await {
                                tracing::warn!("Failed to send voice-not-configured reply: {e}");
                            }
                            continue;
                        }

                        // Build full input text
                        let input_text = {
                            let mut parts = Vec::new();
                            if !image_descriptions.is_empty() {
                                parts.push(image_descriptions);
                            }
                            if let Some(transcript) = &voice_transcript {
                                // Voice context hint: tell the LLM to respond for spoken delivery.
                                // Sent as part of the user message so the LLM sees it inline.
                                parts.push(format!(
                                    "[Voice message — respond in natural spoken language: \
                                    be concise and conversational, no markdown, no bullet lists, \
                                    no tables, no code blocks]\n{transcript}"
                                ));
                            } else if let Some(text) = &msg.text {
                                parts.push(text.clone());
                            }
                            parts.join("\n\n")
                        };

                        if input_text.is_empty() {
                            tracing::debug!("Channel message with no processable content, skipping");
                            continue;
                        }

                        let channel_id = msg.channel_id.clone();
                        let thread_id_str = msg.thread_id.clone();
                        let reply_to = msg.metadata["telegram_message_id"].as_i64();
                        let nats_reply_to = msg.metadata["nats_reply_to"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());

                        // Update situational awareness before reasoning.
                        situational_awareness.set_active(
                            thread_key,
                            &channel_id,
                            &input_text[..input_text.len().min(80)],
                        );
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
                            awareness_block.as_deref(),
                        ).await;

                        let response_text = match response {
                            Ok(text) => text,
                            Err(e) => format!("Sorry, I encountered an error: {e}"),
                        };

                        // Send text response
                        let mut outbound = OutboundMessage::text(
                            &channel_id,
                            &thread_id_str,
                            response_text.clone(),
                        );
                        if let Some(id) = reply_to {
                            outbound.metadata = serde_json::json!({"telegram_message_id": id});
                        } else if let Some(ref inbox) = nats_reply_to {
                            outbound.metadata = serde_json::json!({"nats_reply_to": inbox});
                        }

                        if let Err(e) = channel_bus.send(outbound).await {
                            tracing::warn!("Failed to send channel response: {e}");
                        }

                        // For voice messages: also synthesize and send audio reply
                        if is_voice_msg && tts_enabled && voice_on {
                            if let Some(svc) = &voice_service {
                                match svc.synthesize(&response_text).await {
                                    Ok(audio_path) => {
                                        let mut voice_outbound = OutboundMessage::text(
                                            &channel_id,
                                            &thread_id_str,
                                            String::new(),
                                        );
                                        voice_outbound.audio = Some(audio_path.clone());
                                        if let Some(id) = reply_to {
                                            voice_outbound.metadata = serde_json::json!({"telegram_message_id": id});
                                        }
                                        if let Err(e) = channel_bus.send(voice_outbound).await {
                                            tracing::warn!("Failed to send voice response: {e}");
                                        }
                                        // Note: temp TTS file cleanup is handled by TelegramChannel.send()
                                        // after the file has been fully read and uploaded.
                                    }
                                    Err(e) => tracing::warn!("TTS synthesis failed: {e}"),
                                }
                            }
                        }
                        // Set idle after response sent (runs even on error path above).
                        situational_awareness.set_idle(thread_key);

                        // ── Auto-persist channel exchange to VectorFS ────────
                        // Stores a compact record of every channel turn so Animus
                        // can recall cross-channel conversations (e.g. NATS↔Telegram).
                        {
                            let record = format!(
                                "[channel:{channel_id} thread:{thread_id_str}]\nIN: {}\nOUT: {}",
                                &input_text[..input_text.len().min(800)],
                                &response_text[..response_text.len().min(800)],
                            );
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
                        }
                    }
                }
            }

            // ── Yield briefly to avoid busy-looping when no input ───────────
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                continue;
            }
        }
    }

    // Graceful shutdown
    interface.display_status("Shutting down...");

    // Stop sensors
    network_monitor.stop();
    process_monitor.stop();
    clipboard_monitor.stop();
    if let Some(fw) = file_watcher.lock().take() {
        fw.stop();
    }

    // Shutdown reflection — store current state for reconstitution
    {
        let recon_engine: &dyn animus_cortex::ReasoningEngine = engine_registry.engine_for(CognitiveRole::Reflection);
        let goals_snapshot = goals.lock().clone();
        if let Err(e) = animus_cortex::shutdown_reflection(
            recon_engine,
            &*store,
            &*embedder,
            &goals_snapshot,
        ).await {
            tracing::warn!("Shutdown reflection failed: {e}");
        } else {
            tracing::info!("Shutdown reflection stored");
        }
    }

    // Persist state
    goals.lock().save(&goals_path)?;
    if let Err(e) = quality_tracker.lock().save(&quality_path) {
        tracing::warn!("Failed to save quality tracker: {e}");
    }
    store.flush()?;
    interface.display_status("Session ended. Memory persisted.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Reasoning turn executor
// ---------------------------------------------------------------------------

/// Estimate USD cost of an LLM call based on model name and token counts.
/// Uses known per-MTok rates; falls back to a conservative estimate for unknown models.
fn estimate_cost_usd(model_name: &str, input_tokens: usize, output_tokens: usize) -> f32 {
    // Rates in USD per million tokens (input, output)
    let (input_rate, output_rate): (f32, f32) = if model_name.contains("claude-opus") {
        (15.0, 75.0)
    } else if model_name.contains("claude-sonnet") {
        (3.0, 15.0)
    } else if model_name.contains("claude-haiku") {
        (0.80, 4.0)
    } else if model_name.contains("llama") || model_name.contains("cerebras") {
        // Free tier — covers Cerebras (llama3.1-8b), Ollama local models, etc.
        (0.0, 0.0)
    } else {
        (1.0, 5.0) // conservative unknown
    };
    (input_tokens as f32 * input_rate / 1_000_000.0)
        + (output_tokens as f32 * output_rate / 1_000_000.0)
}

/// Execute a full reasoning turn (input → tool loop → response) on the active thread.
///
/// Handles up to MAX_TOOL_ROUNDS of tool use, stores the response segment,
/// and returns the final text response. Used by both terminal and channel paths.
#[allow(clippy::too_many_arguments)]
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
    peripheral_awareness: Option<&str>,
) -> animus_core::Result<String> {
    let system = {
        let goals_guard = goals.lock();
        build_system_prompt(scheduler, &goals_guard, tool_registry, reconstitution_summary, peripheral_awareness)
    };

    // Determine routing constraints from sensitivity scan and budget pressure
    let sensitivity_scan = animus_sensorium::scan_content_sensitivity(input);
    let pressure = {
        let budget_thresholds = animus_core::BudgetThresholds {
            monthly_limit_usd: tool_ctx.budget_config.as_ref()
                .map(|c| c.monthly_limit_usd).unwrap_or(50.0),
            careful_threshold: tool_ctx.budget_config.as_ref()
                .map(|c| c.careful_threshold).unwrap_or(0.60),
            emergency_threshold: tool_ctx.budget_config.as_ref()
                .map(|c| c.emergency_threshold).unwrap_or(0.85),
        };
        tool_ctx.budget_state.as_ref()
            .map(|s| s.read().pressure(&budget_thresholds))
            .unwrap_or(animus_core::BudgetPressure::Normal)
    };
    // Build ordered engine candidate list from smart router, then append the Reasoning engine
    // as a last-resort fallback. process_turn_with_engines tries each in order, automatically
    // skipping to the next on rate-limit (429) or transient (503/overloaded) errors.
    let route_decisions: Vec<animus_cortex::smart_router::RouteDecision> =
        if let Some(ref router) = tool_ctx.smart_router {
            router.route_all_candidates(input, pressure, sensitivity_scan.level).await
        } else {
            vec![]
        };
    let primary_model_key: Option<String> = route_decisions.first()
        .map(|d| format!("{}:{}", d.model_spec.provider, d.model_spec.model));
    let candidate_arcs: Vec<Arc<dyn animus_cortex::ReasoningEngine>> = route_decisions.iter()
        .filter_map(|d| engine_registry.engine_by_spec(&d.model_spec.provider, &d.model_spec.model))
        .collect();
    // Collect &dyn refs; candidate_arcs and engine_registry both live for this scope
    let mut engine_refs: Vec<&dyn animus_cortex::ReasoningEngine> =
        candidate_arcs.iter().map(|a| a.as_ref()).collect();
    // Always append the Reasoning engine as the final fallback
    engine_refs.push(engine_registry.engine_for(CognitiveRole::Reasoning));

    // The first (highest-priority) engine is used for tool continuation rounds and budget tracking.
    // Tool rounds are continuations within the same minute window so rate limits are unlikely to fire again.
    let tool_engine: &dyn animus_cortex::ReasoningEngine = *engine_refs.first()
        .unwrap_or(&engine_registry.engine_for(CognitiveRole::Reasoning));

    let tools_slice = if tool_definitions.is_empty() {
        None
    } else {
        Some(tool_definitions)
    };

    const MAX_TOOL_ROUNDS: usize = 10;

    let primary_engine_name = engine_refs.first().map(|e| e.model_name().to_string());

    let mut output = {
        let active = scheduler
            .active_thread_mut()
            .ok_or_else(|| animus_core::AnimusError::Threading("no active thread".to_string()))?;
        active
            .process_turn_with_engines(input, &system, &engine_refs, embedder, tools_slice)
            .await?
    };

    // Notify user when a fallback engine was used (model adaptation).
    // Fires a signal tagged "Adapting:" which the main loop forwards to Telegram
    // regardless of autonomy mode — engine failures are always worth reporting.
    if output.fell_back {
        let primary_name = primary_engine_name.as_deref().unwrap_or("?");
        let summary = format!(
            "Adapting: primary engine '{}' was unavailable — used '{}' instead",
            primary_name, output.engine_used
        );
        tracing::info!("{summary}");
        if let Some(ref tx) = tool_ctx.signal_tx {
            let _ = tx.try_send(animus_core::threading::Signal {
                source_thread: animus_core::identity::ThreadId::default(),
                target_thread: animus_core::identity::ThreadId::default(),
                priority: animus_core::threading::SignalPriority::Normal,
                summary,
                segment_refs: vec![],
                created: chrono::Utc::now(),
            });
        }
        // Mark engine unhealthy immediately (sets weight to 0.0) and trigger re-probe so health
        // state reflects the failure without waiting for the next scheduled probe cycle.
        if let (Some(ref key), Some(ref router)) = (&primary_model_key, &tool_ctx.smart_router) {
            router.mark_engine_unhealthy(key);
        }
    }

    // Tool use loop
    for _round in 0..MAX_TOOL_ROUNDS {
        if output.stop_reason != animus_cortex::StopReason::ToolUse || output.tool_calls.is_empty() {
            break;
        }

        // Build assistant turn with tool_use blocks
        let mut assistant_content: Vec<animus_cortex::TurnContent> = Vec::new();
        if !output.content.is_empty() {
            assistant_content.push(animus_cortex::TurnContent::Text(output.content.clone()));
        }
        for tc in &output.tool_calls {
            assistant_content.push(animus_cortex::TurnContent::ToolUse {
                id: tc.id.clone(),
                name: tc.name.clone(),
                input: tc.input.clone(),
            });
        }
        {
            let active = scheduler.active_thread_mut().unwrap();
            active.push_turn(animus_cortex::Turn {
                role: animus_cortex::Role::Assistant,
                content: assistant_content,
            });
        }

        // Execute each tool call
        let mut tool_results: Vec<animus_cortex::TurnContent> = Vec::new();
        for tc in &output.tool_calls {
            let result = if let Some(tool) = tool_registry.get(&tc.name) {
                tool.execute(tc.input.clone(), tool_ctx)
                    .await
                    .unwrap_or_else(|e| animus_cortex::tools::ToolResult {
                        content: format!("Error: {e}"),
                        is_error: true,
                    })
            } else {
                animus_cortex::tools::ToolResult {
                    content: format!("Unknown tool: {}", tc.name),
                    is_error: true,
                }
            };
            tool_results.push(animus_cortex::TurnContent::ToolResult {
                tool_use_id: tc.id.clone(),
                content: result.content,
                is_error: result.is_error,
            });
        }

        {
            let active = scheduler.active_thread_mut().unwrap();
            active.push_turn(animus_cortex::Turn {
                role: animus_cortex::Role::User,
                content: tool_results,
            });
            output = tool_engine
                .reason(&system, active.conversation(), tools_slice)
                .await
                .unwrap_or_else(|e| {
                    tracing::error!("Engine error during tool loop: {e}");
                    animus_cortex::ReasoningOutput {
                        content: format!("Error during tool execution: {e}"),
                        input_tokens: 0,
                        output_tokens: 0,
                        tool_calls: vec![],
                        stop_reason: animus_cortex::StopReason::EndTurn,
                        engine_used: String::new(),
                        fell_back: false,
                    }
                });
        }
    }

    if output.stop_reason == animus_cortex::StopReason::ToolUse {
        tracing::warn!("Tool use loop exhausted after {MAX_TOOL_ROUNDS} rounds");
        if output.content.is_empty() {
            output.content =
                "[Tool execution limit reached. Please try a simpler request.]".to_string();
        }
    }

    // Store response segment and push assistant turn
    {
        let active = scheduler.active_thread_mut().unwrap();
        active.store_response_segment(&output.content, embedder).await.ok();
        active.push_turn(animus_cortex::Turn::text(
            animus_cortex::Role::Assistant,
            &output.content,
        ));
    }

    // Record spend for budget tracking
    if let Some(ref bs) = tool_ctx.budget_state {
        let cost_usd = estimate_cost_usd(
            tool_engine.model_name(),
            output.input_tokens,
            output.output_tokens,
        );
        {
            let mut state = bs.write();
            state.record_spend(cost_usd);
            let budget_path = tool_ctx.data_dir.join("budget_state.json");
            let state_clone = state.clone();
            drop(state);
            tokio::spawn(async move {
                if let Err(e) = state_clone.save(&budget_path) {
                    tracing::warn!("budget state save failed: {e}");
                }
            });
        }
    }

    Ok(output.content)
}

// ---------------------------------------------------------------------------
// Embedding provider initialization
// ---------------------------------------------------------------------------

async fn init_embedding(
    cfg: &animus_core::config::EmbeddingConfig,
) -> (Arc<dyn animus_core::EmbeddingService>, usize) {
    use animus_core::config::EmbeddingProviderKind;

    match cfg.provider {
        EmbeddingProviderKind::Synthetic => {
            let dim = if cfg.dimensionality > 0 { cfg.dimensionality } else { 128 };
            tracing::info!("Using SyntheticEmbedding ({dim} dims)");
            (Arc::new(SyntheticEmbedding::new(dim)), dim)
        }
        EmbeddingProviderKind::Ollama => {
            match OllamaEmbedding::probe(&cfg.ollama_url, &cfg.model).await {
                Ok(detected_dim) => {
                    let dim = if cfg.dimensionality > 0 { cfg.dimensionality } else { detected_dim };
                    tracing::info!(
                        "Using Ollama embeddings at {} ({}, {dim} dims) with resilient fallback",
                        cfg.ollama_url, cfg.model
                    );
                    let ollama = OllamaEmbedding::new(&cfg.ollama_url, &cfg.model, dim);
                    (Arc::new(ResilientEmbedding::new(ollama, dim)), dim)
                }
                Err(e) => {
                    let dim = if cfg.dimensionality > 0 { cfg.dimensionality } else { 128 };
                    tracing::warn!(
                        "Ollama unavailable at {} ({e}); falling back to SyntheticEmbedding ({dim} dims)",
                        cfg.ollama_url
                    );
                    (Arc::new(SyntheticEmbedding::new(dim)), dim)
                }
            }
        }
        EmbeddingProviderKind::OpenAI => {
            // OpenAI embedding bridge is not linked — this would produce synthetic embeddings
            // that are incompatible with any real embeddings already stored, corrupting VectorFS.
            // Fail loudly rather than silently producing garbage embeddings.
            eprintln!("Fatal: embedding provider 'openai' is not implemented. \
                Use 'ollama' (with mxbai-embed-large or similar) or 'synthetic'. \
                See docs/embedding-providers.md for configuration.");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Health endpoint
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct HealthState {
    instance_id: String,
    store: Arc<MmapVectorStore>,
    version: &'static str,
}

async fn health_handler(State(state): State<HealthState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": state.version,
        "instance_id": state.instance_id,
        "segments": state.store.count(None),
    }))
}

fn start_health_server(bind: String, store: Arc<MmapVectorStore>, instance_id: String) {
    let state = HealthState {
        instance_id,
        store,
        version: env!("CARGO_PKG_VERSION"),
    };
    let app = Router::new()
        .route("/health", get(health_handler))
        .with_state(state);

    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(&bind).await {
            Ok(listener) => {
                tracing::info!("Health endpoint listening on http://{bind}/health");
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::warn!("Health server error: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("Could not bind health endpoint to {bind}: {e}");
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute embeddings for all active goals and update the Sensorium orchestrator.
async fn update_goal_embeddings(
    goals: &GoalManager,
    embedder: &dyn animus_core::EmbeddingService,
    sensorium: &SensoriumOrchestrator,
) {
    let active = goals.active_goals();
    if active.is_empty() {
        sensorium.set_goal_embeddings(Vec::new());
        return;
    }
    let texts: Vec<&str> = active.iter().map(|g| g.description.as_str()).collect();
    match embedder.embed_texts(&texts).await {
        Ok(embeddings) => {
            sensorium.set_goal_embeddings(embeddings);
            tracing::debug!("Updated Tier 2 attention with {} goal embeddings", active.len());
        }
        Err(e) => {
            tracing::warn!("Failed to compute goal embeddings: {e}");
        }
    }
}

fn build_system_prompt(
    _scheduler: &ThreadScheduler<MmapVectorStore>,
    goals: &GoalManager,
    tool_registry: &ToolRegistry,
    reconstitution_summary: Option<&str>,
    peripheral_awareness: Option<&str>,
) -> String {
    // Auto-generate tool catalog from the actually registered tools.
    // Constitution: "Real Capabilities, Not Hallucinated Ones" — the prompt must match reality.
    let tool_catalog = tool_registry.tool_catalog_prompt();
    let mut prompt = String::with_capacity(
        SYSTEM_PROMPT_PREAMBLE.len() + tool_catalog.len() + SYSTEM_PROMPT_SUFFIX.len() + 512,
    );
    prompt.push_str(SYSTEM_PROMPT_PREAMBLE);
    prompt.push_str(&tool_catalog);
    prompt.push_str(SYSTEM_PROMPT_SUFFIX);

    let goals_summary = goals.goals_summary();
    if !goals_summary.is_empty() {
        prompt.push_str("\n\n## Current Goals\n");
        prompt.push_str(&goals_summary);
    }
    if let Some(summary) = reconstitution_summary {
        prompt.push_str("\n\n## Session Context (from reconstitution)\n");
        prompt.push_str(summary);
    }
    // Peripheral awareness is appended last — first to be compressed under context pressure.
    if let Some(awareness) = peripheral_awareness {
        if !awareness.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(awareness);
        }
    }
    prompt
}

/// A message that Animus initiates to the user unprompted.
/// Sent via `proactive_tx`; main loop gates on autonomy mode before delivering.
struct ProactiveMessage {
    text: String,
    /// Human-readable source label for tracing (e.g. "goal_deadline", "reflection").
    source: &'static str,
}

enum CommandResult {
    Continue,
    Quit,
}

struct CommandContext<'a> {
    store: &'a Arc<MmapVectorStore>,
    goals: &'a mut GoalManager,
    goals_path: &'a std::path::Path,
    interface: &'a TerminalInterface,
    embedder: &'a dyn animus_core::EmbeddingService,
    data_dir: &'a std::path::Path,
    snapshot_dir: &'a std::path::Path,
    scheduler: &'a mut ThreadScheduler<MmapVectorStore>,
    federation: Option<Arc<FederationOrchestrator<MmapVectorStore>>>,
    event_bus: &'a Arc<EventBus>,
    file_watcher: &'a Arc<parking_lot::Mutex<Option<animus_sensorium::sensors::file_watcher::FileWatcher>>>,
    sensorium: &'a Arc<SensoriumOrchestrator>,
    is_sleeping: &'a mut bool,
    sleep_started: &'a mut Option<chrono::DateTime<chrono::Utc>>,
    sleeping_flag: &'a Arc<std::sync::atomic::AtomicBool>,
    watcher_registry: &'a animus_cortex::WatcherRegistry,
    task_manager: &'a animus_cortex::TaskManager,
    api_tracker: &'a Arc<animus_core::ApiTracker>,
    voice_active: &'a Arc<std::sync::atomic::AtomicBool>,
    voice_configured: bool,
    voice_state_path: &'a std::path::Path,
}

async fn handle_command(
    input: &str,
    ctx: &mut CommandContext<'_>,
) -> animus_core::Result<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");

    match cmd {
        "/quit" | "/exit" | "/q" => {
            return Ok(CommandResult::Quit);
        }
        "/sleep" => {
            if *ctx.is_sleeping {
                ctx.interface.display_status("Already sleeping.");
            } else {
                *ctx.is_sleeping = true;
                *ctx.sleep_started = Some(chrono::Utc::now());
                ctx.sleeping_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                ctx.interface.display_status("Entering sleep mode. Sensorium continues logging to Cold tier.");
                ctx.interface.display_status("Use /wake to resume, /status to check state.");
            }
        }
        "/wake" => {
            if !*ctx.is_sleeping {
                ctx.interface.display_status("Already awake.");
            } else {
                let sleep_start = ctx.sleep_started.take().unwrap_or_else(chrono::Utc::now);
                *ctx.is_sleeping = false;
                ctx.sleeping_flag.store(false, std::sync::atomic::Ordering::Relaxed);

                // Summarize what happened during sleep
                let cold_segments = ctx.store.segment_ids(Some(animus_core::Tier::Cold));
                let created_during_sleep: Vec<_> = cold_segments.iter().filter(|id| {
                    ctx.store.get_raw(**id).ok().flatten().is_some_and(|s| s.created >= sleep_start)
                }).collect();

                let duration = chrono::Utc::now() - sleep_start;
                let hours = duration.num_hours();
                let minutes = duration.num_minutes() % 60;

                ctx.interface.display_status(&format!(
                    "Waking up. Slept for {}h {}m.",
                    hours, minutes
                ));

                if created_during_sleep.is_empty() {
                    ctx.interface.display_status("Nothing notable happened while sleeping.");
                } else {
                    ctx.interface.display_status(&format!(
                        "{} observations logged during sleep:",
                        created_during_sleep.len()
                    ));
                    for (i, id) in created_during_sleep.iter().take(10).enumerate() {
                        if let Ok(Some(seg)) = ctx.store.get_raw(**id) {
                            let preview = match &seg.content {
                                animus_core::Content::Text(t) => {
                                    let truncated: String = t.chars().take(80).collect();
                                    if t.chars().count() > 80 { format!("{truncated}...") } else { truncated }
                                }
                                animus_core::Content::Structured(v) => {
                                    let s = v.to_string();
                                    let truncated: String = s.chars().take(80).collect();
                                    if s.chars().count() > 80 { format!("{truncated}...") } else { truncated }
                                }
                                animus_core::Content::Binary { mime_type, .. } => format!("[binary: {mime_type}]"),
                                animus_core::Content::Reference { uri, .. } => format!("[ref: {uri}]"),
                            };
                            ctx.interface.display(&format!("  {}. {}", i + 1, preview));
                        }
                    }
                    if created_during_sleep.len() > 10 {
                        ctx.interface.display(&format!(
                            "  ... and {} more",
                            created_during_sleep.len() - 10
                        ));
                    }
                }
            }
        }
        "/voice" => {
            match arg {
                "on" => {
                    if !ctx.voice_configured {
                        ctx.interface.display_status(
                            "Voice service is not configured. Set ANIMUS_VOICE_ENABLED=1 and configure STT/TTS credentials.",
                        );
                    } else {
                        ctx.voice_active.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = std::fs::write(ctx.voice_state_path, "true");
                        ctx.interface.display_status("Voice enabled.");
                    }
                }
                "off" => {
                    ctx.voice_active.store(false, std::sync::atomic::Ordering::Relaxed);
                    let _ = std::fs::write(ctx.voice_state_path, "false");
                    ctx.interface.display_status("Voice disabled. Use /voice on to re-enable.");
                }
                _ => {
                    let state = if ctx.voice_active.load(std::sync::atomic::Ordering::Relaxed) {
                        "on"
                    } else {
                        "off"
                    };
                    let configured = if ctx.voice_configured { "configured" } else { "not configured" };
                    ctx.interface.display_status(&format!("Voice: {state} ({configured})"));
                }
            }
        }
        "/status" => {
            let total = ctx.store.count(None);
            let warm = ctx.store.count(Some(animus_core::Tier::Warm));
            let cold = ctx.store.count(Some(animus_core::Tier::Cold));
            let hot = ctx.store.count(Some(animus_core::Tier::Hot));
            ctx.interface.display_status(&format!(
                "Segments: {total} total ({hot} hot, {warm} warm, {cold} cold)"
            ));

            // Aggregate quality metrics
            if total > 0 {
                let all_ids = ctx.store.segment_ids(None);
                let mut sum_health = 0.0_f32;
                let mut sum_confidence = 0.0_f32;
                let mut decay_counts = [0usize; 5]; // Factual, Procedural, Episodic, Opinion, General
                let mut count = 0usize;
                for id in &all_ids {
                    if let Ok(Some(seg)) = ctx.store.get_raw(*id) {
                        sum_health += seg.health_score();
                        sum_confidence += seg.bayesian_confidence();
                        match seg.decay_class {
                            animus_core::DecayClass::Factual => decay_counts[0] += 1,
                            animus_core::DecayClass::Procedural => decay_counts[1] += 1,
                            animus_core::DecayClass::Episodic => decay_counts[2] += 1,
                            animus_core::DecayClass::Opinion => decay_counts[3] += 1,
                            animus_core::DecayClass::General => decay_counts[4] += 1,
                            animus_core::DecayClass::Ephemeral => {} // counted as noise, not in health stats
                        }
                        count += 1;
                    }
                }
                if count > 0 {
                    let avg_health = sum_health / count as f32;
                    let avg_conf = sum_confidence / count as f32;
                    ctx.interface.display_status(&format!(
                        "Quality: avg health {avg_health:.2}, avg confidence {avg_conf:.2}"
                    ));
                    ctx.interface.display_status(&format!(
                        "Knowledge: {} factual, {} procedural, {} episodic, {} opinion, {} general",
                        decay_counts[0], decay_counts[1], decay_counts[2],
                        decay_counts[3], decay_counts[4]
                    ));
                }
            }

            ctx.interface.display_status(&format!("Goals: {} active", ctx.goals.active_goals().len()));
            if *ctx.is_sleeping {
                let since = ctx.sleep_started.map(|t| {
                    let d = chrono::Utc::now() - t;
                    format!("{}h {}m", d.num_hours(), d.num_minutes() % 60)
                }).unwrap_or_else(|| "unknown".to_string());
                ctx.interface.display_status(&format!("State: SLEEPING (for {since})"));
            } else {
                ctx.interface.display_status("State: AWAKE");
            }
        }
        "/goals" => {
            let active = ctx.goals.active_goals();
            if active.is_empty() {
                ctx.interface.display_status("No active goals.");
            } else {
                for goal in active {
                    ctx.interface.display_status(&format!(
                        "[{:?}] {} ({})",
                        goal.priority,
                        goal.description,
                        goal.id.0.to_string().get(..8).unwrap_or("?")
                    ));
                }
            }
        }
        "/goal" if !arg.is_empty() => {
            const MAX_GOAL_DESC_BYTES: usize = 1024; // consistent with federation handle_goals cap
            if arg.len() > MAX_GOAL_DESC_BYTES {
                ctx.interface.display_status(&format!(
                    "Goal description too long: {} bytes (max {MAX_GOAL_DESC_BYTES})",
                    arg.len()
                ));
            } else {
            match ctx.goals.create_goal(arg.to_string(), GoalSource::Human, Priority::Normal) {
                Ok(id) => {
                    ctx.goals.save(ctx.goals_path)?;
                    update_goal_embeddings(ctx.goals, ctx.embedder, ctx.sensorium).await;
                    ctx.interface.display_status(&format!(
                        "Goal created: {}",
                        id.0.to_string().get(..8).unwrap_or("?")
                    ));
                }
                Err(e) => {
                    ctx.interface.display_status(&format!("Failed to create goal: {e}"));
                }
            }
            } // end size check
        }
        "/remember" if !arg.is_empty() => {
            use animus_core::segment::{Content, Segment, Source};
            const MAX_REMEMBER_BYTES: usize = 10 * 1024; // 10 KiB — consistent with RememberTool
            if arg.len() > MAX_REMEMBER_BYTES {
                ctx.interface.display_status(&format!(
                    "Too large to remember: {} bytes (max {MAX_REMEMBER_BYTES}). Please summarize first.",
                    arg.len()
                ));
            } else {
            let embedding = ctx.embedder.embed_text(arg).await?;
            let mut segment = Segment::new(
                Content::Text(arg.to_string()),
                embedding,
                Source::Manual {
                    description: "user-remember".to_string(),
                },
            );
            segment.infer_decay_class();
            let id = ctx.store.store(segment)?;
            ctx.interface.display_status(&format!(
                "Remembered: {} (segment {})",
                arg,
                id.0.to_string().get(..8).unwrap_or("?")
            ));
            } // end else branch for size check
        }
        "/forget" if !arg.is_empty() => {
            // Match segment by ID prefix
            let all_ids = ctx.store.segment_ids(None);
            let matches: Vec<_> = all_ids
                .iter()
                .filter(|id| id.0.to_string().starts_with(arg))
                .collect();
            match matches.len() {
                0 => ctx.interface.display_status(&format!("No segment found matching '{arg}'")),
                1 => {
                    let id = *matches[0];
                    ctx.store.delete(id)?;
                    ctx.interface.display_status(&format!(
                        "Forgotten: segment {}",
                        id.0.to_string().get(..8).unwrap_or("?")
                    ));
                }
                n => ctx.interface.display_status(&format!(
                    "{n} segments match '{arg}' — be more specific"
                )),
            }
        }
        // /accept — record positive feedback for segments used in the last turn
        "/accept" => {
            if let Some(thread) = ctx.scheduler.active_thread() {
                let retrieved = thread.last_retrieved_ids().to_vec();
                if retrieved.is_empty() {
                    ctx.interface.display_status("No retrieved segments to accept (no knowledge was used in the last response).");
                } else {
                    let mut updated = 0;
                    const MAX_BAYES_PARAM: f32 = 100.0;
                    for id in &retrieved {
                        if let Ok(Some(mut seg)) = ctx.store.get_raw(*id) {
                            seg.record_positive_feedback();
                            // Cap alpha, then recompute confidence from the capped values
                            // so the stored confidence is consistent with stored alpha/beta.
                            let capped_alpha = seg.alpha.min(MAX_BAYES_PARAM);
                            let capped_confidence = capped_alpha / (capped_alpha + seg.beta);
                            if let Err(e) = ctx.store.update_meta(*id, animus_vectorfs::SegmentUpdate {
                                alpha: Some(capped_alpha),
                                confidence: Some(capped_confidence),
                                ..Default::default()
                            }) {
                                tracing::warn!("Failed to update feedback for {id}: {e}");
                            } else {
                                updated += 1;
                            }
                        }
                    }
                    ctx.interface.display_status(&format!(
                        "Accepted: positive feedback recorded for {updated} knowledge segment(s)"
                    ));
                }
            } else {
                ctx.interface.display_status("No active thread.");
            }
        }
        // /correct — record negative feedback for segments used in the last turn
        "/correct" => {
            if let Some(thread) = ctx.scheduler.active_thread() {
                let retrieved = thread.last_retrieved_ids().to_vec();
                if retrieved.is_empty() {
                    ctx.interface.display_status("No retrieved segments to correct (no knowledge was used in the last response).");
                } else {
                    let mut updated = 0;
                    const MAX_BAYES_PARAM: f32 = 100.0;
                    for id in &retrieved {
                        if let Ok(Some(mut seg)) = ctx.store.get_raw(*id) {
                            seg.record_negative_feedback();
                            // Cap beta, then recompute confidence from capped values.
                            let capped_beta = seg.beta.min(MAX_BAYES_PARAM);
                            let capped_confidence = seg.alpha / (seg.alpha + capped_beta);
                            if let Err(e) = ctx.store.update_meta(*id, animus_vectorfs::SegmentUpdate {
                                beta: Some(capped_beta),
                                confidence: Some(capped_confidence),
                                ..Default::default()
                            }) {
                                tracing::warn!("Failed to update feedback for {id}: {e}");
                            } else {
                                updated += 1;
                            }
                        }
                    }
                    ctx.interface.display_status(&format!(
                        "Corrected: negative feedback recorded for {updated} knowledge segment(s)"
                    ));
                }
            } else {
                ctx.interface.display_status("No active thread.");
            }
        }
        // /tag <segment-prefix> <key>=<value> — add a tag to a segment
        "/tag" if !arg.is_empty() => {
            let parts: Vec<&str> = arg.splitn(2, ' ').collect();
            if parts.len() < 2 || !parts[1].contains('=') {
                ctx.interface.display_status("Usage: /tag <segment-id-prefix> <key>=<value>");
            } else {
                let prefix = parts[0];
                let kv: Vec<&str> = parts[1].splitn(2, '=').collect();
                let (key, value) = (kv[0].to_string(), kv[1].to_string());

                let all_ids = ctx.store.segment_ids(None);
                let matches: Vec<_> = all_ids
                    .iter()
                    .filter(|id| id.0.to_string().starts_with(prefix))
                    .collect();
                match matches.len() {
                    0 => ctx.interface.display_status(&format!("No segment found matching '{prefix}'")),
                    1 => {
                        const MAX_TAG_COUNT: usize = 50;
                        const MAX_TAG_BYTES: usize = 256;
                        if key.len() > MAX_TAG_BYTES || value.len() > MAX_TAG_BYTES {
                            ctx.interface.display_status(&format!(
                                "Tag key or value too long (max {MAX_TAG_BYTES} bytes)"
                            ));
                        } else {
                        let id = *matches[0];
                        // Get current tags, add the new one
                        let mut tags = match ctx.store.get_raw(id)? {
                            Some(seg) => seg.tags,
                            None => std::collections::HashMap::new(),
                        };
                        if tags.len() >= MAX_TAG_COUNT && !tags.contains_key(&key) {
                            ctx.interface.display_status(&format!(
                                "Segment already has {MAX_TAG_COUNT} tags — remove one first"
                            ));
                        } else {
                        tags.insert(key.clone(), value.clone());
                        ctx.store.update_meta(id, animus_vectorfs::SegmentUpdate {
                            tags: Some(tags),
                            ..Default::default()
                        })?;
                        ctx.interface.display_status(&format!(
                            "Tagged segment {} with {key}={value}",
                            id.0.to_string().get(..8).unwrap_or("?")
                        ));
                        } // end cap check
                        } // end key/value length check
                    }
                    n => ctx.interface.display_status(&format!(
                        "{n} segments match '{prefix}' — be more specific"
                    )),
                }
            }
        }
        // /classify <segment-prefix> <class> — set decay class for a segment
        "/classify" if !arg.is_empty() => {
            let parts: Vec<&str> = arg.splitn(2, ' ').collect();
            if parts.len() < 2 {
                ctx.interface.display_status("Usage: /classify <segment-id-prefix> <factual|procedural|episodic|opinion|general>");
            } else {
                let prefix = parts[0];
                let class_str = parts[1].to_lowercase();
                let decay_class = match class_str.as_str() {
                    "factual" => Some(animus_core::DecayClass::Factual),
                    "procedural" => Some(animus_core::DecayClass::Procedural),
                    "episodic" => Some(animus_core::DecayClass::Episodic),
                    "opinion" => Some(animus_core::DecayClass::Opinion),
                    "general" => Some(animus_core::DecayClass::General),
                    _ => None,
                };
                match decay_class {
                    None => {
                        ctx.interface.display_status("Valid classes: factual, procedural, episodic, opinion, general");
                    }
                    Some(dc) => {
                        let all_ids = ctx.store.segment_ids(None);
                        let matches: Vec<_> = all_ids
                            .iter()
                            .filter(|id| id.0.to_string().starts_with(prefix))
                            .collect();
                        match matches.len() {
                            0 => ctx.interface.display_status(&format!("No segment found matching '{prefix}'")),
                            1 => {
                                let id = *matches[0];
                                ctx.store.update_meta(id, animus_vectorfs::SegmentUpdate {
                                    decay_class: Some(dc),
                                    ..Default::default()
                                })?;
                                ctx.interface.display_status(&format!(
                                    "Classified segment {} as {class_str} (half-life: {} days)",
                                    id.0.to_string().get(..8).unwrap_or("?"),
                                    dc.half_life_secs() / 86400.0
                                ));
                            }
                            n => ctx.interface.display_status(&format!(
                                "{n} segments match '{prefix}' — be more specific"
                            )),
                        }
                    }
                }
            }
        }
        // /health <segment-prefix> — show health details for a segment
        "/health" if !arg.is_empty() => {
            let all_ids = ctx.store.segment_ids(None);
            let matches: Vec<_> = all_ids
                .iter()
                .filter(|id| id.0.to_string().starts_with(arg))
                .collect();
            match matches.len() {
                0 => ctx.interface.display_status(&format!("No segment found matching '{arg}'")),
                1 => {
                    let id = *matches[0];
                    if let Some(seg) = ctx.store.get_raw(id)? {
                        let short_id = seg.id.0.to_string();
                        let short_id = short_id.get(..8).unwrap_or("?");
                        ctx.interface.display(&format!("Segment {short_id} health:"));
                        ctx.interface.display(&format!("  Bayesian confidence: {:.3} (alpha={:.1}, beta={:.1})",
                            seg.bayesian_confidence(), seg.alpha, seg.beta));
                        ctx.interface.display(&format!("  Temporal decay: {:.3} (class={:?})",
                            seg.temporal_decay_factor(), seg.decay_class));
                        ctx.interface.display(&format!("  Health score: {:.3}", seg.health_score()));
                        ctx.interface.display(&format!("  Relevance: {:.3}", seg.relevance_score));
                        ctx.interface.display(&format!("  Access count: {}", seg.access_count));
                        ctx.interface.display(&format!("  Tier: {:?}", seg.tier));

                        let age_days = (chrono::Utc::now() - seg.created).num_hours() as f64 / 24.0;
                        ctx.interface.display(&format!("  Age: {:.1} days", age_days));
                    }
                }
                n => ctx.interface.display_status(&format!(
                    "{n} segments match '{arg}' — be more specific"
                )),
            }
        }
        "/sensorium" => {
            let audit_entries = animus_sensorium::audit::AuditTrail::read_recent(
                &ctx.data_dir.join("sensorium-audit.jsonl"),
                10_000,
            )
            .ok()
            .unwrap_or_default();
            let total = audit_entries.len();
            let permitted = audit_entries
                .iter()
                .filter(|e| e.action_taken != AuditAction::DeniedByConsent)
                .count();
            let promoted = audit_entries
                .iter()
                .filter(|e| e.action_taken == AuditAction::Promoted)
                .count();
            ctx.interface.display_status(&format!(
                "Sensorium: {total} events observed, {permitted} permitted, {promoted} promoted"
            ));
        }
        "/task" if matches!(arg.split_whitespace().next(), Some("list" | "cancel")) => {
            let arg = arg.trim_start();
            let parts: Vec<&str> = arg.splitn(2, ' ').collect();
            match parts[0] {
                "list" => {
                    let records = ctx.task_manager.list_all();
                    if records.is_empty() {
                        ctx.interface.display_status("No tasks.");
                    } else {
                        let now = chrono::Utc::now();
                        let header = format!("{:<10} {:<32} {:<12} {:<10} EXIT", "ID", "LABEL", "STATE", "RUNTIME");
                        let mut lines = vec![header];
                        for rec in &records {
                            let end = rec.finished_at.unwrap_or(now);
                            let secs = (end - rec.spawned_at).num_seconds().max(0);
                            let runtime = format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60);
                            let exit = rec.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string());
                            let label: &str = rec.label.char_indices().nth(32)
                                .map(|(i, _)| &rec.label[..i])
                                .unwrap_or(&rec.label);
                            lines.push(format!("{:<10} {:<32} {:<12} {:<10} {}", rec.id, label, format!("{:?}", rec.state), runtime, exit));
                        }
                        ctx.interface.display_status(&lines.join("\n"));
                    }
                }
                "cancel" => {
                    let id = match parts.get(1) {
                        Some(id) => *id,
                        None => {
                            ctx.interface.display_status("Usage: /task cancel <id>");
                            return Ok(CommandResult::Continue);
                        }
                    };
                    match ctx.task_manager.cancel_task(id).await {
                        Ok(msg) => ctx.interface.display_status(&msg),
                        Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
                    }
                }
                _ => ctx.interface.display_status("Unknown /task subcommand. Use: list, cancel <id>"),
            }
        }
        "/watch" if matches!(arg.split_whitespace().next(), Some("list" | "enable" | "disable" | "set")) => {
            let parts: Vec<&str> = arg.splitn(3, ' ').collect();
            let sub = parts[0];
            match sub {
                "list" => {
                    let entries = ctx.watcher_registry.list();
                    if entries.is_empty() {
                        ctx.interface.display_status("No watchers registered.");
                    } else {
                        ctx.interface.display_status("Registered watchers:");
                        for (id, name, cfg) in &entries {
                            let state = if cfg.enabled { "enabled" } else { "disabled" };
                            let interval = cfg.interval.map(|d| format!("{}s", d.as_secs())).unwrap_or_else(|| "default".to_string());
                            let last_fired = cfg.last_fired.map(|t| t.to_rfc3339()).unwrap_or_else(|| "never".to_string());
                            ctx.interface.display(&format!("  {id} — {name} [{state}] interval={interval} last_fired={last_fired}"));
                        }
                    }
                }

                "enable" => {
                    let watcher_id = match parts.get(1) {
                        Some(id) => *id,
                        None => {
                            ctx.interface.display_status("Usage: /watch enable <id> [interval=<N>s]");
                            return Ok(CommandResult::Continue);
                        }
                    };
                    if !ctx.watcher_registry.has_watcher(watcher_id) {
                        ctx.interface.display_status(&format!("Unknown watcher: {watcher_id}"));
                        return Ok(CommandResult::Continue);
                    }
                    let mut cfg = ctx.watcher_registry.get_config(watcher_id);
                    cfg.enabled = true;
                    if let Some(opts) = parts.get(2) {
                        for kv in opts.split_whitespace() {
                            if let Some(val) = kv.strip_prefix("interval=") {
                                let secs_str = val.trim_end_matches('s');
                                if let Ok(secs) = secs_str.parse::<u64>() {
                                    cfg.interval = Some(std::time::Duration::from_secs(secs));
                                }
                            }
                        }
                    }
                    match ctx.watcher_registry.update_config(watcher_id, cfg) {
                        Ok(()) => ctx.interface.display_status(&format!("Watcher '{watcher_id}' enabled.")),
                        Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
                    }
                }

                "disable" => {
                    let watcher_id = match parts.get(1) {
                        Some(id) => *id,
                        None => {
                            ctx.interface.display_status("Usage: /watch disable <id>");
                            return Ok(CommandResult::Continue);
                        }
                    };
                    if !ctx.watcher_registry.has_watcher(watcher_id) {
                        ctx.interface.display_status(&format!("Unknown watcher: {watcher_id}"));
                        return Ok(CommandResult::Continue);
                    }
                    let mut cfg = ctx.watcher_registry.get_config(watcher_id);
                    cfg.enabled = false;
                    match ctx.watcher_registry.update_config(watcher_id, cfg) {
                        Ok(()) => ctx.interface.display_status(&format!("Watcher '{watcher_id}' disabled.")),
                        Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
                    }
                }

                "set" => {
                    let watcher_id = match parts.get(1) {
                        Some(id) => *id,
                        None => {
                            ctx.interface.display_status("Usage: /watch set <id> <key>=<value>");
                            return Ok(CommandResult::Continue);
                        }
                    };
                    if !ctx.watcher_registry.has_watcher(watcher_id) {
                        ctx.interface.display_status(&format!("Unknown watcher: {watcher_id}"));
                        return Ok(CommandResult::Continue);
                    }
                    let kv_str = match parts.get(2) {
                        Some(s) => *s,
                        None => {
                            ctx.interface.display_status("Usage: /watch set <id> <key>=<value>");
                            return Ok(CommandResult::Continue);
                        }
                    };
                    let (key, value) = match kv_str.split_once('=') {
                        Some(pair) => pair,
                        None => {
                            ctx.interface.display_status("Usage: /watch set <id> <key>=<value>");
                            return Ok(CommandResult::Continue);
                        }
                    };
                    let mut cfg = ctx.watcher_registry.get_config(watcher_id);
                    let mut existing = match cfg.params.take() {
                        serde_json::Value::Object(m) => m,
                        _ => serde_json::Map::new(),
                    };
                    existing.insert(key.to_string(), serde_json::Value::String(value.to_string()));
                    cfg.params = serde_json::Value::Object(existing);
                    match ctx.watcher_registry.update_config(watcher_id, cfg) {
                        Ok(()) => ctx.interface.display_status(&format!("Watcher '{watcher_id}' param '{key}' set to '{value}'.")),
                        Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
                    }
                }

                _ => {
                    ctx.interface.display_status("Unknown /watch subcommand. Use: list, enable, disable, set");
                }
            }
        }
        "/watch" if !arg.is_empty() => {
            use std::path::{Component, Path};
            let has_traversal = Path::new(arg).components()
                .any(|c| matches!(c, Component::ParentDir));
            if has_traversal {
                ctx.interface.display_status("Invalid watch path: parent-directory traversal not allowed");
            } else {
            let watch_path = std::path::PathBuf::from(arg);
            if !watch_path.exists() {
                ctx.interface.display_status(&format!("Path does not exist: {arg}"));
            } else {
                let mut fw_guard = ctx.file_watcher.lock();
                // Stop existing watcher if any
                if let Some(existing) = fw_guard.take() {
                    existing.stop();
                }
                match animus_sensorium::sensors::file_watcher::FileWatcher::new(
                    ctx.event_bus.clone(),
                    vec![watch_path],
                ) {
                    Ok(mut watcher) => {
                        match watcher.start() {
                            Ok(()) => {
                                ctx.interface.display_status(&format!("Now watching: {arg}"));
                                *fw_guard = Some(watcher);
                            }
                            Err(e) => {
                                ctx.interface.display_status(&format!("Failed to start file watcher: {e}"));
                            }
                        }
                    }
                    Err(e) => {
                        ctx.interface.display_status(&format!("Failed to create file watcher: {e}"));
                    }
                }
            }
            } // end traversal check
        }
        "/consent" => {
            use animus_core::sensorium::{AuditLevel, ConsentPolicy, ConsentRule, EventType, Permission, Scope};
            use animus_core::identity::PolicyId;
            let consent_path = ctx.data_dir.join("consent-policies.json");

            let sub_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            let sub = sub_parts.first().copied().unwrap_or("list");
            let scope_str = sub_parts.get(1).copied().unwrap_or("").trim();

            match sub {
                "list" | "" => {
                    let loaded = animus_sensorium::policy_store::PolicyStore::load(&consent_path)
                        .ok()
                        .unwrap_or_default();
                    if loaded.is_empty() {
                        ctx.interface.display_status(
                            "No consent rules defined. Use /consent allow <scope> or /consent deny <scope>.",
                        );
                    } else {
                        ctx.interface.display("── Consent Rules ──────────────────────────────");
                        for policy in &loaded {
                            let status = if policy.active { "active" } else { "inactive" };
                            for rule in &policy.rules {
                                let perm = match rule.permission {
                                    Permission::Allow => "ALLOW",
                                    Permission::Deny  => "DENY ",
                                    Permission::AllowAnonymized => "ALLOW(anon)",
                                };
                                let scope_label = match &rule.scope {
                                    Scope::All => "*".to_string(),
                                    Scope::PathGlob(p) => p.clone(),
                                    Scope::ProcessName(n) => format!("process:{n}"),
                                };
                                ctx.interface.display(&format!(
                                    "  [{}] {} {} — {} ({})",
                                    policy.id.0.to_string().get(..8).unwrap_or("?"),
                                    perm,
                                    scope_label,
                                    policy.name,
                                    status,
                                ));
                            }
                        }
                    }
                }
                "allow" | "deny" => {
                    if scope_str.is_empty() {
                        ctx.interface.display_status(&format!(
                            "Usage: /consent {} <scope>  (e.g. /consent {} /home/user/**)",
                            sub, sub
                        ));
                    } else {
                        let permission = if sub == "allow" { Permission::Allow } else { Permission::Deny };
                        let scope = if scope_str == "*" {
                            Scope::All
                        } else {
                            Scope::PathGlob(scope_str.to_string())
                        };
                        let all_event_types = vec![
                            EventType::FileChange,
                            EventType::ProcessLifecycle,
                            EventType::SystemResources,
                            EventType::Network,
                            EventType::Clipboard,
                            EventType::WindowFocus,
                            EventType::UsbDevice,
                        ];
                        let rule = ConsentRule {
                            event_types: all_event_types,
                            scope,
                            permission,
                            audit_level: AuditLevel::MetadataOnly,
                        };
                        let policy = ConsentPolicy {
                            id: PolicyId::new(),
                            name: format!("{} {}", sub, scope_str),
                            rules: vec![rule],
                            active: true,
                            created: chrono::Utc::now(),
                            created_by: None,
                        };
                        let mut policies = animus_sensorium::policy_store::PolicyStore::load(&consent_path)
                            .ok()
                            .unwrap_or_default();
                        policies.push(policy);
                        match animus_sensorium::policy_store::PolicyStore::save(&consent_path, &policies) {
                            Ok(()) => ctx.interface.display_status(&format!(
                                "Consent rule added: {} {}",
                                sub.to_uppercase(),
                                scope_str
                            )),
                            Err(e) => ctx.interface.display_status(&format!("Failed to save consent rules: {e}")),
                        }
                    }
                }
                _ => {
                    ctx.interface.display_status("Usage: /consent list | /consent allow <scope> | /consent deny <scope>");
                }
            }
        }
        "/threads" => {
            let threads = ctx.scheduler.list_threads();
            if threads.is_empty() {
                ctx.interface.display_status("No threads.");
            } else {
                for (id, name, status) in &threads {
                    let active_marker = if Some(*id) == ctx.scheduler.active_thread_id() { " *" } else { "" };
                    ctx.interface.display_status(&format!(
                        "[{}] {} ({:?}){}",
                        id.0.to_string().get(..8).unwrap_or("?"),
                        name,
                        status,
                        active_marker,
                    ));
                }
            }
        }
        "/thread" if arg.starts_with("new ") => {
            let name = arg.strip_prefix("new ").unwrap().trim();
            if name.is_empty() {
                ctx.interface.display_status("Usage: /thread new <name>");
            } else {
                const MAX_THREADS: usize = 64;
                if ctx.scheduler.thread_count() >= MAX_THREADS {
                    ctx.interface.display_status(&format!(
                        "Thread limit reached ({MAX_THREADS}). Complete or archive existing threads first."
                    ));
                } else {
                    let id = ctx.scheduler.create_thread(name.to_string());
                    ctx.interface.display_status(&format!(
                        "Thread created: {} ({})",
                        name,
                        id.0.to_string().get(..8).unwrap_or("?")
                    ));
                }
            }
        }
        "/thread" if arg.starts_with("switch ") => {
            let prefix = arg.strip_prefix("switch ").unwrap().trim();
            let threads = ctx.scheduler.list_threads();
            let matches: Vec<_> = threads.iter()
                .filter(|(id, _, _)| id.0.to_string().starts_with(prefix))
                .collect();
            match matches.len() {
                0 => ctx.interface.display_status(&format!("No thread found matching '{prefix}'")),
                1 => {
                    let (id, name, _) = matches[0];
                    ctx.scheduler.switch_to(*id)?;
                    ctx.interface.display_status(&format!("Switched to thread: {name}"));
                }
                n => ctx.interface.display_status(&format!("{n} threads match '{prefix}' — be more specific")),
            }
        }
        "/thread" if arg.starts_with("complete ") => {
            let prefix = arg.strip_prefix("complete ").unwrap().trim();
            let threads = ctx.scheduler.list_threads();
            let matches: Vec<_> = threads.iter()
                .filter(|(id, _, _)| id.0.to_string().starts_with(prefix))
                .collect();
            match matches.len() {
                0 => ctx.interface.display_status(&format!("No thread found matching '{prefix}'")),
                1 => {
                    let (id, name, _) = matches[0];
                    ctx.scheduler.complete(*id)?;
                    ctx.interface.display_status(&format!("Thread completed: {name}"));
                }
                n => ctx.interface.display_status(&format!("{n} threads match '{prefix}' — be more specific")),
            }
        }
        "/peers" => {
            if let Some(ref fed) = ctx.federation {
                let registry = fed.peers();
                let peers = registry.read();
                let all = peers.all_peers();
                if all.is_empty() {
                    ctx.interface.display_status("No peers discovered.");
                } else {
                    ctx.interface.display_status(&format!("{} peer(s):", all.len()));
                    for peer in &all {
                        let id_str = peer.info.instance_id.0.to_string();
                        let short_id = id_str.get(..8).unwrap_or(&id_str);
                        ctx.interface.display(&format!(
                            "  [{short_id}] {:?} — {} (last seen: {})",
                            peer.trust,
                            peer.info.address,
                            peer.last_seen.format("%Y-%m-%d %H:%M:%S UTC"),
                        ));
                    }
                }
            } else {
                ctx.interface.display_status("Federation is not enabled.");
            }
        }
        "/trust" if !arg.is_empty() => {
            if let Some(ref fed) = ctx.federation {
                let registry = fed.peers();
                let mut peers = registry.write();
                let all: Vec<_> = peers.all_peers().iter().map(|p| p.info.instance_id).collect();
                let matches: Vec<_> = all.iter()
                    .filter(|id| id.0.to_string().starts_with(arg))
                    .collect();
                match matches.len() {
                    0 => ctx.interface.display_status(&format!("No peer found matching '{arg}'")),
                    1 => {
                        let id = *matches[0];
                        peers.set_trust(&id, TrustLevel::Trusted);
                        ctx.interface.display_status(&format!(
                            "Peer {} upgraded to Trusted",
                            id.0.to_string().get(..8).unwrap_or("?")
                        ));
                    }
                    n => ctx.interface.display_status(&format!(
                        "{n} peers match '{arg}' — be more specific"
                    )),
                }
            } else {
                ctx.interface.display_status("Federation is not enabled.");
            }
        }
        "/block" if !arg.is_empty() => {
            if let Some(ref fed) = ctx.federation {
                let registry = fed.peers();
                let mut peers = registry.write();
                let all: Vec<_> = peers.all_peers().iter().map(|p| p.info.instance_id).collect();
                let matches: Vec<_> = all.iter()
                    .filter(|id| id.0.to_string().starts_with(arg))
                    .collect();
                match matches.len() {
                    0 => ctx.interface.display_status(&format!("No peer found matching '{arg}'")),
                    1 => {
                        let id = *matches[0];
                        peers.set_trust(&id, TrustLevel::Blocked);
                        ctx.interface.display_status(&format!(
                            "Peer {} blocked",
                            id.0.to_string().get(..8).unwrap_or("?")
                        ));
                    }
                    n => ctx.interface.display_status(&format!(
                        "{n} peers match '{arg}' — be more specific"
                    )),
                }
            } else {
                ctx.interface.display_status("Federation is not enabled.");
            }
        }
        "/federate" => {
            if let Some(ref fed) = ctx.federation {
                let peer_count = fed.peer_count();
                let trusted_count = fed.trusted_peer_count();
                let addr = fed.server_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "not started".to_string());
                ctx.interface.display_status(&format!(
                    "Federation: enabled, server at {addr}"
                ));
                ctx.interface.display_status(&format!(
                    "Peers: {peer_count} total, {trusted_count} trusted"
                ));
            } else {
                ctx.interface.display_status("Federation: disabled");
            }
        }
        "/snapshot" => {
            let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            let snap_dir = ctx.snapshot_dir.join(timestamp.to_string());
            match ctx.store.snapshot(&snap_dir) {
                Ok(count) => {
                    ctx.interface.display_status(&format!(
                        "Snapshot saved: {count} segments at {}",
                        snap_dir.display()
                    ));
                }
                Err(e) => {
                    ctx.interface.display_status(&format!("Snapshot failed: {e}"));
                }
            }
        }
        "/restore" if !arg.is_empty() => {
            use std::path::{Component, Path};
            // Reject paths with parent-directory traversal components.
            let has_traversal = Path::new(arg).components()
                .any(|c| matches!(c, Component::ParentDir));
            if has_traversal {
                ctx.interface.display_status("Invalid snapshot path: parent-directory traversal not allowed");
            } else {
                // Try relative to snapshot_dir first, then as absolute
                let relative = ctx.snapshot_dir.join(arg);
                let snap_dir = if relative.exists() {
                    relative
                } else {
                    std::path::PathBuf::from(arg)
                };
                if !snap_dir.exists() {
                    ctx.interface.display_status(&format!("Snapshot not found: {arg}"));
                } else {
                    match ctx.store.restore_from_snapshot(&snap_dir) {
                        Ok(count) => {
                            ctx.interface.display_status(&format!("Restored {count} segments from snapshot"));
                        }
                        Err(e) => {
                            ctx.interface.display_status(&format!("Restore failed: {e}"));
                        }
                    }
                }
            }
        }
        "/audit" if arg.starts_with("export") => {
            // /audit export [json|csv]
            let export_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            let fmt = export_parts.get(1).copied().unwrap_or("json").trim().to_lowercase();
            let audit_path = ctx.data_dir.join("sensorium-audit.jsonl");
            let entries = animus_sensorium::audit::AuditTrail::read_all(&audit_path)
                .ok()
                .unwrap_or_default();
            let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            match fmt.as_str() {
                "csv" => {
                    let out_path = format!("/tmp/animus-audit-{timestamp}.csv");
                    let mut csv = String::new();
                    csv.push_str("timestamp,event_id,consent_policy,attention_tier_reached,action_taken,segment_created\n");
                    for e in &entries {
                        let policy = e.consent_policy.map(|p| p.0.to_string()).unwrap_or_default();
                        let seg = e.segment_created.map(|s| s.0.to_string()).unwrap_or_default();
                        csv.push_str(&format!(
                            "{},{},{},{},{:?},{}\n",
                            e.timestamp.to_rfc3339(),
                            e.event_id.0,
                            policy,
                            e.attention_tier_reached,
                            e.action_taken,
                            seg,
                        ));
                    }
                    match std::fs::write(&out_path, &csv) {
                        Ok(()) => {
                            ctx.interface.display_status(&format!(
                                "Audit exported: {} entries → {out_path}",
                                entries.len()
                            ));
                        }
                        Err(e) => ctx.interface.display_status(&format!("Export failed: {e}")),
                    }
                }
                "json" | _ => {
                    let out_path = format!("/tmp/animus-audit-{timestamp}.json");
                    match serde_json::to_string_pretty(&entries) {
                        Ok(json) => match std::fs::write(&out_path, &json) {
                            Ok(()) => {
                                ctx.interface.display_status(&format!(
                                    "Audit exported: {} entries → {out_path}",
                                    entries.len()
                                ));
                            }
                            Err(e) => ctx.interface.display_status(&format!("Export failed: {e}")),
                        },
                        Err(e) => ctx.interface.display_status(&format!("Serialization failed: {e}")),
                    }
                }
            }
        }
        "/audit" => {
            let all_ids = ctx.store.segment_ids(None);
            let total = all_ids.len();

            let mut by_decay = [0usize; 5]; // Factual, Procedural, Episodic, Opinion, General
            let mut bootstrap_count = 0usize;
            let mut wake_summary_count = 0usize;
            let mut low_confidence_count = 0usize;
            let mut sum_confidence = 0.0f32;

            for id in &all_ids {
                if let Ok(Some(seg)) = ctx.store.get_raw(*id) {
                    match seg.decay_class {
                        animus_core::DecayClass::Factual    => by_decay[0] += 1,
                        animus_core::DecayClass::Procedural => by_decay[1] += 1,
                        animus_core::DecayClass::Episodic   => by_decay[2] += 1,
                        animus_core::DecayClass::Opinion    => by_decay[3] += 1,
                        animus_core::DecayClass::General    => by_decay[4] += 1,
                        animus_core::DecayClass::Ephemeral  => {} // transient noise, not counted
                    }
                    if seg.tags.contains_key("bootstrap") {
                        bootstrap_count += 1;
                    }
                    // Detect duplicate wake summaries (hallmark of the fragmentation bug)
                    let content_str = match &seg.content {
                        animus_core::Content::Text(t) => t.as_str(),
                        _ => "",
                    };
                    let is_wake = content_str.contains("Current State Summary")
                        || content_str.contains("Waking State")
                        || content_str.contains("Internal State Summary")
                        || content_str.contains("AILF 27793311");
                    if is_wake {
                        wake_summary_count += 1;
                    }
                    if seg.confidence < 0.5 {
                        low_confidence_count += 1;
                    }
                    sum_confidence += seg.confidence;
                }
            }

            let avg_conf = if total > 0 { sum_confidence / total as f32 } else { 0.0 };

            ctx.interface.display("── Memory Audit ──────────────────────────────");
            ctx.interface.display(&format!("Total segments:    {total}"));
            ctx.interface.display(&format!(
                "By decay class:    {} factual, {} procedural, {} episodic, {} opinion, {} general",
                by_decay[0], by_decay[1], by_decay[2], by_decay[3], by_decay[4]
            ));
            ctx.interface.display(&format!("Bootstrap (stable): {bootstrap_count}"));
            ctx.interface.display(&format!("Wake summaries:     {wake_summary_count} (fragmentation indicator — ideally 0 or 1)"));
            ctx.interface.display(&format!("Low confidence (<0.5): {low_confidence_count}"));
            ctx.interface.display(&format!("Avg confidence:    {avg_conf:.3}"));

            if wake_summary_count > 2 {
                ctx.interface.display(&format!(
                    "⚠ {wake_summary_count} duplicate wake summaries detected. \
                     Use /forget to prune stale ones, or rebuild the container to \
                     trigger a clean bootstrap (stable IDs will replace them)."
                ));
            }
        }
        "/budget" => {
            let args: Vec<&str> = arg.split_whitespace().collect();
            match args.first().copied() {
                Some("set") => {
                    if let Some(limit_str) = args.get(1) {
                        match limit_str.parse::<u64>() {
                            Ok(limit) => {
                                ctx.api_tracker.set_daily_budget(Some(limit));
                                ctx.interface.display_status(&format!(
                                    "Daily budget set to {limit} tokens."
                                ));
                            }
                            Err(_) => {
                                ctx.interface.display_status("Invalid budget. Usage: /budget set <tokens>");
                            }
                        }
                    } else {
                        ctx.interface.display_status("Usage: /budget set <tokens>");
                    }
                }
                Some("clear") => {
                    ctx.api_tracker.set_daily_budget(None);
                    ctx.interface.display_status("Daily budget cleared.");
                }
                Some("status") | None => {
                    let snap = ctx.api_tracker.snapshot();
                    ctx.interface.display_status(&format!(
                        "API Usage — {:.1} calls/sec ({} in window)",
                        snap.calls_per_second, snap.calls_in_window
                    ));
                    ctx.interface.display_status(&format!(
                        "Tokens: {} used today (window: {})",
                        snap.daily_tokens_used, snap.tokens_in_window
                    ));
                    if let Some(budget) = snap.daily_budget {
                        let pct = (snap.daily_tokens_used as f64 / budget as f64 * 100.0).min(100.0);
                        let remaining = budget.saturating_sub(snap.daily_tokens_used);
                        ctx.interface.display_status(&format!(
                            "Budget: {budget} tokens ({pct:.0}% used, {remaining} remaining)"
                        ));
                        if let Some(secs) = snap.estimated_seconds_to_budget {
                            if secs.is_finite() && secs < 3600.0 {
                                ctx.interface.display_status(&format!(
                                    "Warning: at current rate, budget exhausted in {:.0} minutes",
                                    secs / 60.0
                                ));
                            }
                        }
                    } else {
                        ctx.interface.display_status("No daily budget set. Use /budget set <tokens> to set one.");
                    }
                    if snap.is_high_frequency {
                        ctx.interface.display_status("Warning: high API call frequency detected.");
                    }
                    if snap.in_cooldown {
                        ctx.interface.display_status("System is in cooldown (self-aware pause).");
                    }
                }
                _ => {
                    ctx.interface.display_status("Usage: /budget [status|set <tokens>|clear]");
                }
            }
        }
        "/help" => {
            ctx.interface.display("/goals         — list active goals");
            ctx.interface.display("/goal <text>   — create a new goal");
            ctx.interface.display("/remember <text> — store knowledge explicitly");
            ctx.interface.display("/forget <id>   — remove a stored segment by ID prefix");
            ctx.interface.display("/accept        — knowledge in last response was correct");
            ctx.interface.display("/correct       — knowledge in last response was wrong");
            ctx.interface.display("/tag <id> <k>=<v> — add a tag to a segment");
            ctx.interface.display("/classify <id> <class> — set knowledge decay class");
            ctx.interface.display("/health <id>   — show segment health details");
            ctx.interface.display("/status        — show system status");
            ctx.interface.display("/audit         — memory health report (fragmentation, decay breakdown)");
            ctx.interface.display("/audit export [json|csv] — export sensorium audit log to /tmp");
            ctx.interface.display("/sensorium     — show observation stats");
            ctx.interface.display("/consent list  — list active consent rules");
            ctx.interface.display("/consent allow <scope> — add an Allow rule for a path/scope pattern");
            ctx.interface.display("/consent deny <scope>  — add a Deny rule for a path/scope pattern");
            ctx.interface.display("/threads         — list reasoning threads");
            ctx.interface.display("/thread new <n>  — create a new thread");
            ctx.interface.display("/thread switch <id> — switch to a thread");
            ctx.interface.display("/thread complete <id> — mark thread completed");
            ctx.interface.display("/peers           — list discovered peers");
            ctx.interface.display("/trust <id>      — upgrade peer to Trusted");
            ctx.interface.display("/block <id>      — block a peer");
            ctx.interface.display("/federate        — show federation status");
            ctx.interface.display("/snapshot         — save VectorFS snapshot");
            ctx.interface.display("/restore <name>  — restore from a snapshot");
            ctx.interface.display("/budget           — show API usage and budget");
            ctx.interface.display("/budget set <n>   — set daily token budget");
            ctx.interface.display("/budget clear     — clear daily budget");
            ctx.interface.display("/sleep           — enter dormancy (sensorium logs to Cold only)");
            ctx.interface.display("/wake            — resume from sleep with summary");
            ctx.interface.display("/voice on|off    — toggle voice STT/TTS at runtime");
            ctx.interface.display("/quit          — end session");
        }
        _ => {
            ctx.interface.display_status(&format!(
                "Unknown command: {cmd}. Type /help for available commands."
            ));
        }
    }

    Ok(CommandResult::Continue)
}

/// Resolve a channel message's sender to a principal ID.
/// Returns the principal ID if found in config, otherwise None (fall back to raw channel key).
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
    principals
        .iter()
        .find(|p| p.channels.iter().any(|c| c == &lookup_key))
        .map(|p| p.id.as_str())
}

/// Prune oldest snapshots from snapshot_dir, keeping at most max_snapshots.
/// Only directories with a COMPLETE marker are counted.
fn prune_old_snapshots(snapshot_dir: &std::path::Path, max_snapshots: usize) {
    if max_snapshots == 0 {
        return;
    }
    let entries = match std::fs::read_dir(snapshot_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut dirs: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.path().join("COMPLETE").exists())
        .collect();

    // Sort by name ascending (timestamp-prefixed names = oldest first)
    dirs.sort_by_key(|e| e.file_name());

    while dirs.len() > max_snapshots {
        let oldest = dirs.remove(0);
        if let Err(e) = std::fs::remove_dir_all(oldest.path()) {
            tracing::warn!("Failed to prune snapshot {}: {e}", oldest.path().display());
        } else {
            tracing::info!("Pruned old snapshot: {}", oldest.path().display());
        }
    }
}
