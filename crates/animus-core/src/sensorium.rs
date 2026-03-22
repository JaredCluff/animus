use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{EventId, PolicyId, SegmentId};

/// A normalized event from a sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorEvent {
    pub id: EventId,
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub source: String,
    pub data: serde_json::Value,
    pub consent_policy: Option<PolicyId>,
}

/// Categories of events the Sensorium can capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    FileChange,
    ProcessLifecycle,
    SystemResources,
    Network,
    Clipboard,
    WindowFocus,
    UsbDevice,
}

/// Human-defined boundaries on AILF observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentPolicy {
    pub id: PolicyId,
    pub name: String,
    pub rules: Vec<ConsentRule>,
    pub active: bool,
    pub created: DateTime<Utc>,
}

/// A single consent rule within a policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRule {
    pub event_types: Vec<EventType>,
    pub scope: Scope,
    pub permission: Permission,
    pub audit_level: AuditLevel,
}

/// Defines what the consent rule applies to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Scope {
    PathGlob(String),
    ProcessName(String),
    All,
}

/// Whether events matching this rule are permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    Allow,
    Deny,
    AllowAnonymized,
}

/// How much detail to record in the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditLevel {
    None,
    MetadataOnly,
    Full,
}

/// An entry in the append-only audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub event_id: EventId,
    pub consent_policy: Option<PolicyId>,
    pub attention_tier_reached: u8,
    pub action_taken: AuditAction,
    pub segment_created: Option<SegmentId>,
}

/// What the system did with an observed event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditAction {
    Logged,
    Promoted,
    Ignored,
    DeniedByConsent,
}

/// Result of the attention filter evaluating an event.
#[derive(Debug, Clone)]
pub enum AttentionDecision {
    Pass { promoted: bool },
    Drop { reason: String },
}
