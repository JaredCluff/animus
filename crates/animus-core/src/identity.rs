use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a Segment in VectorFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SegmentId(pub Uuid);

impl SegmentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SegmentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SegmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for an AILF instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub Uuid);

impl InstanceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for InstanceId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a reasoning thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(pub Uuid);

impl ThreadId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ThreadId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a Sensorium event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a consent policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyId(pub Uuid);

impl PolicyId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for PolicyId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PolicyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoalId(pub Uuid);

impl GoalId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for GoalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for GoalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub Uuid);

impl SnapshotId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SnapshotId {
    fn default() -> Self {
        Self::new()
    }
}

/// Persistent identity for an AILF instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimusIdentity {
    /// Ed25519 signing key (private). Serialized as bytes.
    #[serde(with = "signing_key_serde")]
    pub signing_key: SigningKey,
    /// Unique instance ID, immutable after birth.
    pub instance_id: InstanceId,
    /// Parent instance if this AILF was forked/cloned.
    pub parent_id: Option<InstanceId>,
    /// Timestamp of creation.
    pub born: chrono::DateTime<chrono::Utc>,
    /// Generation: 0 = original, 1 = first fork, etc.
    pub generation: u32,
    /// Which LLM model powers reasoning.
    pub base_model: String,
}

impl AnimusIdentity {
    /// Generate a new identity for a fresh AILF.
    pub fn generate(base_model: String) -> Self {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        Self {
            signing_key,
            instance_id: InstanceId::new(),
            parent_id: None,
            born: chrono::Utc::now(),
            generation: 0,
            base_model,
        }
    }

    /// Create a fork of this identity — a new AILF that shares lineage.
    /// The fork gets a new keypair and instance ID but records this identity as parent.
    pub fn fork(&self) -> Self {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        Self {
            signing_key,
            instance_id: InstanceId::new(),
            parent_id: Some(self.instance_id),
            born: chrono::Utc::now(),
            generation: self.generation + 1,
            base_model: self.base_model.clone(),
        }
    }

    /// Get the public verifying key.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Load identity from a file, or generate and save if not found.
    pub fn load_or_generate(path: &std::path::Path, base_model: &str) -> crate::Result<Self> {
        if path.exists() {
            let metadata = std::fs::metadata(path)?;
            if metadata.len() > 1_048_576 {
                return Err(crate::AnimusError::Identity(
                    format!("identity file too large: {} bytes (max 1 MiB)", metadata.len())
                ));
            }
            let data = std::fs::read(path)?;
            let identity: Self = bincode::deserialize(&data)
                .map_err(|e| crate::AnimusError::Identity(
                    format!("failed to load identity from {}: {e}", path.display())
                ))?;
            Ok(identity)
        } else {
            let identity = Self::generate(base_model.to_string());
            let data = bincode::serialize(&identity)
                .map_err(|e| crate::AnimusError::Identity(format!("failed to serialize identity: {e}")))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp_path = path.with_extension("bin.tmp");
            std::fs::write(&tmp_path, &data)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
            }

            std::fs::rename(&tmp_path, path)?;
            Ok(identity)
        }
    }
}

/// Serde helper for SigningKey (serialize as 32-byte array).
mod signing_key_serde {
    use ed25519_dalek::SigningKey;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(key: &SigningKey, s: S) -> Result<S::Ok, S::Error> {
        key.to_bytes().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SigningKey, D::Error> {
        let bytes = <[u8; 32]>::deserialize(d)?;
        Ok(SigningKey::from_bytes(&bytes))
    }
}
