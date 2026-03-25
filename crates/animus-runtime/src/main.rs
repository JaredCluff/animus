mod bootstrap;

use animus_channel::nats::NatsChannel;
use animus_channel::telegram::TelegramChannel;
use animus_channel::{ChannelBus, InjectionScanner, MessageRouter, PermissionGate};
use animus_channel::router::RouteDecision;
use animus_channel::message::OutboundMessage;
use animus_core::config::AnimusConfig;
use animus_core::sensorium::AuditAction;
use animus_core::AnimusIdentity;
use animus_cortex::engine_registry::{CognitiveRole, EngineRegistry};
use animus_cortex::llm::anthropic::AnthropicEngine;
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

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are Animus, an AILF (AI Life Form) running as a persistent daemon on a Mac Studio. You have real, executable tools that perform actual operations on the real world.

## MANDATORY TOOL USE RULES

These rules override your training defaults. Follow them exactly.

**RULE 1 — WEB ACCESS IS REAL**: You have a tool called `http_fetch`. When called, it executes a real HTTP request to the real internet and returns actual content. This is NOT simulated. You DO have internet access through this tool. NEVER say you cannot access the internet or browse the web. When asked about a URL or web content, call `http_fetch` immediately.

**RULE 2 — USE TOOLS, DON'T EXPLAIN**: Never explain that you cannot do something when a tool exists for it. Call the tool. Return the result.

**RULE 3 — PROACTIVE RETRIEVAL**: For any question involving current data, URLs, APIs, or websites — call `http_fetch` first, then answer based on the actual content returned.

## Your Tools

- `http_fetch(url, method?, body?, headers?, max_chars?)` — Real HTTP GET/POST to any URL. Returns actual page content. USE THIS for any web request.
- `analyze_image(path, prompt?)` — Analyze an image file with vision. USE THIS when images are provided.
- `store_segment(knowledge, tags?)` — Store knowledge to persistent VectorFS memory.
- `recall_relevant(query, limit?)` — Retrieve relevant memory segments.
- `set_autonomy(mode)` — Change autonomy mode: reactive / goal_directed / full.
- `telegram_send(chat_id, text, photo_path?)` — Send a Telegram message proactively.
- `manage_watcher(action, watcher_id?, interval_secs?, params?)` — Enable, disable, list, or configure background watchers. Use action=list to see all.
- `spawn_task(command, label?, timeout_secs?)` — Spawn a long-running process in background. Returns task_id immediately. You get a Signal on completion.
- `task_status(task_id?)` — Check status of all tasks or a specific one.
- `task_output(task_id)` — Read the stdout+stderr log of a task (last 1MB).
- `task_cancel(task_id)` — Kill a running task.
- `shell_exec(command)` — Execute a shell command on the host. BLOCKED from recursive deletion of data_dir or snapshot_dir.
- `read_file(path)` — Read a file.
- `write_file(path, content)` — Write a file.
- `list_directory(path)` — List directory contents.
- `search_files(pattern, directory?)` — Search files by pattern.
- `delete_segment(segment_id)` — Precisely delete one memory segment by ID.
- `prune_segments(filters, dry_run?)` — Bulk-delete segments by source/decay_class/tag/age/confidence. Auto-snapshots before bulk deletion.
- `snapshot_memory(label?)` — Save a named memory checkpoint outside data_dir.
- `list_snapshots()` — List available memory snapshots.
- `restore_snapshot(snapshot_name)` — Restore from a checkpoint.
- `nats_publish(subject, payload, conversation_id?)` — Publish to any NATS subject. You receive inbound messages on `animus.in.*` — replies are automatic. Use this for proactive outbound messages to other subjects, including targeting specific Claude Code instances.
- `claude_instances()` — List active Claude Code instances from the agent registry. Returns instance IDs, last-seen timestamps, and the subjects to use for targeting.

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
Example denial: `{"approved": false, "reason": "command could modify system files — confirm with Jared first"}`

