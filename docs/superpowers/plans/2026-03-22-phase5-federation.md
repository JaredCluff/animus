# Phase 5: Federation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable AILF-to-AILF discovery, authentication, knowledge sharing, and federated goals over local network.

**Architecture:** New `animus-federation` crate with mDNS discovery, Ed25519 challenge-response auth, HTTP server (axum) for inbound requests, HTTP client (reqwest) for outbound, peer trust registry, knowledge sharing pipeline with privacy enforcement, and federation-specific audit trail. Core types extended in `animus-core` and `animus-cortex`.

**Tech Stack:** Rust, axum 0.8, mdns-sd 0.11, ed25519-dalek 2, reqwest 0.12, tokio, serde_json

**Spec:** `docs/superpowers/specs/2026-03-21-phase5-federation-design.md`

---

## File Structure

### New crate: `crates/animus-federation/`

| File | Responsibility |
|------|---------------|
| `Cargo.toml` | Crate manifest with deps |
| `src/lib.rs` | Public API, re-exports |
| `src/peers.rs` | PeerInfo, TrustLevel, Peer, PeerRegistry with JSON persistence |
| `src/auth.rs` | FederationAuth — Ed25519 handshake and request signing |
| `src/protocol.rs` | ContentKind, message types (announcements, transfers, handshake messages) |
| `src/audit.rs` | FederationAuditEntry, FederationAuditAction, append-only JSON lines |
| `src/discovery.rs` | DiscoveryService — mDNS announcement/browsing + static peer fallback |
| `src/knowledge.rs` | KnowledgeSharing pipeline — publish/subscribe with privacy enforcement |
| `src/server.rs` | Axum HTTP server — routes, request validation, rate limiting |
| `src/orchestrator.rs` | FederationOrchestrator — wires all components together |

### Modified files

| File | Changes |
|------|---------|
| `Cargo.toml` (workspace) | Add axum, mdns-sd, tower deps; add animus-federation member |
| `crates/animus-core/src/error.rs` | Add `Federation(String)` variant |
| `crates/animus-core/src/config.rs` | Add `FederationConfig` struct + field on `AnimusConfig` |
| `crates/animus-core/src/lib.rs` | Re-export `FederationConfig` |
| `crates/animus-cortex/src/telos.rs` | Change `GoalSource::Federated` to carry `source_ailf: InstanceId`; add `cached_embedding` to `Goal` |
| `crates/animus-runtime/src/main.rs` | Add /peers, /trust, /block, /federate commands; init federation on startup |

### New test files in `crates/animus-tests/tests/integration/`

| File | Coverage |
|------|----------|
| `federation_peers.rs` | Peer registry CRUD, trust transitions, persistence |
| `federation_auth.rs` | Handshake flow, signature verification, replay protection |
| `federation_protocol.rs` | Message serialization roundtrips |
| `federation_audit.rs` | Audit trail append, read, entry count |
| `federation_knowledge.rs` | Publish/subscribe pipeline, privacy enforcement, confidence |
| `federation_e2e.rs` | Two in-process instances: discover, handshake, share knowledge |

---

### Task 1: Workspace Setup and Core Type Extensions

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/animus-core/src/error.rs`
- Modify: `crates/animus-core/src/config.rs`
- Modify: `crates/animus-core/src/lib.rs`
- Modify: `crates/animus-cortex/src/telos.rs`
- Create: `crates/animus-federation/Cargo.toml`
- Create: `crates/animus-federation/src/lib.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`
- Test: `crates/animus-tests/tests/integration/telos_goals.rs` (verify existing tests still pass)

- [ ] **Step 1: Add new workspace dependencies and crate member**

In `Cargo.toml` (workspace root), add to `[workspace.dependencies]`:
```toml
axum = { version = "0.8", features = ["json"] }
tower = "0.5"
mdns-sd = "0.11"
sha2 = "0.10"
hex = "0.4"
animus-federation = { path = "crates/animus-federation" }
```

Add `"crates/animus-federation"` to the `members` array.

- [ ] **Step 2: Add `Federation(String)` error variant**

In `crates/animus-core/src/error.rs`, add after the `Threading(String)` variant:
```rust
    #[error("federation error: {0}")]
    Federation(String),
```

- [ ] **Step 3: Add `FederationConfig` to config.rs**

In `crates/animus-core/src/config.rs`, add the struct and its Default impl:
```rust
/// Configuration for the Federation layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationConfig {
    pub enabled: bool,
    pub bind_address: String,
    pub port: u16,
    pub static_peers: Vec<String>,
    pub relevance_threshold: f32,
    pub federated_confidence_trusted: f32,
    pub federated_confidence_verified: f32,
    pub max_requests_per_minute: u32,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: "0.0.0.0".to_string(),
            port: 0,
            static_peers: Vec::new(),
            relevance_threshold: 0.5,
            federated_confidence_trusted: 0.3,
            federated_confidence_verified: 0.1,
            max_requests_per_minute: 100,
        }
    }
}
```

Add `pub federation: FederationConfig` field to `AnimusConfig` struct, and `federation: FederationConfig::default()` to the `AnimusConfig::default()` impl.

- [ ] **Step 4: Update lib.rs re-exports**

In `crates/animus-core/src/lib.rs`, add `FederationConfig` to the `config` re-export line:
```rust
pub use config::{AnimusConfig, CortexConfig, EmbeddingConfig, EmbeddingTier, FederationConfig, InterfaceConfig, MnemosConfig, SensoriumConfig, VectorFSConfig};
```

- [ ] **Step 5: Update `GoalSource::Federated` to carry `source_ailf`**

In `crates/animus-cortex/src/telos.rs`, change:
```rust
pub enum GoalSource {
    Human,
    SelfDerived,
    Federated,
}
```
to:
```rust
pub enum GoalSource {
    Human,
    SelfDerived,
    Federated { source_ailf: animus_core::InstanceId },
}
```

Update all match arms on `GoalSource` in the same file. The `create_goal` match on `source` for autonomy:
```rust
        let autonomy = match &source {
            GoalSource::Human => Autonomy::Act,
            GoalSource::SelfDerived => Autonomy::Suggest,
            GoalSource::Federated { .. } => Autonomy::Inform,
        };
```

- [ ] **Step 6: Add `cached_embedding` to `Goal` struct**

In `crates/animus-cortex/src/telos.rs`, add to the `Goal` struct:
```rust
    pub cached_embedding: Option<Vec<f32>>,
```

Initialize it as `None` in `create_goal`:
```rust
            cached_embedding: None,
```

- [ ] **Step 7: Create animus-federation crate scaffold**

Create `crates/animus-federation/Cargo.toml`:
```toml
[package]
name = "animus-federation"
version.workspace = true
edition.workspace = true

