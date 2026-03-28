use animus_core::threading::*;
use animus_core::ThreadId;
use animus_cortex::thread::ReasoningThread;
use animus_embed::SyntheticEmbedding;
use animus_vectorfs::store::MmapVectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn make_thread(name: &str) -> (ReasoningThread<MmapVectorStore>, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(&dir.path().join("vfs"), 128).unwrap());
    let thread = ReasoningThread::new(name.to_string(), store, 8000);
    (thread, dir)
}

#[test]
fn thread_starts_active() {
    let (thread, _dir) = make_thread("test");
    assert_eq!(thread.status(), ThreadStatus::Active);
}

#[test]
fn thread_can_be_suspended() {
    let (mut thread, _dir) = make_thread("test");
    assert!(thread.set_status(ThreadStatus::Suspended).is_ok());
    assert_eq!(thread.status(), ThreadStatus::Suspended);
}

#[test]
fn completed_thread_cannot_resume() {
    let (mut thread, _dir) = make_thread("test");
    thread.set_status(ThreadStatus::Completed).unwrap();
    assert!(thread.set_status(ThreadStatus::Active).is_err());
}

#[test]
fn signal_delivery() {
    let (mut thread, _dir) = make_thread("receiver");
    let signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Normal,
        summary: "hello from thread A".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    };
    thread.deliver_signal(signal);
    assert_eq!(thread.pending_signals().len(), 1);
}

#[test]
fn signals_ordered_by_priority() {
    let (mut thread, _dir) = make_thread("receiver");

    thread.deliver_signal(Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Info,
        summary: "low priority".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    });
    thread.deliver_signal(Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Urgent,
        summary: "high priority".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    });

    let signals = thread.drain_signals();
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[0].priority, SignalPriority::Urgent);
    assert_eq!(signals[1].priority, SignalPriority::Info);
}

#[test]
fn drain_signals_clears_inbox() {
    let (mut thread, _dir) = make_thread("receiver");
    thread.deliver_signal(Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Normal,
        summary: "test".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    });
    let _ = thread.drain_signals();
    assert!(thread.pending_signals().is_empty());
}

#[tokio::test]
async fn signals_drained_after_processing() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(&dir.path().join("vfs"), 128).unwrap());
    let mut thread = ReasoningThread::new("test".to_string(), store, 8000);

    // Deliver a signal
    let signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Urgent,
        summary: "Important: build failed".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    };
    thread.deliver_signal(signal);
    assert_eq!(thread.pending_signals().len(), 1);

    // Process a turn
    let embedder = SyntheticEmbedding::new(128);
    let engine = animus_cortex::MockEngine::new("I see the build failure.");

    let _output = thread.process_turn(
        "How are things going?",
        "You are an AI.",
        &engine,
        &embedder,
        None,
    ).await.unwrap();

    // After processing, signals should be drained
    assert!(thread.pending_signals().is_empty());
}
