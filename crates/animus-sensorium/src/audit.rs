use animus_core::sensorium::AuditEntry;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Append-only audit trail backed by a JSON lines file.
pub struct AuditTrail {
    file: File,
    count: usize,
}

impl AuditTrail {
    pub fn open(path: &Path) -> animus_core::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let count = if path.exists() {
            let f = File::open(path)?;
            BufReader::new(f).lines().count()
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        Ok(Self { file, count })
    }

    pub fn append(&mut self, entry: &AuditEntry) -> animus_core::Result<()> {
        let json = serde_json::to_string(entry)?;
        writeln!(self.file, "{json}")?;
        self.file.flush()?;
        self.count += 1;
        Ok(())
    }

    pub fn entry_count(&self) -> usize {
        self.count
    }

    pub fn read_all(path: &Path) -> animus_core::Result<Vec<AuditEntry>> {
        Self::read_recent(path, usize::MAX)
    }

    /// Read the most recent `limit` entries from the audit trail.
    /// Reads the entire file but only retains the last `limit` entries,
    /// preventing unbounded memory growth for large audit files.
    pub fn read_recent(path: &Path, limit: usize) -> animus_core::Result<Vec<AuditEntry>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = std::collections::VecDeque::with_capacity(limit.min(1024));
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: AuditEntry = serde_json::from_str(&line)?;
            if entries.len() == limit {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
        Ok(entries.into())
    }
}
