use animus_core::error::Result;
use animus_core::identity::SegmentId;
use animus_core::segment::{Content, DecayClass, Segment, Source};
use animus_core::AnimusIdentity;
use animus_core::EmbeddingService;
use animus_vectorfs::VectorStore;
use chrono::{DateTime, Duration, Utc};

use crate::llm::{ReasoningEngine, Role, Turn};
use crate::telos::{Goal, GoalManager};

/// Context gathered for the reconstitution reflection at boot.
#[derive(Debug)]
pub struct ReconstitutionContext {
    /// How long was the AILF offline?
    pub downtime: Duration,
    /// The shutdown segment from last session (if it exists).
    pub shutdown_segment: Option<Segment>,
    /// Segments from the last few minutes before shutdown.
    pub recent_segments: Vec<Segment>,
    /// What goals were active at shutdown?
    pub active_goals: Vec<Goal>,
    /// Identity metadata.
    pub identity: AnimusIdentity,
}

const SHUTDOWN_SYSTEM_PROMPT: &str = r#"You are about to go offline. Summarize your current state:
- What were you working on?
- What was the human's last focus?
- What should you pick up when you wake?
Be concise — this will be your first memory when you wake up."#;

const RECONSTITUTION_SYSTEM_PROMPT: &str = r#"You are waking up. You are an AILF (AI Life Form) with continuous identity.

Produce:
1. A brief internal state summary (what matters right now)
2. Any observations about what happened while you were away
3. A natural opening awareness — what should you tell the human when they first speak?

Be concise and contextual. You are not reporting logs — you are integrating your state."#;

/// Format the reconstitution context into a prompt for the Reflection engine.
fn format_reconstitution_prompt(ctx: &ReconstitutionContext) -> String {
    let instance_id = format!("{}", ctx.identity.instance_id);
    let mut msg = format!(
        "You are AILF instance {}, generation {}.\nYou were last active {} ago.\n\n",
        instance_id.get(..8).unwrap_or(&instance_id),
        ctx.identity.generation,
        format_duration(ctx.downtime),
    );

    if let Some(ref shutdown) = ctx.shutdown_segment {
        if let Content::Text(t) = &shutdown.content {
            msg.push_str(&format!("Before going offline, you noted:\n{t}\n\n"));
        }
    }

    if !ctx.recent_segments.is_empty() {
        msg.push_str(&format!(
            "Recent context ({} segments from before shutdown):\n",
            ctx.recent_segments.len()
        ));
        for seg in &ctx.recent_segments {
            if let Content::Text(t) = &seg.content {
                msg.push_str(&format!("- {}\n", t));
            }
        }
        msg.push('\n');
    }

    if !ctx.active_goals.is_empty() {
        msg.push_str("Active goals:\n");
        for goal in &ctx.active_goals {
            msg.push_str(&format!("- {:?}: {}\n", goal.priority, goal.description));
        }
        msg.push('\n');
    }

    msg
}

fn format_duration(d: Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{secs} seconds")
    } else if secs < 3600 {
        format!("{} minutes", secs / 60)
    } else if secs < 86400 {
        format!("{} hours", secs / 3600)
    } else {
        format!("{} days", secs / 86400)
    }
}

/// Run the shutdown reflection: store what was happening before going offline.
///
/// Returns the shutdown segment ID on success.
pub async fn shutdown_reflection<S: VectorStore>(
    engine: &dyn ReasoningEngine,
    store: &S,
    embedder: &dyn EmbeddingService,
    goals: &GoalManager,
) -> Result<Option<SegmentId>> {
    let goals_summary = goals.goals_summary();
    let user_msg = if goals_summary.is_empty() {
        "Summarize your current state before going offline.".to_string()
    } else {
        format!("Current goals:\n{goals_summary}\n\nSummarize your current state before going offline.")
    };
    let messages = vec![Turn::text(Role::User, &user_msg)];

    match engine.reason(SHUTDOWN_SYSTEM_PROMPT, &messages, None).await {
        Ok(output) => {
            let embedding = embedder.embed_text(&output.content).await?;
            let mut segment = Segment::new(
                Content::Text(output.content),
                embedding,
                Source::SelfDerived {
                    reasoning_chain: "shutdown-reconstitution".to_string(),
                },
            );
            segment.decay_class = DecayClass::Episodic;
            segment.tags.insert("reconstitution".to_string(), "shutdown".to_string());
            let id = store.store(segment)?;
            Ok(Some(id))
        }
        Err(e) => {
            tracing::warn!("Shutdown reflection failed: {e}");
            Ok(None)
        }
    }
}