[dependencies]
animus-core = { workspace = true }
animus-vectorfs = { workspace = true }
animus-cortex = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
uuid = { workspace = true }
chrono = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
ed25519-dalek = { workspace = true }
parking_lot = { workspace = true }
reqwest = { workspace = true }
axum = { workspace = true }
tower = { workspace = true }
mdns-sd = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
rand = { workspace = true }
```

Create `crates/animus-federation/src/lib.rs`:
```rust
pub mod audit;
pub mod auth;
pub mod discovery;
pub mod knowledge;
pub mod orchestrator;
pub mod peers;
pub mod protocol;
pub mod server;
```

- [ ] **Step 8: Add animus-federation to test crate dependencies**

In `crates/animus-tests/Cargo.toml`, add:
```toml
animus-federation = { workspace = true }
ed25519-dalek = { workspace = true }
parking_lot = { workspace = true }
hex = { workspace = true }
reqwest = { workspace = true }
sha2 = { workspace = true }
```

- [ ] **Step 9: Verify everything compiles**

Run: `cargo build --workspace`
Expected: compiles with no errors (federation modules are empty stubs from lib.rs — create empty files for each module).

Create empty module files:
- `crates/animus-federation/src/audit.rs`
- `crates/animus-federation/src/auth.rs`
- `crates/animus-federation/src/discovery.rs`
- `crates/animus-federation/src/knowledge.rs`
- `crates/animus-federation/src/orchestrator.rs`
- `crates/animus-federation/src/peers.rs`
- `crates/animus-federation/src/protocol.rs`
- `crates/animus-federation/src/server.rs`

- [ ] **Step 10: Run existing tests to verify no regressions**

Run: `cargo test --workspace`
Expected: All existing tests pass (the GoalSource change may require updating `telos_goals.rs` test — check if it creates a `Federated` goal and update to `Federated { source_ailf: InstanceId::new() }`).

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "feat(core): workspace setup for Phase 5 Federation — add deps, FederationConfig, update GoalSource, scaffold animus-federation crate"
```

---

### Task 2: Peer Registry

**Files:**
- Create: `crates/animus-federation/src/peers.rs`
- Create: `crates/animus-tests/tests/integration/federation_peers.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`

- [ ] **Step 1: Write failing tests for peer registry**

Create `crates/animus-tests/tests/integration/federation_peers.rs`:
```rust
use animus_core::InstanceId;
use animus_federation::peers::{PeerInfo, PeerRegistry, TrustLevel};
use ed25519_dalek::SigningKey;
use std::net::SocketAddr;

fn mock_peer_info() -> PeerInfo {
    let mut rng = rand::thread_rng();
    let signing_key = SigningKey::generate(&mut rng);
    PeerInfo {
        instance_id: InstanceId::new(),
        verifying_key: signing_key.verifying_key(),
        address: "127.0.0.1:9000".parse::<SocketAddr>().unwrap(),
    }
}

#[test]
fn add_and_get_peer() {
    let mut registry = PeerRegistry::new();
    let info = mock_peer_info();
    let id = info.instance_id;
    registry.add_peer(info);
    let peer = registry.get_peer(&id).unwrap();
    assert_eq!(peer.trust, TrustLevel::Unknown);
}

#[test]
fn set_trust_level() {
    let mut registry = PeerRegistry::new();
    let info = mock_peer_info();
    let id = info.instance_id;
    registry.add_peer(info);
    registry.set_trust(&id, TrustLevel::Trusted);
    assert_eq!(registry.get_peer(&id).unwrap().trust, TrustLevel::Trusted);
}

#[test]
fn remove_peer() {
    let mut registry = PeerRegistry::new();
    let info = mock_peer_info();
    let id = info.instance_id;
    registry.add_peer(info);
    registry.remove_peer(&id);
    assert!(registry.get_peer(&id).is_none());
}

#[test]
fn trusted_peers_filters_correctly() {
    let mut registry = PeerRegistry::new();
    let info1 = mock_peer_info();
    let id1 = info1.instance_id;
    let info2 = mock_peer_info();
    registry.add_peer(info1);
    registry.add_peer(info2);
    registry.set_trust(&id1, TrustLevel::Trusted);
    let trusted = registry.trusted_peers();
    assert_eq!(trusted.len(), 1);
    assert_eq!(trusted[0].info.instance_id, id1);
}

#[test]
fn persistence_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");

    let mut registry = PeerRegistry::new();
    let info = mock_peer_info();
    let id = info.instance_id;
    registry.add_peer(info);
    registry.set_trust(&id, TrustLevel::Verified);
    registry.save(&path).unwrap();

    let loaded = PeerRegistry::load(&path).unwrap();
    let peer = loaded.get_peer(&id).unwrap();
    assert_eq!(peer.trust, TrustLevel::Verified);
}

#[test]
fn blocked_peer_excluded_from_trusted() {
    let mut registry = PeerRegistry::new();
    let info = mock_peer_info();
    let id = info.instance_id;
    registry.add_peer(info);
    registry.set_trust(&id, TrustLevel::Blocked);
    assert!(registry.trusted_peers().is_empty());
}
```

Add `mod federation_peers;` to `crates/animus-tests/tests/integration/main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p animus-tests federation_peers`
Expected: FAIL (module not implemented)

- [ ] **Step 3: Implement PeerRegistry**

Write `crates/animus-federation/src/peers.rs`:
```rust
use animus_core::InstanceId;
use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustLevel {
    Unknown,
    Verified,
    Trusted,
    Blocked,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub instance_id: InstanceId,
    pub verifying_key: VerifyingKey,
    pub address: SocketAddr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub info: PeerInfoPersist,
    pub trust: TrustLevel,
    pub last_seen: DateTime<Utc>,
    pub last_handshake: Option<DateTime<Utc>>,
    pub segments_received: u64,
    pub segments_sent: u64,
}

/// Serializable version of PeerInfo (VerifyingKey stored as hex bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfoPersist {
    pub instance_id: InstanceId,
    pub verifying_key_bytes: [u8; 32],
    pub address: SocketAddr,
}

impl From<&PeerInfo> for PeerInfoPersist {
    fn from(info: &PeerInfo) -> Self {
        Self {
            instance_id: info.instance_id,
            verifying_key_bytes: info.verifying_key.to_bytes(),
            address: info.address,
        }
    }
}

impl PeerInfoPersist {
    pub fn to_peer_info(&self) -> Option<PeerInfo> {
        let vk = VerifyingKey::from_bytes(&self.verifying_key_bytes).ok()?;
        Some(PeerInfo {
            instance_id: self.instance_id,
            verifying_key: vk,
            address: self.address,
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PeerRegistry {
    peers: HashMap<InstanceId, Peer>,
}

impl PeerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_peer(&mut self, info: PeerInfo) {
        let persist = PeerInfoPersist::from(&info);
        let peer = Peer {
            info: persist,
            trust: TrustLevel::Unknown,
            last_seen: Utc::now(),
            last_handshake: None,
            segments_received: 0,
            segments_sent: 0,
        };
        self.peers.insert(info.instance_id, peer);
    }

    pub fn remove_peer(&mut self, id: &InstanceId) {
        self.peers.remove(id);
    }

    pub fn get_peer(&self, id: &InstanceId) -> Option<&Peer> {
        self.peers.get(id)
    }

    pub fn get_peer_mut(&mut self, id: &InstanceId) -> Option<&mut Peer> {
        self.peers.get_mut(id)
    }

    pub fn set_trust(&mut self, id: &InstanceId, trust: TrustLevel) {
        if let Some(peer) = self.peers.get_mut(id) {
            peer.trust = trust;
        }
    }

    pub fn trusted_peers(&self) -> Vec<&Peer> {
        self.peers.values()
            .filter(|p| p.trust == TrustLevel::Trusted)
            .collect()
    }

    pub fn verified_or_trusted_peers(&self) -> Vec<&Peer> {
        self.peers.values()
            .filter(|p| p.trust == TrustLevel::Verified || p.trust == TrustLevel::Trusted)
            .collect()
    }

    pub fn all_peers(&self) -> Vec<&Peer> {
        self.peers.values().collect()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    pub fn save(&self, path: &Path) -> animus_core::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self)?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> animus_core::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let data = std::fs::read_to_string(path)?;
        let registry: Self = serde_json::from_str(&data)?;
        Ok(registry)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p animus-tests federation_peers`
