use animus_core::threading::*;
use animus_core::GoalId;
use animus_cortex::scheduler::ThreadScheduler;
use animus_vectorfs::store::MmapVectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn make_scheduler() -> (ThreadScheduler<MmapVectorStore>, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(&dir.path().join("vfs"), 128).unwrap());
    let scheduler = ThreadScheduler::new(store, 8000);
    (scheduler, dir)
}

#[test]
fn scheduler_starts_empty() {
    let (scheduler, _dir) = make_scheduler();
    assert_eq!(scheduler.thread_count(), 0);
}

#[test]
fn create_and_list_threads() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("main".to_string());
    assert_eq!(scheduler.thread_count(), 1);

    let threads = scheduler.list_threads();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].0, id);
    assert_eq!(threads[0].1, "main");
    assert_eq!(threads[0].2, ThreadStatus::Active);
}

#[test]
fn first_thread_is_active() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("main".to_string());
    assert_eq!(scheduler.active_thread_id(), Some(id));
}

#[test]
fn second_thread_starts_suspended() {
    let (mut scheduler, _dir) = make_scheduler();
    let _main_id = scheduler.create_thread("main".to_string());
    let work_id = scheduler.create_thread("work".to_string());

    let threads = scheduler.list_threads();
    let work = threads.iter().find(|(id, _, _)| *id == work_id).unwrap();
    assert_eq!(work.2, ThreadStatus::Suspended);
}

#[test]
fn switch_to_thread() {
    let (mut scheduler, _dir) = make_scheduler();
    let main_id = scheduler.create_thread("main".to_string());
    let work_id = scheduler.create_thread("work".to_string());

    assert_eq!(scheduler.active_thread_id(), Some(main_id));

    scheduler.switch_to(work_id).unwrap();
    assert_eq!(scheduler.active_thread_id(), Some(work_id));

    let threads = scheduler.list_threads();
    let main_thread = threads.iter().find(|(id, _, _)| *id == main_id).unwrap();
    assert_eq!(main_thread.2, ThreadStatus::Suspended);
}

#[test]
fn suspend_active_thread() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("main".to_string());
    scheduler.suspend(id).unwrap();
    assert_eq!(scheduler.active_thread_id(), None);
}

#[test]
fn cannot_switch_to_completed_thread() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("main".to_string());
    scheduler.complete(id).unwrap();
    assert!(scheduler.switch_to(id).is_err());
}

#[test]
fn signal_between_threads() {
    let (mut scheduler, _dir) = make_scheduler();
    let t1 = scheduler.create_thread("thread-1".to_string());
    let t2 = scheduler.create_thread("thread-2".to_string());

    scheduler.send_signal(t1, t2, SignalPriority::Normal, "hello".to_string(), vec![]).unwrap();

    let signals = scheduler.drain_signals(t2);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].summary, "hello");
}

#[test]
fn set_thread_background() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("worker".to_string());
    scheduler.set_background(id, GoalId::new()).unwrap();

    let threads = scheduler.list_threads();
    let worker = threads.iter().find(|(tid, _, _)| *tid == id).unwrap();
    assert_eq!(worker.2, ThreadStatus::Background);
}

#[test]
fn background_thread_cannot_be_switched_to() {
    let (mut scheduler, _dir) = make_scheduler();
    let _main_id = scheduler.create_thread("main".to_string());
    let bg_id = scheduler.create_thread("background".to_string());
    scheduler.set_background(bg_id, GoalId::new()).unwrap();
    assert!(scheduler.switch_to(bg_id).is_err());
}