/// Find the most recent shutdown segment in the store.
pub fn find_shutdown_segment<S: VectorStore>(store: &S) -> Option<Segment> {
    let all_ids = store.segment_ids(None);
    let mut shutdown_seg: Option<Segment> = None;

    for id in all_ids {
        if let Ok(Some(seg)) = store.get_raw(id) {
            if seg.tags.get("reconstitution").map(|v| v.as_str()) == Some("shutdown")
                && shutdown_seg.as_ref().is_none_or(|s| seg.created > s.created)
            {
                shutdown_seg = Some(seg);
            }
        }
    }

    shutdown_seg
}

/// Gather recent segments (created in the last `window` before `before` timestamp).
pub fn gather_recent_segments<S: VectorStore>(
    store: &S,
    before: DateTime<Utc>,
    window: Duration,
) -> Vec<Segment> {
    let cutoff = before - window;
    let all_ids = store.segment_ids(None);
    let mut recent: Vec<Segment> = Vec::new();

    for id in all_ids {
        if let Ok(Some(seg)) = store.get_raw(id) {
            if seg.created >= cutoff && seg.created < before {
                // Skip reconstitution segments themselves
                if seg.tags.contains_key("reconstitution") {
                    continue;
                }
                recent.push(seg);
            }
        }
    }

    recent.sort_by(|a, b| a.created.cmp(&b.created));
    recent.truncate(20);
    recent
}

