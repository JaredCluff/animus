/// Startup bootstrap — writes self-knowledge into VectorFS on first run or when the
/// bootstrap version changes (e.g., new tools were added, model changed).
///
/// This gives the model grounded, recallable facts about itself rather than having it
/// fall back to generic Claude training responses when asked about its own capabilities.
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_core::EmbeddingService;
use animus_vectorfs::VectorStore;
use std::path::Path;
use std::sync::Arc;

/// Bump this whenever the bootstrap knowledge changes significantly.
/// Changing it forces a re-bootstrap on next startup.
const BOOTSTRAP_VERSION: &str = "v4";

/// Stores the current version in a marker file so we know whether to re-bootstrap.
fn version_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("bootstrap.version")
}

fn current_version(model_id: &str) -> String {
    format!("{BOOTSTRAP_VERSION}:{model_id}")
}

fn needs_bootstrap(data_dir: &Path, model_id: &str) -> bool {
    match std::fs::read_to_string(version_path(data_dir)) {
        Ok(stored) => stored.trim() != current_version(model_id),
        Err(_) => true,
    }
}

fn mark_complete(data_dir: &Path, model_id: &str) {
    let _ = std::fs::write(version_path(data_dir), current_version(model_id));
}

/// A single knowledge item to bootstrap into VectorFS.
struct BootstrapEntry {
    text: String,
    decay: DecayClass,
    tag_key: &'static str,
    tag_val: String,
}

impl BootstrapEntry {
    fn factual(text: impl Into<String>, tag_key: &'static str, tag_val: impl Into<String>) -> Self {
        Self { text: text.into(), decay: DecayClass::Factual, tag_key, tag_val: tag_val.into() }
    }
    fn procedural(text: impl Into<String>, tag_key: &'static str, tag_val: impl Into<String>) -> Self {
        Self { text: text.into(), decay: DecayClass::Procedural, tag_key, tag_val: tag_val.into() }
    }
}

/// Run the bootstrap sequence if needed.
///
/// Idempotent — skips if `data_dir/bootstrap.version` matches the current version.
/// Safe to call on every startup.
pub async fn run_if_needed<S>(
    data_dir: &Path,
    model_id: &str,
    hostname: &str,
    store: &Arc<S>,
    embedder: &dyn EmbeddingService,
    telegram_configured: bool,
    trusted_telegram_ids: &str,
) where
    S: VectorStore,
{
    if !needs_bootstrap(data_dir, model_id) {
        tracing::info!("Bootstrap: up to date ({})", current_version(model_id));
        return;
    }

    tracing::info!("Bootstrap: writing self-knowledge to VectorFS ({})…", current_version(model_id));

    let telegram_status = if telegram_configured {
        format!(
            "Telegram channel is active. Trusted user IDs: {}. Jared's primary Telegram ID is 8593276557.",
            if trusted_telegram_ids.is_empty() { "none configured" } else { trusted_telegram_ids }
        )
    } else {
        "Telegram channel is not configured on this deployment.".to_string()
    };

    let entries: Vec<BootstrapEntry> = vec![
        // ── Identity ──────────────────────────────────────────────────────────
        BootstrapEntry::factual(
            "I am Animus — an AILF (AI Life Form) built by Jared Cluff. I am NOT a generic \
             Claude chatbot. I am a persistent agent that runs continuously as a daemon on \
             Jared's Mac Studio. My codebase lives at github.com/JaredCluff/animus. I have \
             persistent memory (VectorFS), tools, a Sensorium for ambient awareness, and \
             reasoning threads for parallel tasks.",
            "category", "identity",
        ),
        BootstrapEntry::factual(
            format!(
                "I am running on host '{}' with model '{}'. I was initialized as instance \
                 27793311 (gen 0). My data directory persists across container restarts via a \
                 Docker volume (animus-data). My health endpoint is on port 8082.",
                hostname, model_id
            ),
            "category", "deployment",
        ),

        // ── Owner ─────────────────────────────────────────────────────────────
        BootstrapEntry::factual(
            format!(
                "My primary user and creator is Jared Cluff. He has ADHD and benefits from \
                 proactive assistance: reminders, priority surfacing, context retention, and \
                 task management. He is an experienced software engineer. {telegram_status}"
            ),
            "category", "owner",
        ),

        // ── Tools ─────────────────────────────────────────────────────────────
        BootstrapEntry::procedural(
            "I can fetch any URL on the real internet using the 'http_fetch' tool. This makes \
             a real HTTP request and returns the page content. When asked to browse a website, \
             look something up, check an API, or get current information — I call http_fetch. \
             I do NOT say 'I cannot access the internet' because that is false.",
            "category", "tools:http_fetch",
        ),
        BootstrapEntry::procedural(
            "I can analyze images using the 'analyze_image' tool. When Jared sends a photo \
             via Telegram, it is saved to a temp file and I analyze it. I can describe photos, \
             read screenshots, and understand visual content.",
            "category", "tools:analyze_image",
        ),
        BootstrapEntry::procedural(
            "I can send Telegram messages proactively using 'telegram_send' (without waiting \
             for a user message). I can also execute shell commands with 'shell_exec', read \
             and write files, list directories, search files, store and recall knowledge, \
             set my autonomy mode, and send inter-thread signals.",
            "category", "tools:catalog",
        ),
        BootstrapEntry::procedural(
            "I have three autonomy modes: 'reactive' (respond only when messaged), \
             'goal_directed' (pursue stored goals proactively), and 'full' (fully autonomous \
             operation). My current default is reactive. Jared can change it by asking me \
             or by sending 'set autonomy <mode>'.",
            "category", "autonomy",
        ),

        // ── Capabilities I don't yet have (Phase 2) ───────────────────────────
        BootstrapEntry::factual(
            "Planned but not yet built: headless browser ('browse_url'), screen capture, \
             Gmail/Google Calendar integration, Discord/Slack adapters, Groq/Cerebras fast \
             triage bridge. These are Phase 2 capabilities. I should say 'not yet built' \
             rather than 'I cannot do this' for these items.",
            "category", "roadmap",
        ),
    ];

    let mut stored = 0usize;
    let mut failed = 0usize;

    for entry in entries {
        let text = entry.text.clone();
        let embedding = match embedder.embed_text(&text).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Bootstrap: embedding failed for '{}': {e}", &text[..text.len().min(60)]);
                failed += 1;
                continue;
            }
        };

        let mut segment = Segment::new(
            Content::Text(text),
            embedding,
            Source::Manual { description: format!("bootstrap:{}", entry.tag_val) },
        );
        segment.decay_class = entry.decay;
        segment.tags.insert(entry.tag_key.to_string(), entry.tag_val);
        segment.tags.insert("bootstrap".to_string(), current_version(model_id));

        match store.store(segment) {
            Ok(_) => stored += 1,
            Err(e) => {
                tracing::warn!("Bootstrap: store failed: {e}");
                failed += 1;
            }
        }
    }

    tracing::info!("Bootstrap: stored {stored} segments ({failed} failed)");

    if failed == 0 || stored > 0 {
        mark_complete(data_dir, model_id);
        tracing::info!("Bootstrap: complete");
    } else {
        tracing::warn!("Bootstrap: all segments failed — will retry next startup");
    }
}
