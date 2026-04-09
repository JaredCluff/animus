use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Default max file size before rotation: 50 MiB.
/// Larger than AuditTrail because full conversation text is stored.
const DEFAULT_MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Number of rotated journal files to keep.
/// With 50 MiB per file and 5 rotated files, that's ~300 MiB max journal history.
const MAX_ROTATED_FILES: u32 = 5;

/// A single conversation exchange persisted to the journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Unique entry identifier for dedup and watermark tracking.
    pub id: String,
    /// When this exchange occurred.
    pub timestamp: DateTime<Utc>,
    /// Channel identifier (e.g. "telegram", "nats").
    pub channel_id: String,
    /// Thread/conversation identifier within the channel.
    pub thread_id: String,
    /// Full user input text (no truncation).
    pub input: String,
    /// Full assistant response text (no truncation).
    pub output: String,
}

/// Append-only conversation journal backed by a JSONL file.
///
/// Writes are fsynced immediately — no conversation data is lost on crash or
/// container restart as long as this method returns Ok. The journal is the
/// "hippocampal buffer": fast, durable, unprocessed. Background "dreaming"
/// consolidates entries into proper VectorFS segments over time.
pub struct ConversationJournal {
    path: PathBuf,
    file: File,
    entry_count: usize,
    max_file_size: u64,
}

impl ConversationJournal {
    /// Open or create a conversation journal at the given path.
    pub fn open(path: &Path) -> animus_core::Result<Self> {
        Self::open_with_max_size(path, DEFAULT_MAX_FILE_SIZE)
    }

    pub fn open_with_max_size(path: &Path, max_file_size: u64) -> animus_core::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let entry_count = if path.exists() {
            let f = File::open(path)?;
            BufReader::new(f).lines().count()
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        Ok(Self {
            path: path.to_path_buf(),
            file,
            entry_count,
            max_file_size,
        })
    }

    /// Append a conversation exchange to the journal.
    /// This is fsynced to disk immediately — survives crash/restart.
    pub fn append(&mut self, entry: &JournalEntry) -> animus_core::Result<()> {
        let json = serde_json::to_string(entry)
            .map_err(|e| animus_core::AnimusError::Storage(format!("journal serialize: {e}")))?;
        writeln!(self.file, "{json}")?;
        self.file.sync_data()?; // fsync — durable on disk
        self.entry_count += 1;

        if self.needs_rotation() {
            self.rotate()?;
        }

        Ok(())
    }

    /// Read the most recent `limit` entries across current and rotated files.
    /// Used for startup recovery to reconstruct conversation context.
    /// Reads all files (rotated + current) and returns only the last `limit`.
    pub fn read_recent(path: &Path, limit: usize) -> animus_core::Result<Vec<JournalEntry>> {
        let mut entries = std::collections::VecDeque::with_capacity(limit.min(256));

        // Collect all file paths: rotated oldest-first (.5, .4, .3, .2, .1), then current
        let mut files: Vec<PathBuf> = Vec::new();
        for i in (1..=MAX_ROTATED_FILES).rev() {
            let rotated = path.with_extension(format!("jsonl.{i}"));
            if rotated.exists() {
                files.push(rotated);
            }
        }
        if path.exists() {
            files.push(path.to_path_buf());
        }

        for file_path in &files {
            let file = File::open(file_path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<JournalEntry>(&line) {
                    Ok(entry) => {
                        if entries.len() == limit {
                            entries.pop_front();
                        }
                        entries.push_back(entry);
                    }
                    Err(e) => {
                        tracing::warn!("Skipping malformed journal entry: {e}");
                    }
                }
            }
        }
        Ok(entries.into())
    }

    /// Read all entries across current and rotated files, newest last.
    /// Used by the dreaming consolidation to find unconsolidated entries.
    /// Returns (entries, total_line_count_in_current_file).
    pub fn read_all(path: &Path) -> animus_core::Result<Vec<JournalEntry>> {
        let mut all_entries = Vec::new();

        // Read rotated files oldest-first (.5, .4, .3, .2, .1)
        for i in (1..=MAX_ROTATED_FILES).rev() {
            let rotated = path.with_extension(format!("jsonl.{i}"));
            if rotated.exists() {
                let file = File::open(&rotated)?;
                let reader = BufReader::new(file);
                for line in reader.lines() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(entry) = serde_json::from_str::<JournalEntry>(&line) {
                        all_entries.push(entry);
                    }
                }
            }
        }

