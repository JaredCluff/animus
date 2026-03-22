use animus_core::sensorium::AuditAction;
use animus_core::AnimusIdentity;
use animus_cortex::llm::anthropic::AnthropicEngine;
use animus_cortex::telos::{GoalManager, GoalSource, Priority};
use animus_cortex::thread::ReasoningThread;
use animus_cortex::ReasoningEngine;
use animus_embed::SyntheticEmbedding;
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

Available commands the human can use:
- /goals — show active goals
- /remember <text> — explicitly store knowledge
- /forget <id> — remove a stored segment
- /status — show system status
- /sensorium — show observation statistics
- /consent — list consent policies
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

    // Initialize VectorFS
    let vectorfs_dir = data_dir.join("vectorfs");
    let dimensionality = 128;
    let store = Arc::new(MmapVectorStore::open(&vectorfs_dir, dimensionality)?);
    let segment_count = store.count(None);

    // Initialize embedding service
    let embedder = SyntheticEmbedding::new(dimensionality);

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
    )?);

    // Start background event processing loop
    let orch_clone = orchestrator.clone();
    let store_clone = store.clone();
    let dim = dimensionality;
    let mut bus_rx = event_bus.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = bus_rx.recv().await {
            match orch_clone.process_event(event.clone()).await {
                Ok(outcome) if outcome.passed_attention => {
                    let embedding = vec![0.0f32; dim]; // synthetic placeholder
                    let segment = animus_core::Segment::new(
                        animus_core::Content::Structured(event.data.clone()),
                        embedding,
                        animus_core::Source::Observation {
                            event_type: format!("{:?}", event.event_type),
                            raw_event_id: event.id,
                        },
                    );
                    if let Err(e) = store_clone.store(segment) {
                        tracing::warn!("Failed to store observation: {e}");
                    }
                }
                Ok(_) => {} // filtered out — expected
                Err(e) => tracing::warn!("Sensorium processing error: {e}"),
            }
        }
    });

    // File watcher is not started by default; enabled via /watch command in future updates
    let _event_bus = event_bus; // retain for future use
    let _orchestrator = orchestrator; // retain for future use

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

    // Initialize goal manager
    let goals_path = data_dir.join("goals.bin");
    let mut goals = GoalManager::load(&goals_path)?;

    // Initialize reasoning thread
    let token_budget = 8000;
    let mut thread = ReasoningThread::new(
        "main".to_string(),
        store.clone(),
        token_budget,
        dimensionality,
    );

    // Initialize terminal interface
    let interface = TerminalInterface::new(">> ".to_string());
    let instance_str = format!("{}", identity.instance_id);
    interface.display_banner(instance_str.get(..8).unwrap_or(&instance_str), engine.model_name(), segment_count);

    // Main conversation loop
    loop {
        let input = match interface.read_input()? {
            Some(input) if input.is_empty() => continue,
            Some(input) => input,
            None => break, // EOF
        };

        // Handle slash commands
        if input.starts_with('/') {
            match handle_command(
                &input,
                &store,
                &mut goals,
                &goals_path,
                &interface,
                &embedder,
                &data_dir,
            )
            .await?
            {
                CommandResult::Continue => continue,
                CommandResult::Quit => break,
            }
        }

        // Process through reasoning thread
        let system = build_system_prompt(&goals);
        match thread
            .process_turn(&input, &system, engine.as_ref(), &embedder)
            .await
        {
            Ok(response) => {
                interface.display_response(&response);
            }
            Err(e) => {
                interface.display_status(&format!("Error: {e}"));
            }
        }
    }

    // Persist state before exit
    goals.save(&goals_path)?;
    store.flush()?;
    interface.display_status("Session ended. Memory persisted.");

    Ok(())
}

fn build_system_prompt(goals: &GoalManager) -> String {
    let mut prompt = DEFAULT_SYSTEM_PROMPT.to_string();
    let goals_summary = goals.goals_summary();
    if !goals_summary.is_empty() {
        prompt.push_str("\n\n## Current Goals\n");
        prompt.push_str(&goals_summary);
    }
    prompt
}

enum CommandResult {
    Continue,
    Quit,
}

