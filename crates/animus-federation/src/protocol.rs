use animus_core::{GoalId, InstanceId, SegmentId};
use animus_cortex::Priority;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Protocol version — increment on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Content kind — mirrors Content enum variants for type-safe announcements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentKind {
    Text,
    Structured,
    Binary,
    Reference,
}

/// Broadcast: "I have this knowledge, here's the embedding + metadata."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentAnnouncement {
    pub segment_id: SegmentId,
    pub embedding: Vec<f32>,
    pub content_kind: ContentKind,
    pub created: DateTime<Utc>,
    pub tags: HashMap<String, String>,
}

/// Full segment transfer response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentTransfer {
    pub segment: animus_core::Segment,
    pub source_ailf: InstanceId,
    pub signature_hex: String,
}

/// Federated goal announcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalAnnouncement {
    pub goal_id: GoalId,
    pub description: String,
    pub priority: Priority,
    pub source_ailf: InstanceId,
}

/// Goal status update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalStatusUpdate {
    pub goal_id: GoalId,
    pub completed: bool,
    pub summary: Option<String>,
}

/// Handshake request (initiator → responder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub instance_id: InstanceId,
    pub verifying_key_hex: String,
    pub nonce: [u8; 32],
    pub protocol_version: u32,
}

/// Handshake response (responder → initiator).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub instance_id: InstanceId,
    pub verifying_key_hex: String,
    pub signature_hex: String,
    pub counter_nonce: [u8; 32],
    pub protocol_version: u32,
}

/// Handshake confirmation (initiator → responder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeConfirm {
    pub signature_hex: String,
}