        // Read current file
        if path.exists() {
            let file = File::open(path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<JournalEntry>(&line) {
                    all_entries.push(entry);
                }
            }
        }

        Ok(all_entries)
    }

    /// Read entries that haven't been consolidated yet.
    /// Uses a watermark file to track the last consolidated entry ID.
    ///
    /// Optimized to scan only the current file first. Falls back to older
    /// rotated files only if the watermark isn't found in the current file.
    /// When the watermark ID has been rotated away entirely, returns only
    /// entries from the last 24 hours to prevent mass re-consolidation.
    pub fn read_unconsolidated(path: &Path) -> animus_core::Result<Vec<JournalEntry>> {
        let watermark = Self::read_watermark(path);

        // Fast path: try current file only
        let current_entries = Self::read_file_entries(path)?;

        if let Some(ref wm_id) = watermark {
            // Check if watermark is in the current file
            if let Some(pos) = current_entries.iter().position(|e| e.id == *wm_id) {
                return Ok(current_entries.into_iter().skip(pos + 1).collect());
            }

            // Watermark not in current file — scan rotated files
            let all = Self::read_all(path)?;
            if let Some(pos) = all.iter().position(|e| e.id == *wm_id) {
                return Ok(all.into_iter().skip(pos + 1).collect());
            }

            // Watermark ID rotated away — fall through to age-based cutoff
            tracing::warn!(
                "Journal watermark ID not found (rotated away). Using 24h age cutoff to prevent mass re-consolidation."
            );
        }

        // No watermark or watermark rotated away: return entries from last 24h only.
        // This prevents re-consolidating the entire journal history as duplicates.
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
        let all = if watermark.is_none() && current_entries.is_empty() {
            // No watermark and empty current file — check rotated files
            Self::read_all(path)?
        } else if watermark.is_none() {
            // No watermark — might be first run. Use all files.
            Self::read_all(path)?
        } else {
            // Watermark rotated away — all files needed for age filter
            Self::read_all(path)?
        };
        Ok(all.into_iter().filter(|e| e.timestamp >= cutoff).collect())
    }

    /// Read entries from a single file.
    fn read_file_entries(path: &Path) -> animus_core::Result<Vec<JournalEntry>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<JournalEntry>(&line) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Advance the watermark to the given entry ID.
    /// Called after successful consolidation of entries up to this point.
    /// Fsynced to prevent re-consolidation duplicates on crash.
    pub fn advance_watermark(journal_path: &Path, last_consolidated_id: &str) -> animus_core::Result<()> {
        let watermark_path = Self::watermark_path(journal_path);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&watermark_path)?;
        let mut writer = std::io::BufWriter::new(&file);
        writer.write_all(last_consolidated_id.as_bytes())?;
        writer.flush()?;
        file.sync_data()?;
        Ok(())
    }

    /// Get the current number of entries in the active journal file.
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Flush the journal to disk (called during graceful shutdown).
    pub fn flush(&mut self) -> animus_core::Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    // -- Private helpers --

    fn watermark_path(journal_path: &Path) -> PathBuf {
        journal_path.with_extension("watermark")
    }

    fn read_watermark(journal_path: &Path) -> Option<String> {
        let wm_path = Self::watermark_path(journal_path);
        fs::read_to_string(&wm_path).ok().map(|s| s.trim().to_string())
    }

    fn needs_rotation(&self) -> bool {
        fs::metadata(&self.path)
            .map(|m| m.len() >= self.max_file_size)
            .unwrap_or(false)
    }

    fn rotate(&mut self) -> animus_core::Result<()> {
        // Shift existing rotated files: .5 deleted, .4 → .5, .3 → .4, etc.
        let oldest = self.path.with_extension(format!("jsonl.{MAX_ROTATED_FILES}"));
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }

        for i in (1..MAX_ROTATED_FILES).rev() {
            let from = self.path.with_extension(format!("jsonl.{i}"));
            let to = self.path.with_extension(format!("jsonl.{}", i + 1));
            if from.exists() {
                fs::rename(&from, &to)?;
            }
        }

        // Rotate current file to .1
        let rotated = self.path.with_extension("jsonl.1");
        fs::rename(&self.path, &rotated)?;

        // Open fresh file
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.entry_count = 0;

        Ok(())
    }
}