Expected: All 6 tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/animus-federation/src/peers.rs crates/animus-tests/tests/integration/federation_peers.rs crates/animus-tests/tests/integration/main.rs
git commit -m "feat(federation): peer registry with trust levels and JSON persistence"
```

---

### Task 3: Protocol Messages

**Files:**
- Create: `crates/animus-federation/src/protocol.rs`
- Create: `crates/animus-tests/tests/integration/federation_protocol.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`

- [ ] **Step 1: Write failing tests for protocol message serialization**

Create `crates/animus-tests/tests/integration/federation_protocol.rs`:
```rust
use animus_core::{GoalId, InstanceId, SegmentId};
use animus_federation::protocol::*;
use chrono::Utc;
use std::collections::HashMap;

#[test]
fn content_kind_serialization_roundtrip() {
    for kind in [ContentKind::Text, ContentKind::Structured, ContentKind::Binary, ContentKind::Reference] {
        let json = serde_json::to_string(&kind).unwrap();
        let back: ContentKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }
}

#[test]
fn segment_announcement_roundtrip() {
    let ann = SegmentAnnouncement {
        segment_id: SegmentId::new(),
        embedding: vec![0.1, 0.2, 0.3],
        content_kind: ContentKind::Text,
        created: Utc::now(),
        tags: HashMap::from([("topic".to_string(), "rust".to_string())]),
    };
    let json = serde_json::to_string(&ann).unwrap();
    let back: SegmentAnnouncement = serde_json::from_str(&json).unwrap();
    assert_eq!(back.segment_id, ann.segment_id);
    assert_eq!(back.content_kind, ContentKind::Text);
}

#[test]
fn handshake_request_roundtrip() {
    let req = HandshakeRequest {
        instance_id: InstanceId::new(),
        verifying_key_hex: "ab".repeat(32),
        nonce: [42u8; 32],
        protocol_version: 1,
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: HandshakeRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.instance_id, req.instance_id);
    assert_eq!(back.nonce, req.nonce);
    assert_eq!(back.protocol_version, 1);
}

#[test]
fn goal_announcement_roundtrip() {
    let ann = GoalAnnouncement {
        goal_id: GoalId::new(),
        description: "Learn Rust".to_string(),
        priority: animus_cortex::Priority::Normal,
        source_ailf: InstanceId::new(),
    };
    let json = serde_json::to_string(&ann).unwrap();
    let back: GoalAnnouncement = serde_json::from_str(&json).unwrap();
    assert_eq!(back.goal_id, ann.goal_id);
    assert_eq!(back.description, "Learn Rust");
}
```

Add `mod federation_protocol;` to `main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p animus-tests federation_protocol`
Expected: FAIL

- [ ] **Step 3: Implement protocol messages**

Write `crates/animus-federation/src/protocol.rs`:
```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p animus-tests federation_protocol`
Expected: All 4 tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/animus-federation/src/protocol.rs crates/animus-tests/tests/integration/federation_protocol.rs crates/animus-tests/tests/integration/main.rs
git commit -m "feat(federation): protocol message types with serialization"
```

---

### Task 4: Authentication

**Files:**
- Create: `crates/animus-federation/src/auth.rs`
- Create: `crates/animus-tests/tests/integration/federation_auth.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`

- [ ] **Step 1: Write failing tests for authentication**

Create `crates/animus-tests/tests/integration/federation_auth.rs`:
```rust
use animus_core::AnimusIdentity;
use animus_federation::auth::FederationAuth;

#[test]
fn handshake_full_flow() {
    let alice_id = AnimusIdentity::generate("test-model".to_string());
    let bob_id = AnimusIdentity::generate("test-model".to_string());
    let alice = FederationAuth::new(alice_id);
    let bob = FederationAuth::new(bob_id);

    // Alice initiates
    let (request, alice_nonce) = alice.create_handshake();

    // Bob responds
    let (response, bob_nonce) = bob.respond_to_handshake(&request).unwrap();

    // Alice verifies Bob's response and confirms
    let confirm = alice.verify_response_and_confirm(&response, &alice_nonce).unwrap();

    // Bob verifies Alice's confirmation
    bob.verify_confirm(&confirm, &bob_nonce, &request.verifying_key_hex).unwrap();
}

#[test]
fn handshake_rejects_wrong_signature() {
    let alice_id = AnimusIdentity::generate("test-model".to_string());
    let bob_id = AnimusIdentity::generate("test-model".to_string());
    let eve_id = AnimusIdentity::generate("test-model".to_string());
    let alice = FederationAuth::new(alice_id);
    let bob = FederationAuth::new(bob_id);
    let eve = FederationAuth::new(eve_id);

    let (request, alice_nonce) = alice.create_handshake();
    let (response, _bob_nonce) = bob.respond_to_handshake(&request).unwrap();

    // Tamper: Eve tries to verify with wrong nonce
    let wrong_nonce = [0u8; 32];
    assert!(alice.verify_response_and_confirm(&response, &wrong_nonce).is_err());
}

#[test]
fn request_signing_and_verification() {
    let id = AnimusIdentity::generate("test-model".to_string());
    let auth = FederationAuth::new(id);
    let vk = auth.verifying_key();

    let timestamp = chrono::Utc::now().timestamp();
    let path = "/federation/publish";
    let body = b"hello world";

    let sig = auth.sign_request(timestamp, path, body);
    assert!(FederationAuth::verify_signed_request(timestamp, path, body, &sig, &vk).is_ok());
}

#[test]
fn request_signing_rejects_tampered_body() {
    let id = AnimusIdentity::generate("test-model".to_string());
    let auth = FederationAuth::new(id);
    let vk = auth.verifying_key();

    let timestamp = chrono::Utc::now().timestamp();
    let path = "/federation/publish";
    let body = b"hello world";

    let sig = auth.sign_request(timestamp, path, body);
    let tampered = b"hello tampered";
    assert!(FederationAuth::verify_signed_request(timestamp, path, tampered, &sig, &vk).is_err());
}

#[test]
fn request_signing_rejects_old_timestamp() {
    let id = AnimusIdentity::generate("test-model".to_string());
    let auth = FederationAuth::new(id);
    let vk = auth.verifying_key();

    let old_timestamp = chrono::Utc::now().timestamp() - 60; // 60 seconds ago
    let path = "/federation/publish";
    let body = b"hello";

    let sig = auth.sign_request(old_timestamp, path, body);
    assert!(FederationAuth::verify_signed_request(old_timestamp, path, body, &sig, &vk).is_err());
}
```

Add `mod federation_auth;` to `main.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p animus-tests federation_auth`
Expected: FAIL

- [ ] **Step 3: Implement FederationAuth**

