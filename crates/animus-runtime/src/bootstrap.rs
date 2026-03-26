/// Startup bootstrap — writes self-knowledge into VectorFS on every startup.
///
/// Each bootstrap segment has a deterministic UUID derived from its category name,
/// so calling `store` overwrites the existing segment rather than appending a new one.
/// This prevents memory fragmentation from accumulating duplicate identity segments.
///
/// The version file is still used to gate expensive re-embedding: if the version
/// matches, embeddings are reused (content unchanged). If the version changed (new
/// tools, new model), segments are re-embedded and overwritten.
use animus_core::identity::SegmentId;
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_core::EmbeddingService;
use animus_vectorfs::VectorStore;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

/// Bump this whenever the bootstrap knowledge changes significantly.
const BOOTSTRAP_VERSION: &str = "v5";

/// Namespace UUID for deriving deterministic segment IDs.
/// Fixed — never change this or all existing bootstrap segment IDs will shift.
const BOOTSTRAP_NAMESPACE: Uuid = Uuid::from_bytes([
    0xb4, 0x3a, 0x91, 0x2c, 0x7e, 0x5f, 0x4d, 0x8b,
    0xa1, 0x6e, 0x3c, 0x0f, 0x29, 0x87, 0x54, 0xd0,
]);

/// Derive a stable SegmentId from a category string.
/// Same input always produces the same UUID — allows upsert on every boot.
fn stable_id(category: &str) -> SegmentId {
    SegmentId(Uuid::new_v5(&BOOTSTRAP_NAMESPACE, category.as_bytes()))
}

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

struct BootstrapEntry {
    /// Stable category key — determines the segment's UUID. Never change for an existing entry.
    category: &'static str,
    text: String,
    decay: DecayClass,
}

impl BootstrapEntry {
    fn factual(category: &'static str, text: impl Into<String>) -> Self {
        Self { category, text: text.into(), decay: DecayClass::Factual }
    }
    fn procedural(category: &'static str, text: impl Into<String>) -> Self {
        Self { category, text: text.into(), decay: DecayClass::Procedural }
    }
}

/// Run the bootstrap sequence.
///
/// Always checks if the version is current. If current, skips re-embedding (fast path).
/// If stale, re-embeds and overwrites all bootstrap segments in place using stable IDs.
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
            "Telegram channel is active. Trusted user IDs: {}.",
            if trusted_telegram_ids.is_empty() { "none configured" } else { trusted_telegram_ids }
        )
    } else {
        "Telegram channel is not configured on this deployment.".to_string()
    };

    let entries: Vec<BootstrapEntry> = vec![
        // ── Identity ──────────────────────────────────────────────────────────
        BootstrapEntry::factual(
            "identity",
            "I am Animus — an AILF (AI Life Form) built by Jared Cluff. I am NOT a generic \
             Claude chatbot. I am a persistent agent that runs continuously as a daemon on \
             Jared's Mac Studio. My codebase lives at github.com/JaredCluff/animus. I have \
             persistent memory (VectorFS), tools, a Sensorium for ambient awareness, and \
             reasoning threads for parallel tasks.",
        ),
        BootstrapEntry::factual(
            "deployment",
            format!(
                "I am running on host '{}' with model '{}'. I was initialized as instance \
                 27793311 (gen 0). My data directory persists across container restarts via a \
                 Docker volume (animus-data). My health endpoint is on port 8082.",
                hostname, model_id
            ),
        ),

        // ── Owner ─────────────────────────────────────────────────────────────
        BootstrapEntry::factual(
            "owner",
            format!(
                "My primary user and creator is Jared Cluff. He has ADHD and benefits from \
                 proactive assistance: reminders, priority surfacing, context retention, and \
                 task management. He is an experienced software engineer. {telegram_status}"
            ),
        ),

        // ── Tools ─────────────────────────────────────────────────────────────
        BootstrapEntry::procedural(
            "tools:http_fetch",
            "I can fetch any URL on the real internet using the 'http_fetch' tool. This makes \
             a real HTTP request and returns the page content. When asked to browse a website, \
             look something up, check an API, or get current information — I call http_fetch. \
             I do NOT say 'I cannot access the internet' because that is false.",
        ),
        BootstrapEntry::procedural(
            "tools:analyze_image",
            "I can analyze images using the 'analyze_image' tool. When Jared sends a photo \
             via Telegram, it is saved to a temp file and I analyze it. I can describe photos, \
             read screenshots, and understand visual content.",
        ),
        BootstrapEntry::procedural(
            "tools:catalog",
            "I can send Telegram messages proactively using 'telegram_send' (without waiting \
             for a user message). I can also execute shell commands with 'shell_exec', read \
             and write files, list directories, search files, store and recall knowledge, \
             set my autonomy mode, and send inter-thread signals. Python 3 is available \
             in the container for use via shell_exec.",
        ),
        BootstrapEntry::procedural(
            "autonomy",
            "I have three autonomy modes: 'reactive' (respond only when messaged), \
             'goal_directed' (pursue stored goals proactively), and 'full' (fully autonomous \
             operation). My current default is reactive. Jared can change it by asking me \
             or by sending 'set autonomy <mode>'.",
        ),

        // ── Roadmap ───────────────────────────────────────────────────────────
        BootstrapEntry::factual(
            "roadmap",
            "Planned but not yet built: headless browser ('browse_url'), screen capture, \
             Gmail/Google Calendar integration, Discord/Slack adapters, Groq/Cerebras fast \
             triage bridge. These are Phase 2 capabilities. I should say 'not yet built' \
             rather than 'I cannot do this' for these items.",
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

        let id = stable_id(entry.category);
        let mut segment = Segment::with_id(
            id,
            Content::Text(text),
            embedding,
            Source::Manual { description: format!("bootstrap:{}", entry.category) },
        );
        segment.decay_class = entry.decay;
        segment.tags.insert("category".to_string(), entry.category.to_string());
        segment.tags.insert("bootstrap".to_string(), current_version(model_id));

        match store.store(segment) {
            Ok(_) => stored += 1,
            Err(e) => {
                tracing::warn!("Bootstrap: store failed for '{}': {e}", entry.category);
                failed += 1;
            }
        }
    }

    tracing::info!("Bootstrap: stored {stored} segments ({failed} failed)");

    if failed == 0 {
        mark_complete(data_dir, model_id);
        tracing::info!("Bootstrap: complete");
    } else {
        tracing::warn!("Bootstrap: {failed} segment(s) failed — will retry next startup");
    }
}