/// Create a new JournalEntry with a fresh UUID.
pub fn new_journal_entry(
    channel_id: &str,
    thread_id: &str,
    input: &str,
    output: &str,
) -> JournalEntry {
    JournalEntry {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        channel_id: channel_id.to_string(),
        thread_id: thread_id.to_string(),
        input: input.to_string(),
        output: output.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_journal_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_journal.jsonl");
        let mut journal = ConversationJournal::open(&path).unwrap();

        let entry = new_journal_entry("telegram", "12345", "hello", "hi there");
        journal.append(&entry).unwrap();
        assert_eq!(journal.entry_count(), 1);

        let loaded = ConversationJournal::read_recent(&path, 10).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].input, "hello");
        assert_eq!(loaded[0].output, "hi there");
        assert_eq!(loaded[0].channel_id, "telegram");
    }

    #[test]
    fn watermark_tracks_consolidation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_journal.jsonl");
        let mut journal = ConversationJournal::open(&path).unwrap();

        let e1 = new_journal_entry("tg", "1", "a", "b");
        let e2 = new_journal_entry("tg", "1", "c", "d");
        let e3 = new_journal_entry("tg", "1", "e", "f");
        journal.append(&e1).unwrap();
        journal.append(&e2).unwrap();
        journal.append(&e3).unwrap();

        // Mark first two as consolidated
        ConversationJournal::advance_watermark(&path, &e2.id).unwrap();

        let unconsolidated = ConversationJournal::read_unconsolidated(&path).unwrap();
        assert_eq!(unconsolidated.len(), 1);
        assert_eq!(unconsolidated[0].input, "e");
    }

    #[test]
    fn read_recent_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_journal.jsonl");
        let mut journal = ConversationJournal::open(&path).unwrap();

        for i in 0..20 {
            let entry = new_journal_entry("tg", "1", &format!("in-{i}"), &format!("out-{i}"));
            journal.append(&entry).unwrap();
        }

        let recent = ConversationJournal::read_recent(&path, 5).unwrap();
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0].input, "in-15");
        assert_eq!(recent[4].input, "in-19");
    }

    #[test]
    fn rotation_preserves_entries_across_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_journal.jsonl");
        // ~500 bytes holds ~3 entries per file before rotation.
        // With 5 rotated files + current, that's ~18 entry capacity.
        let mut journal = ConversationJournal::open_with_max_size(&path, 500).unwrap();

        for i in 0..10 {
            let entry = new_journal_entry("tg", "1", &format!("in-{i}"), &format!("out-{i}"));
            journal.append(&entry).unwrap();
        }

        // Verify at least one rotated file exists
        let rotated_1 = path.with_extension("jsonl.1");
        assert!(rotated_1.exists(), "Expected rotated file .jsonl.1 to exist");

        // read_recent should span rotated files and return all 10 entries
        let recent = ConversationJournal::read_recent(&path, 100).unwrap();
        assert_eq!(recent.len(), 10, "Expected all 10 entries across rotated files");
        assert_eq!(recent[0].input, "in-0");
        assert_eq!(recent[9].input, "in-9");

        // read_all should also return all 10
        let all = ConversationJournal::read_all(&path).unwrap();
        assert_eq!(all.len(), 10);

        // read_recent with limit should return only the last N
        let last_3 = ConversationJournal::read_recent(&path, 3).unwrap();
        assert_eq!(last_3.len(), 3);
        assert_eq!(last_3[0].input, "in-7");
        assert_eq!(last_3[2].input, "in-9");
    }

    #[test]
    fn rotation_caps_at_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_journal.jsonl");
        // Very tiny: forces rotation almost every entry
        let mut journal = ConversationJournal::open_with_max_size(&path, 80).unwrap();

        // Write enough entries to exceed MAX_ROTATED_FILES (5) rotations
        for i in 0..30 {
            let entry = new_journal_entry("tg", "1", &format!("i{i}"), &format!("o{i}"));
            journal.append(&entry).unwrap();
        }

        // Should not have more than MAX_ROTATED_FILES + 1 files
        let rotated_6 = path.with_extension("jsonl.6");
        assert!(!rotated_6.exists(), "Should not have more than 5 rotated files");

        // All readable entries should be consistent (no corruption)
        let all = ConversationJournal::read_all(&path).unwrap();
        assert!(!all.is_empty(), "Should have some entries after rotation");
        // Entries should be in chronological order
        for window in all.windows(2) {
            assert!(
                window[0].timestamp <= window[1].timestamp,
                "Entries should be chronologically ordered"
            );
        }
    }
}