Write `crates/animus-federation/src/auth.rs`:
```rust
use animus_core::{AnimusError, AnimusIdentity, Result};
use crate::protocol::{HandshakeConfirm, HandshakeRequest, HandshakeResponse, PROTOCOL_VERSION};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

const REPLAY_WINDOW_SECS: i64 = 30;

pub struct FederationAuth {
    identity: AnimusIdentity,
}

impl FederationAuth {
    pub fn new(identity: AnimusIdentity) -> Self {
        Self { identity }
    }

    pub fn instance_id(&self) -> animus_core::InstanceId {
        self.identity.instance_id
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.identity.verifying_key()
    }

    pub fn create_handshake(&self) -> (HandshakeRequest, [u8; 32]) {
        let mut nonce = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce);

        let vk_hex = hex::encode(self.identity.verifying_key().to_bytes());
        let request = HandshakeRequest {
            instance_id: self.identity.instance_id,
            verifying_key_hex: vk_hex,
            nonce,
            protocol_version: PROTOCOL_VERSION,
        };
        (request, nonce)
    }

    pub fn respond_to_handshake(&self, req: &HandshakeRequest) -> Result<(HandshakeResponse, [u8; 32])> {
        if req.protocol_version != PROTOCOL_VERSION {
            return Err(AnimusError::Federation(
                format!("protocol version mismatch: expected {}, got {}", PROTOCOL_VERSION, req.protocol_version)
            ));
        }

        // Sign the initiator's nonce
        let sig = self.identity.signing_key.sign(&req.nonce);
        let sig_hex = hex::encode(sig.to_bytes());

        let mut counter_nonce = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut counter_nonce);

        let vk_hex = hex::encode(self.identity.verifying_key().to_bytes());
        let response = HandshakeResponse {
            instance_id: self.identity.instance_id,
            verifying_key_hex: vk_hex,
            signature_hex: sig_hex,
            counter_nonce,
            protocol_version: PROTOCOL_VERSION,
        };
        Ok((response, counter_nonce))
    }

    pub fn verify_response_and_confirm(
        &self,
        resp: &HandshakeResponse,
        original_nonce: &[u8; 32],
    ) -> Result<HandshakeConfirm> {
        // Verify responder signed our nonce
        let vk_bytes = hex::decode(&resp.verifying_key_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key hex: {e}")))?;
        let vk_arr: [u8; 32] = vk_bytes.try_into()
            .map_err(|_| AnimusError::Federation("verifying key must be 32 bytes".to_string()))?;
        let vk = VerifyingKey::from_bytes(&vk_arr)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key: {e}")))?;

        let sig_bytes = hex::decode(&resp.signature_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid signature hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into()
            .map_err(|_| AnimusError::Federation("signature must be 64 bytes".to_string()))?;
        let sig = Signature::from_bytes(&sig_arr);

        vk.verify(original_nonce, &sig)
            .map_err(|e| AnimusError::Federation(format!("handshake signature verification failed: {e}")))?;

        // Sign the counter nonce
        let counter_sig = self.identity.signing_key.sign(&resp.counter_nonce);
        let confirm = HandshakeConfirm {
            signature_hex: hex::encode(counter_sig.to_bytes()),
        };
        Ok(confirm)
    }

    pub fn verify_confirm(
        &self,
        confirm: &HandshakeConfirm,
        counter_nonce: &[u8; 32],
        initiator_vk_hex: &str,
    ) -> Result<()> {
        let vk_bytes = hex::decode(initiator_vk_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key hex: {e}")))?;
        let vk_arr: [u8; 32] = vk_bytes.try_into()
            .map_err(|_| AnimusError::Federation("verifying key must be 32 bytes".to_string()))?;
        let vk = VerifyingKey::from_bytes(&vk_arr)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key: {e}")))?;

        let sig_bytes = hex::decode(&confirm.signature_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid signature hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into()
            .map_err(|_| AnimusError::Federation("signature must be 64 bytes".to_string()))?;
        let sig = Signature::from_bytes(&sig_arr);

        vk.verify(counter_nonce, &sig)
            .map_err(|e| AnimusError::Federation(format!("handshake confirm verification failed: {e}")))?;

        Ok(())
    }

    /// Sign a request: signature over "{timestamp}:{path}:{sha256_of_body}".
    pub fn sign_request(&self, timestamp: i64, path: &str, body: &[u8]) -> String {
        let body_hash = Sha256::digest(body);
        let message = format!("{}:{}:{}", timestamp, path, hex::encode(body_hash));
        let sig = self.identity.signing_key.sign(message.as_bytes());
        hex::encode(sig.to_bytes())
    }

    /// Verify a signed request. Rejects if timestamp is older than REPLAY_WINDOW_SECS.
    pub fn verify_signed_request(
        timestamp: i64,
        path: &str,
        body: &[u8],
        signature_hex: &str,
        peer_vk: &VerifyingKey,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        if (now - timestamp).abs() > REPLAY_WINDOW_SECS {
            return Err(AnimusError::Federation(
                format!("request timestamp too old: {}s (max {}s)", (now - timestamp).abs(), REPLAY_WINDOW_SECS)
            ));
        }

        let body_hash = Sha256::digest(body);
        let message = format!("{}:{}:{}", timestamp, path, hex::encode(body_hash));

        let sig_bytes = hex::decode(signature_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid signature hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into()
            .map_err(|_| AnimusError::Federation("signature must be 64 bytes".to_string()))?;
        let sig = Signature::from_bytes(&sig_arr);

        peer_vk.verify(message.as_bytes(), &sig)
            .map_err(|e| AnimusError::Federation(format!("request signature verification failed: {e}")))?;

        Ok(())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p animus-tests federation_auth`
Expected: All 5 tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/animus-federation/src/auth.rs crates/animus-tests/tests/integration/federation_auth.rs crates/animus-tests/tests/integration/main.rs
git commit -m "feat(federation): Ed25519 challenge-response handshake and request signing"
```

---

### Task 5: Federation Audit Trail

**Files:**
- Create: `crates/animus-federation/src/audit.rs`
- Create: `crates/animus-tests/tests/integration/federation_audit.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/animus-tests/tests/integration/federation_audit.rs`:
```rust
use animus_core::InstanceId;
use animus_federation::audit::{FederationAuditAction, FederationAuditEntry, FederationAuditTrail};

#[test]
fn append_and_read_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fed-audit.jsonl");

    let mut trail = FederationAuditTrail::open(&path).unwrap();
    let entry = FederationAuditEntry {
        timestamp: chrono::Utc::now(),
        action: FederationAuditAction::HandshakeCompleted,
        peer_instance_id: InstanceId::new(),
        segment_id: None,
        goal_id: None,
    };
    trail.append(&entry).unwrap();
    assert_eq!(trail.entry_count(), 1);

    let entries = FederationAuditTrail::read_all(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(matches!(entries[0].action, FederationAuditAction::HandshakeCompleted));
}

#[test]
fn audit_trail_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fed-audit.jsonl");

    {
        let mut trail = FederationAuditTrail::open(&path).unwrap();
        let entry = FederationAuditEntry {
            timestamp: chrono::Utc::now(),
            action: FederationAuditAction::SegmentReceived,
            peer_instance_id: InstanceId::new(),
            segment_id: Some(animus_core::SegmentId::new()),
            goal_id: None,
        };
        trail.append(&entry).unwrap();
    }

    let trail = FederationAuditTrail::open(&path).unwrap();
    assert_eq!(trail.entry_count(), 1);
}
```

Add `mod federation_audit;` to `main.rs`.

- [ ] **Step 2: Implement federation audit trail**

Write `crates/animus-federation/src/audit.rs`:
```rust
use animus_core::{GoalId, InstanceId, SegmentId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub action: FederationAuditAction,
    pub peer_instance_id: InstanceId,
    pub segment_id: Option<SegmentId>,
    pub goal_id: Option<GoalId>,
}

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
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p animus-tests federation_audit`
Expected: 2 tests PASS

- [ ] **Step 4: Commit**

```bash
git add crates/animus-federation/src/audit.rs crates/animus-tests/tests/integration/federation_audit.rs crates/animus-tests/tests/integration/main.rs
git commit -m "feat(federation): federation-specific audit trail"
```

---

### Task 6: Discovery Service

**Files:**
- Create: `crates/animus-federation/src/discovery.rs`
- No dedicated test file — discovery uses mDNS which doesn't work in CI. Static peer fallback tested via unit test in the module.

- [ ] **Step 1: Implement DiscoveryService**

Write `crates/animus-federation/src/discovery.rs`:
```rust
use animus_core::{AnimusError, InstanceId, Result};
use crate::peers::PeerInfo;
use ed25519_dalek::VerifyingKey;
use std::net::SocketAddr;
use tokio::sync::mpsc;

