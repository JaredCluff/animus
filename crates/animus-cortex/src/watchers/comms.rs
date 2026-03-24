use crate::watcher::{Watcher, WatcherConfig, WatcherEvent};
use animus_core::SignalPriority;
use std::time::Duration;

pub struct CommsWatcher;

impl Watcher for CommsWatcher {
    fn id(&self) -> &str { "comms" }
    fn name(&self) -> &str { "Claude Code Comms" }
    fn default_interval(&self) -> Duration { Duration::from_secs(30) }

    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent> {
        let dir = config.params["dir"].as_str()?;
        let entries = std::fs::read_dir(dir).ok()?;

        let mut batch: Vec<(String, String)> = Vec::new();
        let mut has_alert = false;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let raw = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut msg: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if msg["status"].as_str() != Some("pending") {
                continue;
            }

            // Atomically mark as read BEFORE collecting content into the batch.
            // This closes the window where a concurrent caller could process the same message.
            msg["status"] = serde_json::Value::String("read".to_string());
            if let Ok(updated) = serde_json::to_string_pretty(&msg) {
                let tmp_path = path.with_extension("tmp");
                if std::fs::write(&tmp_path, updated.as_bytes()).is_ok() {
                    let _ = std::fs::rename(&tmp_path, &path);
                } else {
                    // If we can't write the mark, skip this message to avoid duplicate processing
                    continue;
                }
            } else {
                continue;
            }

            // Extract content from the in-memory value (already marked read)
            let subject = msg["subject"]
                .as_str()
                .unwrap_or("(no subject)")
                .to_string();
            let content = msg["content"].as_str().unwrap_or("").to_string();

            if msg["type"].as_str() == Some("alert") {
                has_alert = true;
            }

            batch.push((subject, content));
        }

        if batch.is_empty() { return None; }

        let mut summary = format!("[CommsWatcher] {} message(s) from Claude Code:\n", batch.len());
        for (i, (subject, content)) in batch.iter().enumerate() {
            summary.push_str(&format!("\n{}. **{}**\n{}\n", i + 1, subject, content));
        }

        Some(WatcherEvent {
            priority: if has_alert { SignalPriority::Urgent } else { SignalPriority::Normal },
            summary,
            segment_refs: vec![],
        })
    }
}

// tests only — implementation below
#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::WatcherConfig;

    #[test]
    fn check_returns_none_when_no_dir_param() {
        let cfg = WatcherConfig::default();
        assert!(CommsWatcher.check(&cfg).is_none());
    }

    #[test]
    fn check_returns_none_for_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });
        assert!(CommsWatcher.check(&cfg).is_none());
    }

    #[test]
    fn check_detects_pending_message_and_marks_read() {
        let tmp = tempfile::tempdir().unwrap();
        let msg_path = tmp.path().join("msg-001.json");
        std::fs::write(
            &msg_path,
            r#"{"id":"msg-001","from":"claude","subject":"Hello","content":"Hi there","status":"pending"}"#,
        ).unwrap();

        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });

        let event = CommsWatcher.check(&cfg);
        assert!(event.is_some());
        assert!(event.unwrap().summary.contains("Hello"));

        let raw = std::fs::read_to_string(&msg_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(json["status"], "read");
    }

    #[test]
    fn check_ignores_already_read_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let msg_path = tmp.path().join("msg-002.json");
        std::fs::write(
            &msg_path,
            r#"{"id":"msg-002","from":"claude","subject":"Old","content":"Already read","status":"read"}"#,
        ).unwrap();

        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });
        assert!(CommsWatcher.check(&cfg).is_none());
    }

    #[test]
    fn check_batches_multiple_pending_messages() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            std::fs::write(
                tmp.path().join(format!("msg-{i:03}.json")),
                format!(r#"{{"id":"msg-{i:03}","from":"claude","subject":"Msg {i}","content":"Content {i}","status":"pending"}}"#),
            ).unwrap();
        }

        let mut cfg = WatcherConfig::default();
        cfg.params = serde_json::json!({ "dir": tmp.path().to_str().unwrap() });

        let event = CommsWatcher.check(&cfg);
        assert!(event.is_some());
        let summary = event.unwrap().summary;
        assert!(summary.contains("Msg 1"));
        assert!(summary.contains("Msg 2"));
        assert!(summary.contains("Msg 3"));

        for i in 1..=3 {
            let raw = std::fs::read_to_string(tmp.path().join(format!("msg-{i:03}.json"))).unwrap();
            let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(json["status"], "read");
        }
    }
}
