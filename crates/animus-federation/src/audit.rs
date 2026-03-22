use animus_core::{GoalId, InstanceId, SegmentId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

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

/// Append-only audit trail backed by a JSON lines file.
pub struct FederationAuditTrail {
    file: File,
    count: usize,
}

impl FederationAuditTrail {
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

    pub fn append(&mut self, entry: &FederationAuditEntry) -> animus_core::Result<()> {
        let json = serde_json::to_string(entry)?;
        writeln!(self.file, "{json}")?;
        self.file.flush()?;
        self.count += 1;
        Ok(())
    }

    pub fn entry_count(&self) -> usize {
        self.count
    }

    pub fn read_all(path: &Path) -> animus_core::Result<Vec<FederationAuditEntry>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: FederationAuditEntry = serde_json::from_str(&line)?;
            entries.push(entry);
        }
        Ok(entries)
    }
}