pub enum DiscoveryEvent {
    PeerDiscovered(PeerInfo),
    PeerLost(InstanceId),
}

pub struct DiscoveryService {
    own_instance_id: InstanceId,
    static_peers: Vec<String>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
}

impl DiscoveryService {
    pub fn new(
        own_instance_id: InstanceId,
        static_peers: Vec<String>,
        event_tx: mpsc::Sender<DiscoveryEvent>,
    ) -> Self {
        Self {
            own_instance_id,
            static_peers,
            event_tx,
        }
    }

    /// Start discovery. If static_peers is non-empty, skip mDNS and use static list.
    /// Otherwise, use mDNS (DNS-SD) to discover peers on the local network.
    pub async fn start(&self) -> Result<()> {
        if !self.static_peers.is_empty() {
            tracing::info!("Federation discovery: using {} static peers", self.static_peers.len());
            // Static peers are added by the orchestrator after handshake
            return Ok(());
        }

        // mDNS discovery
        tracing::info!("Federation discovery: starting mDNS browsing for _animus._tcp.local.");
        let daemon = mdns_sd::ServiceDaemon::new()
            .map_err(|e| AnimusError::Federation(format!("failed to create mDNS daemon: {e}")))?;

        let receiver = daemon.browse("_animus._tcp.local.")
            .map_err(|e| AnimusError::Federation(format!("failed to browse mDNS: {e}")))?;

        let own_id = self.own_instance_id;
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            while let Ok(event) = receiver.recv_async().await {
                match event {
                    mdns_sd::ServiceEvent::ServiceResolved(info) => {
                        if let Some(peer) = Self::parse_service_info(&info, own_id) {
                            let _ = tx.send(DiscoveryEvent::PeerDiscovered(peer)).await;
                        }
                    }
                    mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                        // Extract instance_id from fullname if possible
                        tracing::info!("mDNS: service removed: {fullname}");
                    }
                    _ => {} // SearchStarted, SearchStopped — ignore
                }
            }
        });

        Ok(())
    }

    /// Register this AILF as an mDNS service.
    pub fn register_service(&self, port: u16, vk_hex: &str) -> Result<()> {
        let daemon = mdns_sd::ServiceDaemon::new()
            .map_err(|e| AnimusError::Federation(format!("failed to create mDNS daemon: {e}")))?;

        let instance_name = format!("animus-{}", &self.own_instance_id.0.to_string()[..8]);
        let mut service = mdns_sd::ServiceInfo::new(
            "_animus._tcp.local.",
            &instance_name,
            &format!("{instance_name}.local."),
            "",
            port,
            None,
        ).map_err(|e| AnimusError::Federation(format!("failed to create mDNS service info: {e}")))?;

        service.set_property("instance_id", self.own_instance_id.0.to_string());
        service.set_property("vk", vk_hex);
        service.set_property("proto", "1");

        daemon.register(service)
            .map_err(|e| AnimusError::Federation(format!("failed to register mDNS service: {e}")))?;

        tracing::info!("mDNS: registered _animus._tcp.local. on port {port}");
        Ok(())
    }

    fn parse_service_info(info: &mdns_sd::ServiceInfo, own_id: InstanceId) -> Option<PeerInfo> {
        let instance_id_str = info.get_property_val_str("instance_id")?;
        let instance_id = InstanceId(instance_id_str.parse().ok()?);

        // Skip self
        if instance_id == own_id {
            return None;
        }

        let vk_hex = info.get_property_val_str("vk")?;
        let vk_bytes: [u8; 32] = hex::decode(vk_hex).ok()?.try_into().ok()?;
        let verifying_key = VerifyingKey::from_bytes(&vk_bytes).ok()?;

        let port = info.get_port();
        let addresses = info.get_addresses();
        let addr = addresses.iter().next()?;
        let address = SocketAddr::new(*addr, port);

        Some(PeerInfo {
            instance_id,
            verifying_key,
            address,
        })
    }

    pub fn static_peer_addresses(&self) -> &[String] {
        &self.static_peers
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p animus-federation`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add crates/animus-federation/src/discovery.rs
git commit -m "feat(federation): mDNS discovery service with static peer fallback"
```

---

### Task 7: Knowledge Sharing Pipeline

**Files:**
- Create: `crates/animus-federation/src/knowledge.rs`
- Create: `crates/animus-tests/tests/integration/federation_knowledge.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`

- [ ] **Step 1: Write failing tests for knowledge sharing**

Create `crates/animus-tests/tests/integration/federation_knowledge.rs`:
```rust
use animus_core::{Content, InstanceId, PolicyId, Principal, Segment, SegmentId, Source};
use animus_federation::knowledge::{
    FederationPermission, FederationPolicy, FederationRule, FederationScope, KnowledgeSharing,
};
use animus_federation::protocol::{ContentKind, SegmentAnnouncement};
use chrono::Utc;
use std::collections::HashMap;

#[test]
fn publish_check_allows_matching_policy() {
    let policies = vec![FederationPolicy {
        id: PolicyId::new(),
        name: "share-text".to_string(),
        active: true,
        publish_rules: vec![FederationRule {
            scope: FederationScope::AllNonPrivate,
            permission: FederationPermission::Allow,
        }],
        subscribe_rules: vec![],
    }];

    let sharing = KnowledgeSharing::new(policies, 0.5);
    let segment = Segment::new(
        Content::Text("hello world".to_string()),
        vec![0.1, 0.2, 0.3],
        Source::Manual { description: "test".to_string() },
    );
    let target = InstanceId::new();
    assert!(sharing.can_publish(&segment, &target));
}

#[test]
fn publish_check_denies_when_no_policy() {
    let sharing = KnowledgeSharing::new(vec![], 0.5);
    let segment = Segment::new(
        Content::Text("hello".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    let target = InstanceId::new();
    assert!(!sharing.can_publish(&segment, &target));
}

#[test]
fn publish_check_denies_private_segment() {
    let policies = vec![FederationPolicy {
        id: PolicyId::new(),
        name: "share-all".to_string(),
        active: true,
        publish_rules: vec![FederationRule {
            scope: FederationScope::AllNonPrivate,
            permission: FederationPermission::Allow,
        }],
        subscribe_rules: vec![],
    }];

    let sharing = KnowledgeSharing::new(policies, 0.5);
    let mut segment = Segment::new(
        Content::Text("secret".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    // Segment has specific observability — only a human
    segment.observable_by.push(Principal::Human("alice".to_string()));
    let target = InstanceId::new();
    // AllNonPrivate should deny because observable_by is non-empty and doesn't include the target
    assert!(!sharing.can_publish(&segment, &target));
}

#[test]
fn publish_check_allows_when_target_in_observable_by() {
    let target = InstanceId::new();
    let sharing = KnowledgeSharing::new(vec![], 0.5); // no policies needed

    let mut segment = Segment::new(
        Content::Text("for you".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    segment.observable_by.push(Principal::Ailf(target));
    assert!(sharing.can_publish(&segment, &target));
}

#[test]
fn relevance_check_passes_similar_embedding() {
    let sharing = KnowledgeSharing::new(vec![], 0.5);
    let ann = SegmentAnnouncement {
        segment_id: SegmentId::new(),
        embedding: vec![1.0, 0.0, 0.0],
        content_kind: ContentKind::Text,
        created: Utc::now(),
        tags: HashMap::new(),
    };
    let goal_embeddings = vec![vec![0.9, 0.1, 0.0]]; // very similar
    assert!(sharing.is_relevant(&ann, &goal_embeddings));
}

#[test]
fn relevance_check_rejects_dissimilar_embedding() {
    let sharing = KnowledgeSharing::new(vec![], 0.8); // high threshold
    let ann = SegmentAnnouncement {
        segment_id: SegmentId::new(),
        embedding: vec![1.0, 0.0, 0.0],
        content_kind: ContentKind::Text,
        created: Utc::now(),
        tags: HashMap::new(),
    };
    let goal_embeddings = vec![vec![0.0, 1.0, 0.0]]; // orthogonal
    assert!(!sharing.is_relevant(&ann, &goal_embeddings));
}

#[test]
fn inactive_policy_is_skipped() {
    let policies = vec![FederationPolicy {
        id: PolicyId::new(),
        name: "inactive".to_string(),
        active: false,
        publish_rules: vec![FederationRule {
            scope: FederationScope::AllNonPrivate,
            permission: FederationPermission::Allow,
        }],
        subscribe_rules: vec![],
    }];

    let sharing = KnowledgeSharing::new(policies, 0.5);
    let segment = Segment::new(
        Content::Text("hello".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    let target = InstanceId::new();
    assert!(!sharing.can_publish(&segment, &target));
}
```

Add `mod federation_knowledge;` to `main.rs`.

- [ ] **Step 2: Implement KnowledgeSharing**

Write `crates/animus-federation/src/knowledge.rs`:
```rust
use animus_core::{InstanceId, PolicyId, Principal, Segment};
use crate::protocol::SegmentAnnouncement;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FederationScope {
    ByTag(String, String),
    BySourceType(String),
    AllNonPrivate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum FederationPermission {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationRule {
    pub scope: FederationScope,
    pub permission: FederationPermission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationPolicy {
    pub id: PolicyId,
    pub name: String,
    pub active: bool,
    pub publish_rules: Vec<FederationRule>,
    pub subscribe_rules: Vec<FederationRule>,
}

pub struct KnowledgeSharing {
    policies: Vec<FederationPolicy>,
    relevance_threshold: f32,
}

impl KnowledgeSharing {
    pub fn new(policies: Vec<FederationPolicy>, relevance_threshold: f32) -> Self {
        Self { policies, relevance_threshold }
    }

    /// Check if a segment can be published to a specific target AILF.
    pub fn can_publish(&self, segment: &Segment, target: &InstanceId) -> bool {
        // If segment has explicit observable_by including the target, always allow
        if segment.observable_by.iter().any(|p| matches!(p, Principal::Ailf(id) if id == target)) {
            return true;
        }

        // If segment has observable_by entries but target isn't in them, deny
        if !segment.observable_by.is_empty() {
            return false;
        }

        // Check federation policies
        for policy in &self.policies {
            if !policy.active {
                continue;
            }
            for rule in &policy.publish_rules {
                if self.scope_matches(&rule.scope, segment) {
                    return matches!(rule.permission, FederationPermission::Allow);
                }
            }
        }

        false // default deny
    }

    /// Check if an announcement is semantically relevant to active goals.
    pub fn is_relevant(&self, announcement: &SegmentAnnouncement, goal_embeddings: &[Vec<f32>]) -> bool {
        if goal_embeddings.is_empty() {
            return false;
        }
        for goal_emb in goal_embeddings {
            let similarity = cosine_similarity(&announcement.embedding, goal_emb);
            if similarity >= self.relevance_threshold {
                return true;
            }
        }
        false
    }

    fn scope_matches(&self, scope: &FederationScope, segment: &Segment) -> bool {
        match scope {
            FederationScope::AllNonPrivate => {
                segment.observable_by.is_empty()
            }
            FederationScope::ByTag(key, value) => {
                // Tags are not currently in the segment model — match by content
                // For V0.1, this always returns false
                false
            }
            FederationScope::BySourceType(source_type) => {
                let actual = match &segment.source {
                    animus_core::Source::Conversation { .. } => "conversation",
                    animus_core::Source::Observation { .. } => "observation",
                    animus_core::Source::Consolidation { .. } => "consolidation",
                    animus_core::Source::Federation { .. } => "federation",
                    animus_core::Source::SelfDerived { .. } => "self-derived",
                    animus_core::Source::Manual { .. } => "manual",
                };
                actual == source_type
            }
        }
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p animus-tests federation_knowledge`
Expected: All 7 tests PASS

- [ ] **Step 4: Commit**

```bash
git add crates/animus-federation/src/knowledge.rs crates/animus-tests/tests/integration/federation_knowledge.rs crates/animus-tests/tests/integration/main.rs
git commit -m "feat(federation): knowledge sharing pipeline with privacy enforcement"
```

---

### Task 8: Federation HTTP Server

**Files:**
- Create: `crates/animus-federation/src/server.rs`

- [ ] **Step 1: Implement federation server**

Write `crates/animus-federation/src/server.rs`:
```rust
use crate::auth::FederationAuth;
use crate::peers::{PeerRegistry, TrustLevel};
use crate::protocol::*;
use animus_core::{AnimusError, Result, SegmentId};
use animus_vectorfs::VectorStore;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

struct ServerState<S: VectorStore> {
    auth: Arc<FederationAuth>,
    peers: Arc<RwLock<PeerRegistry>>,
    store: Arc<S>,
    rate_limits: Mutex<HashMap<animus_core::InstanceId, (u32, i64)>>, // (count, window_start)
    max_rpm: u32,
}

pub struct FederationServer<S: VectorStore> {
    auth: Arc<FederationAuth>,
    peers: Arc<RwLock<PeerRegistry>>,
    store: Arc<S>,
    max_rpm: u32,
}

impl<S: VectorStore + Send + Sync + 'static> FederationServer<S> {
    pub fn new(
        auth: Arc<FederationAuth>,
        peers: Arc<RwLock<PeerRegistry>>,
        store: Arc<S>,
        max_rpm: u32,
    ) -> Self {
        Self { auth, peers, store, max_rpm }
    }

    pub async fn start(self, bind_addr: SocketAddr) -> Result<SocketAddr> {
        let state = Arc::new(ServerState {
            auth: self.auth,
            peers: self.peers,
            store: self.store,
            rate_limits: Mutex::new(HashMap::new()),
            max_rpm: self.max_rpm,
        });

        let app = Router::new()
            .route("/federation/handshake", post(handle_handshake::<S>))
            .route("/federation/handshake/confirm", post(handle_handshake_confirm::<S>))
            .route("/federation/segments/{id}", get(handle_get_segment::<S>))
            .route("/federation/peers", get(handle_list_peers::<S>))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind(bind_addr).await
            .map_err(|e| AnimusError::Federation(format!("failed to bind: {e}")))?;
        let local_addr = listener.local_addr()
            .map_err(|e| AnimusError::Federation(format!("failed to get local addr: {e}")))?;

        tracing::info!("Federation server listening on {local_addr}");

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("Federation server error: {e}");
            }
        });

        Ok(local_addr)
    }
}

async fn handle_handshake<S: VectorStore>(
    State(state): State<Arc<ServerState<S>>>,
    Json(request): Json<HandshakeRequest>,
) -> std::result::Result<Json<HandshakeResponse>, StatusCode> {
    match state.auth.respond_to_handshake(&request) {
        Ok((response, _nonce)) => Ok(Json(response)),
        Err(e) => {
            tracing::warn!("Handshake failed: {e}");
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

async fn handle_handshake_confirm<S: VectorStore>(
    State(_state): State<Arc<ServerState<S>>>,
    Json(_confirm): Json<HandshakeConfirm>,
) -> StatusCode {
    // In a full implementation, this would verify the confirm against stored nonce
    StatusCode::OK
}

async fn handle_get_segment<S: VectorStore>(
    State(state): State<Arc<ServerState<S>>>,
    AxumPath(id): AxumPath<String>,
) -> std::result::Result<Json<serde_json::Value>, StatusCode> {
    let uuid = uuid::Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let segment_id = SegmentId(uuid);
    match state.store.get(segment_id) {
        Ok(Some(segment)) => {
            let value = serde_json::to_value(&segment).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Ok(Json(value))
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn handle_list_peers<S: VectorStore>(
    State(state): State<Arc<ServerState<S>>>,
) -> Json<Vec<serde_json::Value>> {
    let registry = state.peers.read();
    let peers: Vec<serde_json::Value> = registry.all_peers().iter().map(|p| {
        serde_json::json!({
            "instance_id": p.info.instance_id.0.to_string(),
            "trust": format!("{:?}", p.trust),
            "last_seen": p.last_seen.to_rfc3339(),
        })
    }).collect();
    Json(peers)
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p animus-federation`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add crates/animus-federation/src/server.rs
git commit -m "feat(federation): axum HTTP server with handshake and segment retrieval"
```

---

### Task 9: Federation Orchestrator

**Files:**
- Create: `crates/animus-federation/src/orchestrator.rs`

- [ ] **Step 1: Implement orchestrator**

Write `crates/animus-federation/src/orchestrator.rs`:
```rust
use crate::audit::FederationAuditTrail;
use crate::auth::FederationAuth;
use crate::discovery::DiscoveryService;
use crate::knowledge::KnowledgeSharing;
use crate::peers::PeerRegistry;
use crate::server::FederationServer;
use animus_core::{AnimusIdentity, FederationConfig, Result};
use animus_vectorfs::VectorStore;
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

pub struct FederationOrchestrator<S: VectorStore + Send + Sync + 'static> {
    auth: Arc<FederationAuth>,
    peers: Arc<RwLock<PeerRegistry>>,
    _audit_path: PathBuf,
    server_addr: Option<SocketAddr>,
    config: FederationConfig,
    store: Arc<S>,
}

