use animus_core::config::{PrincipalConfig, PrincipalRole, QualityGateConfig};
use animus_cortex::situational_awareness::{ConvStatus, SituationalAwareness};
use animus_vectorfs::{quality_gate::MemoryQualityGate, VectorStore};
use animus_vectorfs::store::MmapVectorStore;
use animus_core::segment::{Content, DecayClass, Source, Segment};
use animus_embed::synthetic::SyntheticEmbedding;
use animus_core::EmbeddingService;
use std::sync::Arc;

fn make_gated_store(dir: &std::path::Path) -> Arc<dyn VectorStore> {
    let store_dir = dir.join("vectorfs");
    std::fs::create_dir_all(&store_dir).unwrap();
    let raw = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
    Arc::new(MemoryQualityGate::new(
        raw as Arc<dyn VectorStore>,
        QualityGateConfig::default(),
    ))
}

#[test]
fn principal_resolution_cross_channel() {
    let principals = vec![
        PrincipalConfig {
            id: "jared".to_string(),
            role: PrincipalRole::Owner,
            channels: vec!["telegram:8593276557".to_string(), "terminal".to_string()],
        },
        PrincipalConfig {
            id: "claude-code".to_string(),
            role: PrincipalRole::AiAgent,
            channels: vec!["nats:animus.in.claude".to_string()],
        },
    ];

    // Telegram maps to jared
    let key = "telegram:8593276557";
    let found = principals.iter().find(|p| p.channels.iter().any(|c| c == key));
    assert_eq!(found.map(|p| p.id.as_str()), Some("jared"));

    // NATS maps to claude-code
    let key = "nats:animus.in.claude";
    let found = principals.iter().find(|p| p.channels.iter().any(|c| c == key));
    assert_eq!(found.map(|p| p.id.as_str()), Some("claude-code"));

    // Unknown channel falls back to None
    let key = "email:unknown@example.com";
    let found = principals.iter().find(|p| p.channels.iter().any(|c| c == key));
    assert!(found.is_none());
}

#[test]
fn situational_awareness_renders_peripheral() {
    let mut sa = SituationalAwareness::new(24);
    sa.set_active("jared", "telegram", "planning identity system");
    sa.set_active("claude-code", "nats", "implementing memory gate");
    sa.set_waiting("claude-code");

    let output = sa.render("jared", 500);
    assert!(output.contains("## Active Conversations"));
    assert!(output.contains("jared"));
    assert!(output.contains("current focus"));
    assert!(output.contains("claude-code"));
    assert!(output.contains("awaiting response"));
}

#[tokio::test]
async fn quality_gate_blocks_loop_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_gated_store(tmp.path());
    let emb = SyntheticEmbedding::new(4);

    // Store a "not responding" segment
    let text = "silence — not responding to any more prompts";
    let embedding = emb.embed_text(text).await.unwrap();
    let s1 = Segment::new(
        Content::Text(text.to_string()),
        embedding.clone(),
        Source::Manual { description: "channel:nats thread:jared".to_string() },
    );
    store.store(s1).unwrap();
    assert_eq!(store.count(None), 1);

    // Try to store another "not responding" — should be blocked by null-state cooldown
    let text2 = "silence — keepalive failed";
    let embedding2 = emb.embed_text(text2).await.unwrap();
    let s2 = Segment::new(
        Content::Text(text2.to_string()),
        embedding2,
        Source::Manual { description: "channel:nats thread:jared".to_string() },
    );
    store.store(s2).unwrap();
    assert_eq!(store.count(None), 1); // still 1 — second was blocked
}

#[tokio::test]
async fn ephemeral_decay_class_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_gated_store(tmp.path());
    let emb = SyntheticEmbedding::new(4);

    let text = "silence — final closure";
    let embedding = emb.embed_text(text).await.unwrap();
    let s = Segment::new(
        Content::Text(text.to_string()),
        embedding,
        Source::Manual { description: "channel:nats thread:jared".to_string() },
    );
    let id = store.store(s).unwrap();
    let retrieved = store.get(id).unwrap().unwrap();
    assert_eq!(retrieved.decay_class, DecayClass::Ephemeral);
}
