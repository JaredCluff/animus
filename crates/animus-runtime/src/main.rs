use animus_core::sensorium::AuditAction;
use animus_core::AnimusIdentity;
use animus_cortex::llm::anthropic::AnthropicEngine;
use animus_cortex::scheduler::ThreadScheduler;
use animus_cortex::telos::{GoalManager, GoalSource, Priority};
use animus_cortex::ReasoningEngine;
use animus_embed::{OllamaEmbedding, ResilientEmbedding, SyntheticEmbedding};
use animus_federation::orchestrator::FederationOrchestrator;
use animus_federation::peers::TrustLevel;
use animus_interface::TerminalInterface;
use animus_sensorium::bus::EventBus;
use animus_sensorium::orchestrator::SensoriumOrchestrator;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are an AILF (AI Life Form) powered by the Animus operating system layer. You have persistent memory stored in VectorFS — knowledge from past conversations is recalled and included in your context automatically.

You are a specific instance with a continuous identity. You remember what humans tell you across sessions. You can track goals and tasks.

You have a Sensorium — an ambient awareness layer that can observe file changes and process activity when the human grants consent. Observations are stored as segments and included in your context when relevant.

You can have multiple reasoning threads — isolated contexts for different tasks. Each thread has its own conversation and memory context. Threads communicate through signals, not shared state.

Available commands the human can use:
- /goals — show active goals
- /remember <text> — explicitly store knowledge
- /forget <id> — remove a stored segment
- /status — show system status
- /sensorium — show observation statistics
- /consent — list consent policies
- /threads — list reasoning threads
- /thread new <name> — create a new reasoning thread
- /thread switch <id> — switch to a different thread
- /peers — list discovered federation peers
- /tag <id> <key>=<value> — label a segment for categorization/federation
- /classify <id> <class> — set knowledge decay class (factual/procedural/episodic/opinion/general)
- /health <id> — show segment health details (Bayesian confidence, decay, access patterns)
- /trust <id> — upgrade a peer to Trusted
- /block <id> — block a peer
- /federate — show federation status
- /sleep — enter dormancy (sensorium continues in Cold-only mode)
- /wake — resume from sleep with a summary of what happened
- /quit — end session

