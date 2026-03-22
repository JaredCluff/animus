use animus_core::AnimusIdentity;
use animus_cortex::llm::anthropic::AnthropicEngine;
use animus_cortex::telos::{GoalManager, GoalSource, Priority};
use animus_cortex::thread::ReasoningThread;
use animus_cortex::ReasoningEngine;
use animus_embed::SyntheticEmbedding;
use animus_interface::TerminalInterface;
use animus_vectorfs::store::MmapVectorStore;
use animus_vectorfs::VectorStore;
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are an AILF (AI Life Form) powered by the Animus operating system layer. You have persistent memory stored in VectorFS — knowledge from past conversations is recalled and included in your context automatically.

You are a specific instance with a continuous identity. You remember what humans tell you across sessions. You can track goals and tasks.

Available commands the human can use:
- /goals — show active goals
- /remember <text> — explicitly store knowledge
- /forget <id> — remove a stored segment
- /status — show system status
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
        "/help" => {
            interface.display("/goals         — list active goals");
            interface.display("/goal <text>   — create a new goal");
            interface.display("/remember <text> — store knowledge explicitly");
            interface.display("/forget <id>   — remove a stored segment by ID prefix");
            interface.display("/status        — show system status");
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