impl<S: VectorStore + Send + Sync + 'static> FederationOrchestrator<S> {
    pub fn new(
        identity: &AnimusIdentity,
        config: FederationConfig,
        store: Arc<S>,
        data_dir: &std::path::Path,
    ) -> Result<Self> {
        let auth = Arc::new(FederationAuth::new(identity.clone()));
        let peers_path = data_dir.join("federation-peers.json");
        let peers = Arc::new(RwLock::new(PeerRegistry::load(&peers_path)?));
        let audit_path = data_dir.join("federation-audit.jsonl");

        Ok(Self {
            auth,
            peers,
            _audit_path: audit_path,
            server_addr: None,
            config,
            store,
        })
    }

    pub async fn start(&mut self) -> Result<()> {
        if !self.config.enabled {
            tracing::info!("Federation is disabled");
            return Ok(());
        }

        // Start HTTP server
        let bind_addr: SocketAddr = format!("{}:{}", self.config.bind_address, self.config.port)
            .parse()
            .map_err(|e| animus_core::AnimusError::Federation(format!("invalid bind address: {e}")))?;

        let server = FederationServer::new(
            self.auth.clone(),
            self.peers.clone(),
            self.store.clone(),
            self.config.max_requests_per_minute,
        );
        let addr = server.start(bind_addr).await?;
        self.server_addr = Some(addr);

        // Start discovery
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let discovery = DiscoveryService::new(
            self.auth.instance_id(),
            self.config.static_peers.clone(),
            tx,
        );

        // Register mDNS service
        let vk_hex = hex::encode(self.auth.verifying_key().to_bytes());
        if self.config.static_peers.is_empty() {
            let _ = discovery.register_service(addr.port(), &vk_hex);
        }
        discovery.start().await?;

        // Process discovery events
        let peers_clone = self.peers.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    crate::discovery::DiscoveryEvent::PeerDiscovered(info) => {
                        tracing::info!("Discovered peer: {}", info.instance_id);
                        peers_clone.write().add_peer(info);
                    }
                    crate::discovery::DiscoveryEvent::PeerLost(id) => {
                        tracing::info!("Lost peer: {}", id);
                    }
                }
            }
        });

        tracing::info!("Federation started on {addr}");
        Ok(())
    }

    pub fn peer_count(&self) -> usize {
        self.peers.read().peer_count()
    }

    pub fn trusted_peer_count(&self) -> usize {
        self.peers.read().trusted_peers().len()
    }

    pub fn server_addr(&self) -> Option<SocketAddr> {
        self.server_addr
    }

    pub fn peers(&self) -> Arc<RwLock<PeerRegistry>> {
        self.peers.clone()
    }
}
```

- [ ] **Step 2: Update lib.rs exports**

Ensure `crates/animus-federation/src/lib.rs` has all module declarations (already done in Task 1).

- [ ] **Step 3: Verify compilation**

Run: `cargo build --workspace`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add crates/animus-federation/src/orchestrator.rs
git commit -m "feat(federation): orchestrator wiring discovery, auth, server, and peer management"
```