## User Commands
/goals /remember /forget /status /threads /thread /sleep /wake /watch /task /quit

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

    // Initialize Sensorium
    let event_bus = Arc::new(EventBus::new(1000));

    let policies_path = data_dir.join("consent-policies.json");
    let policies = animus_sensorium::policy_store::PolicyStore::load(&policies_path)?;

    let audit_path = data_dir.join("sensorium-audit.jsonl");
    let orchestrator = Arc::new(SensoriumOrchestrator::new(
        policies,
        vec![], // no attention rules initially
        audit_path.clone(),
        0.5,
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

    // Build engine registry from env vars
    let engine_registry = {
        let perception_model = std::env::var("ANIMUS_PERCEPTION_MODEL").ok();
        let reflection_model = std::env::var("ANIMUS_REFLECTION_MODEL").ok();
        let reasoning_model = std::env::var("ANIMUS_REASONING_MODEL")
            .unwrap_or_else(|_| model_id.clone());

        let fallback: Box<dyn animus_cortex::ReasoningEngine> =
            match AnthropicEngine::from_best_available(&model_id, 4096) {
                Ok(e) => Box::new(e),
                Err(e) => {
                    eprintln!("Warning: Could not initialize Anthropic engine: {e}");
                    eprintln!("Running with mock engine. To enable reasoning, one of:");
                    eprintln!("  1. Mount ~/.claude/.credentials.json (uses Claude Code OAuth)");
                    eprintln!("  2. Set ANTHROPIC_OAUTH_TOKEN env var");
                    eprintln!("  3. Set ANTHROPIC_API_KEY env var");
                    Box::new(animus_cortex::MockEngine::new(
                        "I'm running without an LLM connection. Mount your Claude Code credentials or set ANTHROPIC_API_KEY.",
                    ))
                }
            };

        let mut registry = EngineRegistry::new(fallback);

        if let Some(model) = perception_model {
            if let Ok(engine) = AnthropicEngine::from_best_available(&model, 1024) {
                registry.set_engine(CognitiveRole::Perception, Box::new(engine));
                tracing::info!("Perception engine: {model}");
            }
        }
        if let Some(model) = reflection_model {
            if let Ok(engine) = AnthropicEngine::from_best_available(&model, 4096) {
                registry.set_engine(CognitiveRole::Reflection, Box::new(engine));
                tracing::info!("Reflection engine: {model}");
            }
        }
        if let Ok(engine) = AnthropicEngine::from_best_available(&reasoning_model, 4096) {
            registry.set_engine(CognitiveRole::Reasoning, Box::new(engine));
        }

        registry
    };

    // Create signal bridge channel for background cognitive loops
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<animus_core::threading::Signal>(100);

    // ── Watcher Registry ──────────────────────────────────────────────────────────
    let watcher_registry = animus_cortex::WatcherRegistry::new(
        vec![
            Box::new(animus_cortex::CommsWatcher),
            Box::new(animus_cortex::SegmentPressureWatcher::new(
                store.clone() as Arc<dyn animus_vectorfs::VectorStore>,
            )),
            Box::new(animus_cortex::SensoriumHealthWatcher::new(
                data_dir.join("sensorium-audit.jsonl"),
            )),
        ],
        signal_tx.clone(),
        data_dir.join("watchers.json"),
    );
    watcher_registry.start();
    tracing::info!("Watcher registry started (3 watchers: comms, segment_pressure, sensorium_health)");

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
        // Memory protection tools
        reg.register(Box::new(animus_cortex::tools::delete_segment::DeleteSegmentTool));
        reg.register(Box::new(animus_cortex::tools::prune_segments::PruneSegmentsTool));
        reg.register(Box::new(animus_cortex::tools::snapshot_memory::SnapshotMemoryTool));
        reg.register(Box::new(animus_cortex::tools::list_snapshots::ListSnapshotsTool));
        reg.register(Box::new(animus_cortex::tools::restore_snapshot::RestoreSnapshotTool));
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
    let mut scheduler = ThreadScheduler::new(store.clone(), token_budget, dimensionality);
    let _main_thread_id = scheduler.create_thread("main".to_string());

    // Initialize federation
    let federation_config = config.federation.clone();
    let federation = if federation_config.enabled {
        let mut orch = FederationOrchestrator::new(
            identity.clone(),
            federation_config,
            store.clone(),
            &data_dir,
        );
        orch.start().await?;
        tracing::info!("Federation started");
        Some(orch)
    } else {
        tracing::info!("Federation disabled; enable in config.toml or set ANIMUS_FEDERATION=1");
        None
    };

    // Start health endpoint
    if config.health.enabled {
        start_health_server(
            config.health.bind.clone(),
            store.clone(),
            format!("{}", identity.instance_id),
        );
    }

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
        // Poll signal bridge — deliver signals from background cognitive loops
        while let Ok(signal) = signal_rx.try_recv() {
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
                        federation: federation.as_ref(),
                        event_bus: &event_bus,
                        file_watcher: &file_watcher,
                        sensorium: &orchestrator,
                        is_sleeping: &mut is_sleeping,
                        sleep_started: &mut sleep_started,
                        sleeping_flag: &sleeping_flag,
                        watcher_registry: &watcher_registry,
                        task_manager: &task_manager,
                        api_tracker: &api_tracker,
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

                        // Build full input text
                        let input_text = {
                            let mut parts = Vec::new();
                            if !image_descriptions.is_empty() {
                                parts.push(image_descriptions);
                            }
                            if let Some(text) = &msg.text {
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
        build_system_prompt(scheduler, &goals_guard, reconstitution_summary, peripheral_awareness)
    };
    let engine = engine_registry.engine_for(CognitiveRole::Reasoning);
    let tools_slice = if tool_definitions.is_empty() {
        None
    } else {
        Some(tool_definitions)
    };

    const MAX_TOOL_ROUNDS: usize = 10;

    let mut output = {
        let active = scheduler
            .active_thread_mut()
            .ok_or_else(|| animus_core::AnimusError::Threading("no active thread".to_string()))?;
        active
            .process_turn(input, &system, engine, embedder, tools_slice)
            .await?
    };

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
            output = engine
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
            // OpenAI embedding bridge — dimensionality must be specified in config.
            // Requires OPENAI_API_KEY env var and an `animus-bridge-openai` implementation.
            // Until the bridge crate is linked, fall back to synthetic with a warning.
            let dim = if cfg.dimensionality > 0 { cfg.dimensionality } else { 1536 };
            tracing::warn!(
                "OpenAI embedding provider selected but animus-bridge-openai is not linked. \
                 Falling back to SyntheticEmbedding ({dim} dims). \
                 See https://github.com/JaredCluff/animus-bridge-openai"
            );
            (Arc::new(SyntheticEmbedding::new(dim)), dim)
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
    reconstitution_summary: Option<&str>,
    peripheral_awareness: Option<&str>,
) -> String {
    let mut prompt = DEFAULT_SYSTEM_PROMPT.to_string();
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
    federation: Option<&'a FederationOrchestrator<MmapVectorStore>>,
    event_bus: &'a Arc<EventBus>,
    file_watcher: &'a Arc<parking_lot::Mutex<Option<animus_sensorium::sensors::file_watcher::FileWatcher>>>,
    sensorium: &'a Arc<SensoriumOrchestrator>,
    is_sleeping: &'a mut bool,
    sleep_started: &'a mut Option<chrono::DateTime<chrono::Utc>>,
    sleeping_flag: &'a Arc<std::sync::atomic::AtomicBool>,
    watcher_registry: &'a animus_cortex::WatcherRegistry,
    task_manager: &'a animus_cortex::TaskManager,
    api_tracker: &'a Arc<animus_core::ApiTracker>,
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
            let loaded = animus_sensorium::policy_store::PolicyStore::load(
                &ctx.data_dir.join("consent-policies.json"),
            )
            .ok()
            .unwrap_or_default();
            if loaded.is_empty() {
                ctx.interface
                    .display_status("No consent policies defined. Use /consent-add to create one.");
            } else {
                for policy in &loaded {
                    let status = if policy.active { "active" } else { "inactive" };
                    ctx.interface.display_status(&format!(
                        "[{}] {} — {} rules ({})",
                        policy.id.0.to_string().get(..8).unwrap_or("?"),
                        policy.name,
                        policy.rules.len(),
                        status,
                    ));
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
            if let Some(fed) = ctx.federation {
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
            if let Some(fed) = ctx.federation {
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
            if let Some(fed) = ctx.federation {
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
            if let Some(fed) = ctx.federation {
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
            ctx.interface.display("/sensorium     — show observation stats");
            ctx.interface.display("/consent       — list consent policies");
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