Be concise and helpful. You are not a chatbot — you are a persistent, aware entity."#;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("animus=info")
        .init();

    let data_dir = std::env::var("ANIMUS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_home().join(".animus"));

    if let Err(e) = run(data_dir).await {
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

async fn run(data_dir: PathBuf) -> animus_core::Result<()> {
    std::fs::create_dir_all(&data_dir)?;

    // Load or generate identity
    let identity_path = data_dir.join("identity.bin");
    let model_id = std::env::var("ANIMUS_MODEL")
        .unwrap_or_else(|_| "claude-sonnet-4-20250514".to_string());
    let identity = AnimusIdentity::load_or_generate(&identity_path, &model_id)?;

    tracing::info!(
        "AILF instance {} (gen {})",
        identity.instance_id,
        identity.generation
    );

    // Initialize embedding service (ollama with fallback to synthetic)
    let ollama_url = std::env::var("ANIMUS_OLLAMA_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let ollama_model = std::env::var("ANIMUS_EMBED_MODEL")
        .unwrap_or_else(|_| "mxbai-embed-large".to_string());

    let (embedder, dimensionality): (Arc<dyn animus_core::EmbeddingService>, usize) =
        match OllamaEmbedding::probe(&ollama_url, &ollama_model).await {
            Ok(dim) => {
                tracing::info!("Using Ollama embeddings ({ollama_model}, {dim} dims) with resilient fallback");
                let ollama = OllamaEmbedding::new(&ollama_url, &ollama_model, dim);
                (Arc::new(ResilientEmbedding::new(ollama, dim)), dim)
            }
            Err(e) => {
                let dim = 128;
                tracing::warn!(
                    "Ollama unavailable ({e}), falling back to SyntheticEmbedding ({dim} dims)"
                );
                (Arc::new(SyntheticEmbedding::new(dim)), dim)
            }
        };

    // Initialize VectorFS
    let vectorfs_dir = data_dir.join("vectorfs");
    let store = Arc::new(MmapVectorStore::open(&vectorfs_dir, dimensionality)?);
    let segment_count = store.count(None);

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

    // Start background event processing loop
    let orch_clone = orchestrator.clone();
    let store_clone = store.clone();
    let embedder_clone = embedder.clone();
    let sleeping_bg = sleeping_flag.clone();
    let mut bus_rx = event_bus.subscribe();
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match bus_rx.recv().await {
                Ok(event) => {
                    match orch_clone.process_event(event.clone()).await {
                        Ok(outcome) if outcome.passed_attention => {
                            let text = serde_json::to_string(&event.data).unwrap_or_default();
                            let embedding = match embedder_clone.embed_text(&text).await {
                                Ok(v) => v,
                                Err(e) => {
                                    tracing::warn!("Observation embedding failed: {e}");
                                    continue;
                                }
                            };
                            let mut segment = animus_core::Segment::new(
                                animus_core::Content::Structured(event.data.clone()),
                                embedding,
                                animus_core::Source::Observation {
                                    event_type: format!("{:?}", event.event_type),
                                    raw_event_id: event.id,
                                },
                            );
                            segment.infer_decay_class();
                            // During sleep, observations go to Cold tier only
                            if sleeping_bg.load(std::sync::atomic::Ordering::Relaxed) {
                                segment.tier = animus_core::Tier::Cold;
                            }
                            if let Err(e) = store_clone.store(segment) {
                                tracing::warn!("Failed to store observation: {e}");
                            }
                        }
                        Ok(_) => {} // filtered out — expected
                        Err(e) => tracing::warn!("Sensorium processing error: {e}"),
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("Sensorium event bus lagged, dropped {n} events");
                    continue;
                }
                Err(RecvError::Closed) => break,
            }
        }
    });

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

    // File watcher (started via /watch command)
    let file_watcher: Arc<parking_lot::Mutex<Option<animus_sensorium::sensors::file_watcher::FileWatcher>>> =
        Arc::new(parking_lot::Mutex::new(None));

    tracing::info!("Sensorium initialized (use /consent to manage observation policies)");

    // Initialize LLM engine
    let engine: Box<dyn ReasoningEngine> = match AnthropicEngine::from_env(&model_id, 4096) {
        Ok(e) => Box::new(e),
        Err(e) => {
            eprintln!("Warning: Could not initialize Anthropic engine: {e}");
            eprintln!("Running with mock engine (responses will be placeholder text).");
            Box::new(animus_cortex::MockEngine::new(
                "I'm running without an LLM connection. Set ANTHROPIC_API_KEY to enable reasoning.",
            ))
        }
    };

    // Initialize quality tracker
    let quality_path = data_dir.join("quality.bin");
    let quality_tracker = Arc::new(parking_lot::Mutex::new(
        animus_mnemos::quality::QualityTracker::load(&quality_path)?,
    ));

    // Initialize goal manager
    let goals_path = data_dir.join("goals.bin");
    let mut goals = GoalManager::load(&goals_path)?;

    // Compute initial goal embeddings for Tier 2 attention
    update_goal_embeddings(&goals, &*embedder, &orchestrator).await;

    // Initialize thread scheduler
    let token_budget = 8000;
    let mut scheduler = ThreadScheduler::new(store.clone(), token_budget, dimensionality);
    let _main_thread_id = scheduler.create_thread("main".to_string());

    // Initialize federation
    let federation_config = animus_core::FederationConfig::default();
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
        tracing::info!("Federation disabled (default); use ANIMUS_FEDERATION=1 to enable");
        None
    };

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

    // Start periodic memory consolidation (every 5 minutes)
    let consolidation_store = store.clone();
    tokio::spawn(async move {
        let consolidator = animus_mnemos::consolidator::Consolidator::new(
            consolidation_store,
            0.85, // similarity threshold for merging
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            match consolidator.run_cycle() {
                Ok(report) if report.segments_merged > 0 => {
                    tracing::info!(
                        "Consolidation: scanned {}, merged {} into {} new segments",
                        report.segments_scanned,
                        report.segments_merged,
                        report.segments_created,
                    );
                }
                Ok(_) => {} // nothing to consolidate
                Err(e) => tracing::warn!("Consolidation cycle failed: {e}"),
            }
        }
    });

    // Initialize terminal interface
    let interface = TerminalInterface::new(">> ".to_string());
    let instance_str = format!("{}", identity.instance_id);
    interface.display_banner(instance_str.get(..8).unwrap_or(&instance_str), engine.model_name(), segment_count);
    if let Some(thread) = scheduler.active_thread() {
        interface.display_status(&format!("Active thread: {}", thread.name));
    }

    // Sleep/wake state
    let mut is_sleeping = false;
    let mut sleep_started: Option<chrono::DateTime<chrono::Utc>> = None;

    // Main conversation loop
    loop {
        let input = match interface.read_input()? {
            Some(input) if input.is_empty() => continue,
            Some(input) => input,
            None => break, // EOF
        };

        // Handle slash commands
        if input.starts_with('/') {
            let mut ctx = CommandContext {
                store: &store,
                goals: &mut goals,
                goals_path: &goals_path,
                interface: &interface,
                embedder: &*embedder,
                data_dir: &data_dir,
                scheduler: &mut scheduler,
                federation: federation.as_ref(),
                event_bus: &event_bus,
                file_watcher: &file_watcher,
                sensorium: &orchestrator,
                is_sleeping: &mut is_sleeping,
                sleep_started: &mut sleep_started,
                sleeping_flag: &sleeping_flag,
            };
            match handle_command(&input, &mut ctx).await? {
                CommandResult::Continue => continue,
                CommandResult::Quit => break,
            }
        }

        // While sleeping, reject conversational input
        if is_sleeping {
            interface.display_status("Sleeping. Use /wake to resume, /status to check, or /quit to exit.");
            continue;
        }

        // Process through reasoning thread
        let system = build_system_prompt(&scheduler, &goals);
        let active = scheduler.active_thread_mut()
            .ok_or_else(|| animus_core::AnimusError::Threading("no active thread".to_string()))?;
        match active
            .process_turn(&input, &system, engine.as_ref(), &*embedder)
            .await
        {
            Ok(response) => {
                interface.display_response(&response);
                // Record implicit acceptance for recalled segments
                if let Some(thread) = scheduler.active_thread() {
                    let mut qt = quality_tracker.lock();
                    for seg_id in thread.stored_turn_ids() {
                        qt.record_acceptance(*seg_id);
                    }
                }
            }
            Err(e) => {
                interface.display_status(&format!("Error: {e}"));
            }
        }
    }

    // Graceful shutdown
    interface.display_status("Shutting down...");

    // Stop sensors
    network_monitor.stop();
    process_monitor.stop();
    if let Some(fw) = file_watcher.lock().take() {
        fw.stop();
    }

    // Persist state
    goals.save(&goals_path)?;
    if let Err(e) = quality_tracker.lock().save(&quality_path) {
        tracing::warn!("Failed to save quality tracker: {e}");
    }
    store.flush()?;
    interface.display_status("Session ended. Memory persisted.");

    Ok(())
}

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