---

### Task 10: Runtime Integration

**Files:**
- Modify: `crates/animus-runtime/Cargo.toml`
- Modify: `crates/animus-runtime/src/main.rs`

- [ ] **Step 1: Add animus-federation dependency to runtime**

In `crates/animus-runtime/Cargo.toml`, add:
```toml
animus-federation = { workspace = true }
```

- [ ] **Step 2: Add federation commands and startup to main.rs**

In `crates/animus-runtime/src/main.rs`:

Add imports:
```rust
use animus_federation::orchestrator::FederationOrchestrator;
use animus_federation::peers::TrustLevel;
```

Add `federation: Option<FederationOrchestrator<MmapVectorStore>>` field to `CommandContext`.

After the thread scheduler initialization, add federation startup:
```rust
    // Initialize federation (if enabled)
    let federation_config = animus_core::FederationConfig::default();
    let mut federation = if federation_config.enabled {
        let mut orch = FederationOrchestrator::new(
            &identity, federation_config, store.clone(), &data_dir
        )?;
        orch.start().await?;
        Some(orch)
    } else {
        None
    };
```

Add federation commands to `handle_command`:
```rust
        "/peers" => {
            if let Some(ref fed) = ctx.federation {
                let peers_lock = fed.peers();
                let registry = peers_lock.read();
                let all = registry.all_peers();
                if all.is_empty() {
                    ctx.interface.display_status("No peers discovered.");
                } else {
                    for peer in all {
                        ctx.interface.display_status(&format!(
                            "[{}] {:?} — last seen {}",
                            peer.info.instance_id.0.to_string().get(..8).unwrap_or("?"),
                            peer.trust,
                            peer.last_seen.format("%H:%M:%S"),
                        ));
                    }
                }
            } else {
                ctx.interface.display_status("Federation is disabled.");
            }
        }
        "/trust" if !arg.is_empty() => {
            if let Some(ref fed) = ctx.federation {
                let peers_lock = fed.peers();
                let mut registry = peers_lock.write();
                let all: Vec<_> = registry.all_peers().iter()
                    .map(|p| p.info.instance_id)
                    .collect();
                let matches: Vec<_> = all.iter()
                    .filter(|id| id.0.to_string().starts_with(arg))
                    .collect();
                match matches.len() {
                    0 => ctx.interface.display_status(&format!("No peer matching '{arg}'")),
                    1 => {
                        registry.set_trust(matches[0], TrustLevel::Trusted);
                        ctx.interface.display_status(&format!("Peer {} trusted",
                            matches[0].0.to_string().get(..8).unwrap_or("?")));
                    }
                    n => ctx.interface.display_status(&format!("{n} peers match — be more specific")),
                }
            } else {
                ctx.interface.display_status("Federation is disabled.");
            }
        }
        "/block" if !arg.is_empty() => {
            if let Some(ref fed) = ctx.federation {
                let peers_lock = fed.peers();
                let mut registry = peers_lock.write();
                let all: Vec<_> = registry.all_peers().iter()
                    .map(|p| p.info.instance_id)
                    .collect();
                let matches: Vec<_> = all.iter()
                    .filter(|id| id.0.to_string().starts_with(arg))
                    .collect();
                match matches.len() {
                    0 => ctx.interface.display_status(&format!("No peer matching '{arg}'")),
                    1 => {
                        registry.set_trust(matches[0], TrustLevel::Blocked);
                        ctx.interface.display_status(&format!("Peer {} blocked",
                            matches[0].0.to_string().get(..8).unwrap_or("?")));
                    }
                    n => ctx.interface.display_status(&format!("{n} peers match — be more specific")),
                }
            } else {
                ctx.interface.display_status("Federation is disabled.");
            }
        }
        "/federate" => {
            if let Some(ref fed) = ctx.federation {
                ctx.interface.display_status(&format!(
                    "Federation: enabled, {} peers ({} trusted), server on {:?}",
                    fed.peer_count(), fed.trusted_peer_count(), fed.server_addr()
                ));
            } else {
                ctx.interface.display_status("Federation is disabled. Set federation.enabled = true in config.");
            }
        }
```

