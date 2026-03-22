use animus_core::{GoalId, InstanceId, SegmentId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Actions recorded in the federation audit trail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FederationAuditAction {
    HandshakeCompleted,
    SegmentPublished,
    SegmentReceived,
    SegmentRequestDenied,
    GoalReceived,
    GoalStatusUpdated,
    PeerBlocked,
    PeerTrusted,
}

/// A single entry in the federation audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub action: FederationAuditAction,
    pub peer_instance_id: InstanceId,
    pub segment_id: Option<SegmentId>,
    pub goal_id: Option<GoalId>,
}

/// Default max file size before rotation: 10 MiB.
const DEFAULT_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Number of rotated files to keep.
const MAX_ROTATED_FILES: u32 = 5;

/// Append-only audit trail backed by a JSON lines file with size-based rotation.
pub struct FederationAuditTrail {
    path: PathBuf,
    file: File,
    count: usize,
    max_file_size: u64,
}

impl FederationAuditTrail {
    pub fn open(path: &Path) -> animus_core::Result<Self> {
        Self::open_with_max_size(path, DEFAULT_MAX_FILE_SIZE)
    }

    pub fn open_with_max_size(path: &Path, max_file_size: u64) -> animus_core::Result<Self> {
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

        Ok(Self {
            path: path.to_path_buf(),
            file,
            count,
            max_file_size,
        })
    }

    pub fn append(&mut self, entry: &FederationAuditEntry) -> animus_core::Result<()> {
        let json = serde_json::to_string(entry)?;
        writeln!(self.file, "{json}")?;
        self.file.flush()?;
        self.count += 1;

        if self.needs_rotation() {
            self.rotate()?;
        }

        Ok(())
    }

    fn needs_rotation(&self) -> bool {
        fs::metadata(&self.path)
            .map(|m| m.len() >= self.max_file_size)
            .unwrap_or(false)
    }

    fn rotate(&mut self) -> animus_core::Result<()> {
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

        let rotated = self.path.with_extension("jsonl.1");
        fs::rename(&self.path, &rotated)?;

        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.count = 0;

        Ok(())
    }

    pub fn entry_count(&self) -> usize {
        self.count
    }

    pub fn read_all(path: &Path) -> animus_core::Result<Vec<FederationAuditEntry>> {
        Self::read_recent(path, usize::MAX)
    }

    /// Read the most recent `limit` entries from the federation audit trail.
    /// Reads the entire file but only retains the last `limit` entries,
    /// preventing unbounded memory growth for large audit files.
    pub fn read_recent(path: &Path, limit: usize) -> animus_core::Result<Vec<FederationAuditEntry>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = std::collections::VecDeque::with_capacity(limit.min(1024));
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: FederationAuditEntry = serde_json::from_str(&line)?;
            if entries.len() == limit {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
        Ok(entries.into())
    }
}