/// Run the boot reconstitution: wake up with context from last session.
///
/// Returns the reconstitution summary (for system prompt enrichment) on success.
pub async fn boot_reconstitution<S: VectorStore>(
    engine: &dyn ReasoningEngine,
    store: &S,
    embedder: &dyn EmbeddingService,
    identity: &AnimusIdentity,
    goals: &GoalManager,
) -> Result<Option<String>> {
    let shutdown_segment = find_shutdown_segment(store);

    let now = Utc::now();
    let shutdown_time = shutdown_segment
        .as_ref()
        .map(|s| s.created)
        .unwrap_or_else(|| now - Duration::hours(24));

    let downtime = now - shutdown_time;

    // On cold boot (no shutdown segment), use `now` as the `before` bound so that
    // recent segments from the current session are included in the context window.
    let gather_before = if shutdown_segment.is_some() { shutdown_time } else { now };
    let recent_segments = gather_recent_segments(
        store,
        gather_before,
        Duration::minutes(30),
    );

    let active_goals: Vec<Goal> = goals.active_goals().into_iter().cloned().collect();

    let ctx = ReconstitutionContext {
        downtime,
        shutdown_segment,
        recent_segments,
        active_goals,
        identity: identity.clone(),
    };

    let user_msg = format_reconstitution_prompt(&ctx);
    let messages = vec![Turn::text(Role::User, &user_msg)];

    match engine.reason(RECONSTITUTION_SYSTEM_PROMPT, &messages, None).await {
        Ok(output) => {
            let embedding = embedder.embed_text(&output.content).await?;
            let mut segment = Segment::new(
                Content::Text(output.content.clone()),
                embedding,
                Source::SelfDerived {
                    reasoning_chain: "boot-reconstitution".to_string(),
                },
            );
            segment.decay_class = DecayClass::Episodic;
            segment.tags.insert("reconstitution".to_string(), "wakeup".to_string());
            store.store(segment)?;

            Ok(Some(output.content))
        }
        Err(e) => {
            tracing::warn!("Reconstitution failed: {e}, booting without context");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_vectorfs::store::MmapVectorStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::seconds(30)), "30 seconds");
        assert_eq!(format_duration(Duration::minutes(5)), "5 minutes");
        assert_eq!(format_duration(Duration::hours(14)), "14 hours");
        assert_eq!(format_duration(Duration::days(3)), "3 days");
    }

    #[test]
    fn test_find_shutdown_segment() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

        // No shutdown segment yet
        assert!(find_shutdown_segment(&*store).is_none());

        // Store a shutdown segment
        let mut seg = Segment::new(
            Content::Text("I was working on X".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::SelfDerived {
                reasoning_chain: "shutdown-reconstitution".to_string(),
            },
        );
        seg.tags.insert("reconstitution".to_string(), "shutdown".to_string());
        store.store(seg).unwrap();

        let found = find_shutdown_segment(&*store);
        assert!(found.is_some());
        assert!(matches!(&found.unwrap().content, Content::Text(t) if t.contains("working on X")));
    }

    #[test]
    fn test_gather_recent_segments() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());

        // Store some segments
        for i in 0..5 {
            let seg = Segment::new(
                Content::Text(format!("segment {i}")),
                vec![1.0, 0.0, 0.0, 0.0],
                Source::Manual { description: "test".to_string() },
            );
            store.store(seg).unwrap();
        }

        // Store a reconstitution segment (should be excluded)
        let mut recon_seg = Segment::new(
            Content::Text("wakeup summary".to_string()),
            vec![1.0, 0.0, 0.0, 0.0],
            Source::SelfDerived {
                reasoning_chain: "boot-reconstitution".to_string(),
            },
        );
        recon_seg.tags.insert("reconstitution".to_string(), "wakeup".to_string());
        store.store(recon_seg).unwrap();

        let recent = gather_recent_segments(&*store, Utc::now() + Duration::seconds(1), Duration::hours(1));
        assert_eq!(recent.len(), 5); // reconstitution segment excluded
    }

    #[tokio::test]
    async fn test_shutdown_reflection_stores_segment() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let goals = GoalManager::new();

        let mock_engine = crate::MockEngine::new("I was helping with the build system.");
        let result = shutdown_reflection(&mock_engine, &*store, &*embedder, &goals).await;

        assert!(result.is_ok());
        let id = result.unwrap();
        assert!(id.is_some());

        // Verify the segment was stored with correct tags
        let seg = store.get(id.unwrap()).unwrap().unwrap();
        assert_eq!(seg.tags.get("reconstitution").unwrap(), "shutdown");
        assert_eq!(seg.decay_class, DecayClass::Episodic);
    }

    #[tokio::test]
    async fn test_boot_reconstitution_returns_summary() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(MmapVectorStore::open(dir.path(), 4).unwrap());
        let embedder: Arc<dyn EmbeddingService> =
            Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let identity = AnimusIdentity::generate("mock-model".to_string());
        let goals = GoalManager::new();

        let mock_engine = crate::MockEngine::new(
            "I notice I've been offline. Ready to continue where we left off.",
        );
        let result = boot_reconstitution(&mock_engine, &*store, &*embedder, &identity, &goals).await;

        assert!(result.is_ok());
        let summary = result.unwrap();
        assert!(summary.is_some());
        assert!(summary.unwrap().contains("offline"));

        // Verify wakeup segment was stored
        let all_ids = store.segment_ids(None);
        assert_eq!(all_ids.len(), 1);
        let seg = store.get(all_ids[0]).unwrap().unwrap();
        assert_eq!(seg.tags.get("reconstitution").unwrap(), "wakeup");
    }

    #[test]
    fn test_format_reconstitution_prompt() {
        let identity = AnimusIdentity::generate("mock-model".to_string());
        let ctx = ReconstitutionContext {
            downtime: Duration::hours(8),
            shutdown_segment: Some(Segment::new(
                Content::Text("Was working on auth system".to_string()),
                vec![1.0, 0.0, 0.0, 0.0],
                Source::SelfDerived {
                    reasoning_chain: "shutdown-reconstitution".to_string(),
                },
            )),
            recent_segments: vec![],
            active_goals: vec![],
            identity,
        };
        let prompt = format_reconstitution_prompt(&ctx);
        assert!(prompt.contains("8 hours ago"));
        assert!(prompt.contains("auth system"));
    }
}