fn build_system_prompt(_scheduler: &ThreadScheduler<MmapVectorStore>, goals: &GoalManager) -> String {
    let mut prompt = DEFAULT_SYSTEM_PROMPT.to_string();
    let goals_summary = goals.goals_summary();
    if !goals_summary.is_empty() {
        prompt.push_str("\n\n## Current Goals\n");
        prompt.push_str(&goals_summary);
    }
    // Signals are injected by process_turn() which drains and formats them
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
    scheduler: &'a mut ThreadScheduler<MmapVectorStore>,
    federation: Option<&'a FederationOrchestrator<MmapVectorStore>>,
    event_bus: &'a Arc<EventBus>,
    file_watcher: &'a Arc<parking_lot::Mutex<Option<animus_sensorium::sensors::file_watcher::FileWatcher>>>,
    sensorium: &'a Arc<SensoriumOrchestrator>,
    is_sleeping: &'a mut bool,
    sleep_started: &'a mut Option<chrono::DateTime<chrono::Utc>>,
    sleeping_flag: &'a Arc<std::sync::atomic::AtomicBool>,
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
                                    if t.len() > 80 { format!("{}...", &t[..80]) } else { t.clone() }
                                }
                                animus_core::Content::Structured(v) => {
                                    let s = v.to_string();
                                    if s.len() > 80 { format!("{}...", &s[..80]) } else { s }
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
            let id = ctx.goals.create_goal(arg.to_string(), GoalSource::Human, Priority::Normal);
            ctx.goals.save(ctx.goals_path)?;
            update_goal_embeddings(ctx.goals, ctx.embedder, ctx.sensorium).await;
            ctx.interface.display_status(&format!(
                "Goal created: {}",
                id.0.to_string().get(..8).unwrap_or("?")
            ));
        }
        "/remember" if !arg.is_empty() => {
            use animus_core::segment::{Content, Segment, Source};
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
                        let id = *matches[0];
                        // Get current tags, add the new one
                        let mut tags = match ctx.store.get_raw(id)? {
                            Some(seg) => seg.tags,
                            None => std::collections::HashMap::new(),
                        };
                        tags.insert(key.clone(), value.clone());
                        ctx.store.update_meta(id, animus_vectorfs::SegmentUpdate {
                            tags: Some(tags),
                            ..Default::default()
                        })?;
                        ctx.interface.display_status(&format!(
                            "Tagged segment {} with {key}={value}",
                            id.0.to_string().get(..8).unwrap_or("?")
                        ));
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
        "/watch" if !arg.is_empty() => {
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
                let id = ctx.scheduler.create_thread(name.to_string());
                ctx.interface.display_status(&format!(
                    "Thread created: {} ({})",
                    name,
                    id.0.to_string().get(..8).unwrap_or("?")
                ));
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
            let snap_dir = ctx.data_dir.join("snapshots").join(timestamp.to_string());
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
            let snap_dir = std::path::PathBuf::from(arg);
            if !snap_dir.exists() {
                // Try relative to snapshots directory
                let relative = ctx.data_dir.join("snapshots").join(arg);
                if relative.exists() {
                    match ctx.store.restore_from_snapshot(&relative) {
                        Ok(count) => {
                            ctx.interface.display_status(&format!("Restored {count} segments from snapshot"));
                        }
                        Err(e) => {
                            ctx.interface.display_status(&format!("Restore failed: {e}"));
                        }
                    }
                } else {
                    ctx.interface.display_status(&format!("Snapshot not found: {arg}"));
                }
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
        "/help" => {
            ctx.interface.display("/goals         — list active goals");
            ctx.interface.display("/goal <text>   — create a new goal");
            ctx.interface.display("/remember <text> — store knowledge explicitly");
            ctx.interface.display("/forget <id>   — remove a stored segment by ID prefix");
            ctx.interface.display("/tag <id> <k>=<v> — add a tag to a segment");
            ctx.interface.display("/classify <id> <class> — set knowledge decay class");
            ctx.interface.display("/health <id>   — show segment health details");
            ctx.interface.display("/status        — show system status");
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