async fn handle_command(
    input: &str,
    store: &Arc<MmapVectorStore>,
    goals: &mut GoalManager,
    goals_path: &std::path::Path,
    interface: &TerminalInterface,
    embedder: &dyn animus_core::EmbeddingService,
    data_dir: &std::path::Path,
) -> animus_core::Result<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");

    match cmd {
        "/quit" | "/exit" | "/q" => {
            return Ok(CommandResult::Quit);
        }
        "/status" => {
            let total = store.count(None);
            let warm = store.count(Some(animus_core::Tier::Warm));
            let cold = store.count(Some(animus_core::Tier::Cold));
            let hot = store.count(Some(animus_core::Tier::Hot));
            interface.display_status(&format!(
                "Segments: {total} total ({hot} hot, {warm} warm, {cold} cold)"
            ));
            interface.display_status(&format!("Goals: {} active", goals.active_goals().len()));
        }
        "/goals" => {
            let active = goals.active_goals();
            if active.is_empty() {
                interface.display_status("No active goals.");
            } else {
                for goal in active {
                    interface.display_status(&format!(
                        "[{:?}] {} ({})",
                        goal.priority,
                        goal.description,
                        goal.id.0.to_string().get(..8).unwrap_or("?")
                    ));
                }
            }
        }
        "/goal" if !arg.is_empty() => {
            let id = goals.create_goal(arg.to_string(), GoalSource::Human, Priority::Normal);
            goals.save(goals_path)?;
            interface.display_status(&format!(
                "Goal created: {}",
                id.0.to_string().get(..8).unwrap_or("?")
            ));
        }
        "/remember" if !arg.is_empty() => {
            use animus_core::segment::{Content, Segment, Source};
            use animus_core::EventId;
            let embedding = embedder.embed_text(arg).await?;
            let segment = Segment::new(
                Content::Text(arg.to_string()),
                embedding,
                Source::Observation {
                    event_type: "user-remember".to_string(),
                    raw_event_id: EventId::new(),
                },
            );
            let id = store.store(segment)?;
            interface.display_status(&format!(
                "Remembered: {} (segment {})",
                arg,
                id.0.to_string().get(..8).unwrap_or("?")
            ));
        }
        "/forget" if !arg.is_empty() => {
            // Match segment by ID prefix
            let all_ids = store.segment_ids(None);
            let matches: Vec<_> = all_ids
                .iter()
                .filter(|id| id.0.to_string().starts_with(arg))
                .collect();
            match matches.len() {
                0 => interface.display_status(&format!("No segment found matching '{arg}'")),
                1 => {
                    let id = *matches[0];
                    store.delete(id)?;
                    interface.display_status(&format!(
                        "Forgotten: segment {}",
                        id.0.to_string().get(..8).unwrap_or("?")
                    ));
                }
                n => interface.display_status(&format!(
                    "{n} segments match '{arg}' — be more specific"
                )),
            }
        }
        "/sensorium" => {
            let audit_entries = animus_sensorium::audit::AuditTrail::read_all(
                &data_dir.join("sensorium-audit.jsonl"),
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
            interface.display_status(&format!(
                "Sensorium: {total} events observed, {permitted} permitted, {promoted} promoted"
            ));
        }
        "/watch" if !arg.is_empty() => {
            let watch_path = std::path::PathBuf::from(arg);
            if !watch_path.exists() {
                interface.display_status(&format!("Path does not exist: {arg}"));
            } else {
                interface.display_status(&format!(
                    "Watch path noted: {arg}. File watching will be available in a future update."
                ));
            }
        }
        "/consent" => {
            let loaded = animus_sensorium::policy_store::PolicyStore::load(
                &data_dir.join("consent-policies.json"),
            )
            .ok()
            .unwrap_or_default();
            if loaded.is_empty() {
                interface
                    .display_status("No consent policies defined. Use /consent-add to create one.");
            } else {
                for policy in &loaded {
                    let status = if policy.active { "active" } else { "inactive" };
                    interface.display_status(&format!(
                        "[{}] {} — {} rules ({})",
                        policy.id.0.to_string().get(..8).unwrap_or("?"),
                        policy.name,
                        policy.rules.len(),
                        status,
                    ));
                }
            }
        }
        "/help" => {
            interface.display("/goals         — list active goals");
            interface.display("/goal <text>   — create a new goal");
            interface.display("/remember <text> — store knowledge explicitly");
            interface.display("/forget <id>   — remove a stored segment by ID prefix");
            interface.display("/status        — show system status");
            interface.display("/sensorium     — show observation stats");
            interface.display("/consent       — list consent policies");
            interface.display("/quit          — end session");
        }
        _ => {
            interface.display_status(&format!(
                "Unknown command: {cmd}. Type /help for available commands."
            ));
        }
    }

    Ok(CommandResult::Continue)
}