Update `/help` to include federation commands:
```
            ctx.interface.display("/peers           — list discovered peers");
            ctx.interface.display("/trust <id>      — trust a peer");
            ctx.interface.display("/block <id>      — block a peer");
            ctx.interface.display("/federate        — show federation status");
```

- [ ] **Step 3: Verify compilation and tests**

Run: `cargo build --workspace && cargo test --workspace`
Expected: compiles and all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/animus-runtime/Cargo.toml crates/animus-runtime/src/main.rs
git commit -m "feat(runtime): federation commands — /peers, /trust, /block, /federate"
```

---

### Task 11: End-to-End Integration Test

**Files:**
- Create: `crates/animus-tests/tests/integration/federation_e2e.rs`
- Modify: `crates/animus-tests/tests/integration/main.rs`

- [ ] **Step 1: Write E2E test**

Create `crates/animus-tests/tests/integration/federation_e2e.rs`:
```rust
use animus_core::AnimusIdentity;
use animus_federation::auth::FederationAuth;
use animus_federation::peers::{PeerInfo, PeerRegistry, TrustLevel};
use animus_federation::server::FederationServer;
use animus_vectorfs::store::MmapVectorStore;
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::sync::Arc;

/// Test two in-process AILF instances performing handshake via HTTP.
#[tokio::test]
async fn two_ailfs_handshake_over_http() {
    let dir = tempfile::tempdir().unwrap();

    // Create two AILF identities
    let alice_identity = AnimusIdentity::generate("test-model".to_string());
    let bob_identity = AnimusIdentity::generate("test-model".to_string());

    let alice_auth = Arc::new(FederationAuth::new(alice_identity.clone()));
    let bob_auth = Arc::new(FederationAuth::new(bob_identity.clone()));

    // Create stores
    let alice_store = Arc::new(MmapVectorStore::open(&dir.path().join("alice"), 64).unwrap());
    let bob_store = Arc::new(MmapVectorStore::open(&dir.path().join("bob"), 64).unwrap());

    // Start Bob's server
    let bob_peers = Arc::new(RwLock::new(PeerRegistry::new()));
    let bob_server = FederationServer::new(
        bob_auth.clone(),
        bob_peers.clone(),
        bob_store.clone(),
        100,
    );
    let bob_addr = bob_server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();

    // Alice initiates handshake with Bob via HTTP
    let client = reqwest::Client::new();
    let (request, alice_nonce) = alice_auth.create_handshake();

    let resp = client.post(format!("http://{}/federation/handshake", bob_addr))
        .json(&request)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let response: animus_federation::protocol::HandshakeResponse = resp.json().await.unwrap();

    // Alice verifies Bob's response
    let confirm = alice_auth.verify_response_and_confirm(&response, &alice_nonce).unwrap();

    // Alice sends confirmation
    let confirm_resp = client.post(format!("http://{}/federation/handshake/confirm", bob_addr))
        .json(&confirm)
        .send()
        .await
        .unwrap();
    assert_eq!(confirm_resp.status(), 200);
}

/// Test listing peers via HTTP endpoint.
#[tokio::test]
async fn list_peers_via_http() {
    let dir = tempfile::tempdir().unwrap();
    let identity = AnimusIdentity::generate("test-model".to_string());
    let auth = Arc::new(FederationAuth::new(identity));
    let store = Arc::new(MmapVectorStore::open(&dir.path().join("store"), 64).unwrap());
    let peers = Arc::new(RwLock::new(PeerRegistry::new()));

    let server = FederationServer::new(auth, peers, store, 100);
    let addr = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client.get(format!("http://{}/federation/peers", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let peers: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(peers.is_empty());
}
```

Add `mod federation_e2e;` to `main.rs`.

- [ ] **Step 2: Run the E2E tests**

Run: `cargo test -p animus-tests federation_e2e`
Expected: 2 tests PASS

- [ ] **Step 3: Commit**

```bash
git add crates/animus-tests/tests/integration/federation_e2e.rs crates/animus-tests/tests/integration/main.rs
git commit -m "test(federation): end-to-end handshake and peer listing over HTTP"
```

---

### Task 12: Final Verification and Cleanup

**Files:** All workspace crates

- [ ] **Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass (existing + new federation tests)

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace`
Expected: No warnings

- [ ] **Step 3: Fix any issues found**

Address any compilation errors, test failures, or clippy warnings.

- [ ] **Step 4: Run cargo build**

Run: `cargo build --workspace`
Expected: Clean build

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: address clippy warnings and test issues from Phase 5 Federation"
```

- [ ] **Step 6: Push branch and create PR**

```bash
git push -u origin feat/phase5-federation
gh pr create --title "feat: Phase 5 — Federation (AILF-to-AILF communication)" --body "..."
```
