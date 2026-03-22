# Phase 5: Federation — AILF-to-AILF Communication

## Goal

Enable AILFs to discover each other on a local network, authenticate via Ed25519 keypairs, share knowledge as segments with trust-gated confidence, coordinate on federated goals, and enforce human-controlled privacy boundaries.

**Done when**: Two AILFs can discover each other, perform a cryptographic handshake, share validated knowledge segments, and coordinate on federated goals — all with human-controlled privacy enforcement.

## Architecture

Federation extends the existing identity, segment, and goal systems with a peer-to-peer communication layer. Each AILF announces itself on the local network via mDNS (DNS-SD), authenticates peers using Ed25519 challenge-response, and exchanges knowledge through a reference-first protocol: embeddings and metadata are broadcast, full content is requested only when semantically relevant. This mirrors the inter-thread signaling pattern from Phase 4 — reference sharing, not context sharing.

The new `animus-federation` crate encapsulates all networking, discovery, peer management, and protocol logic. It depends on `animus-core` for types and `animus-vectorfs` for segment storage.

## Tech Stack

- **Discovery**: `mdns-sd = "0.11"` for mDNS/DNS-SD service announcement and browsing. Fallback: config-based static peer list for environments where multicast is unavailable (containers, CI).
- **Transport**: HTTP/1.1 via `axum = { version = "0.8", features = ["json"] }` (server) + `reqwest` (client), JSON payloads. HTTP/2 is not required for local-network federation in V0.1.
- **Authentication**: `ed25519-dalek` (already in workspace) for challenge-response handshake
- **Serialization**: `serde` + `serde_json` (already in workspace)
- **Async**: `tokio` (already in workspace)

**New workspace dependencies to add to `Cargo.toml`**:
- `axum = { version = "0.8", features = ["json"] }`
- `mdns-sd = "0.11"`
- `tower = "0.5"` (axum peer dependency)
- `parking_lot` (already in workspace, used for `RwLock`)

## Prerequisites

Before federation's knowledge pipeline can work, the following changes to existing code are required:

1. **`GoalSource::Federated` must gain a field**: The existing `GoalSource::Federated` in `crates/animus-cortex/src/telos.rs` is a unit variant. This spec changes it to `Federated { source_ailf: InstanceId }`. All existing match arms on `GoalSource` must be updated.

2. **`Goal` needs an embedding field**: The knowledge receiving flow checks semantic relevance by comparing announcement embeddings against active goal embeddings. The current `Goal` struct has no `embedding` field. For V0.1, the approach is: embed goal descriptions on-the-fly during relevance checking using the `EmbeddingService`, caching the result on the `Goal` struct as `embedding: Option<Vec<f32>>`. This avoids requiring embeddings at goal creation time.

## Components

### 1. Discovery (`discovery.rs`)

Handles mDNS service announcement and peer browsing, with a config-based fallback for environments where multicast is unavailable.

**Service type**: `_animus._tcp.local.`

**Protocol version**: `1` (included in TXT records and handshake for forward compatibility)

**TXT records published**:
- `instance_id=<uuid>` — this AILF's instance ID
- `vk=<hex-encoded-verifying-key>` — Ed25519 public key (32 bytes, 64 hex chars)
- `port=<u16>` — HTTP port for federation API
- `proto=1` — federation protocol version

**Behavior**:
- On startup, register mDNS service with TXT records
- Browse for other `_animus._tcp.local.` services
- Emit `PeerDiscovered { instance_id, verifying_key, address, port }` events
- Emit `PeerLost { instance_id }` when service disappears
- Deduplicate by instance_id (same AILF re-announcing)
- `SocketAddr` is assembled from mDNS-resolved host address + TXT record port

**Static fallback**: If `FederationConfig::static_peers` is non-empty, skip mDNS and use the configured peer addresses directly. This enables federation in Docker containers, VMs, and CI environments where multicast UDP on port 5353 is blocked.

```rust
pub struct DiscoveryService {
    daemon: Option<ServiceDaemon>,  // None if using static peers
    own_instance_id: InstanceId,
    static_peers: Vec<SocketAddr>,
}

pub struct PeerInfo {
    pub instance_id: InstanceId,
    pub verifying_key: VerifyingKey,
    pub address: SocketAddr,
}

pub enum DiscoveryEvent {
    PeerDiscovered(PeerInfo),
    PeerLost(InstanceId),
}
```

### 2. Authentication (`auth.rs`)

Ed25519 challenge-response handshake. No PKI — trust is peer-to-peer.

**Handshake flow**:
1. Initiator sends `HandshakeRequest { instance_id, verifying_key, nonce: [u8; 32], protocol_version: u32 }`
2. Responder validates instance_id matches discovered peer, checks protocol_version compatibility, signs nonce with own key
3. Responder replies `HandshakeResponse { instance_id, verifying_key, signature, counter_nonce, protocol_version }`
4. Initiator verifies signature, signs counter_nonce
5. Initiator sends `HandshakeConfirm { signature }`
6. Both sides now have verified peer identity

**Post-handshake request signing**:
After handshake, all requests include two headers for replay protection:
- `X-Animus-Timestamp`: Unix epoch seconds as string
- `X-Animus-Signature`: hex-encoded Ed25519 signature over `"{timestamp}:{request_path}:{sha256_of_body}"`

**Replay window**: Requests with timestamps older than 30 seconds are rejected. Peers should have reasonably synchronized clocks (NTP).

**Session expiry**: Sessions are ephemeral — re-handshake required on reconnect or after 1 hour of inactivity.

```rust
pub struct FederationAuth {
    identity: AnimusIdentity,
}

impl FederationAuth {
    pub fn create_handshake(&self) -> HandshakeRequest;
    pub fn respond_to_handshake(&self, req: &HandshakeRequest) -> Result<HandshakeResponse>;
    pub fn verify_response(&self, resp: &HandshakeResponse, original_nonce: &[u8; 32]) -> Result<()>;
    pub fn sign_request(&self, timestamp: i64, path: &str, body: &[u8]) -> Signature;
    pub fn verify_request(&self, timestamp: i64, path: &str, body: &[u8], sig: &Signature, peer_vk: &VerifyingKey) -> Result<()>;
}
```

### 3. Peer Registry (`peers.rs`)

Tracks known peers, their trust level, and connection state.

```rust
pub enum TrustLevel {
    Unknown,     // just discovered, not yet handshaked
    Verified,    // handshake completed, default trust
    Trusted,     // human explicitly trusted
    Blocked,     // human explicitly blocked
}

pub struct Peer {
    pub info: PeerInfo,
    pub trust: TrustLevel,
    pub last_seen: DateTime<Utc>,
    pub last_handshake: Option<DateTime<Utc>>,
    pub segments_received: u64,
    pub segments_sent: u64,
}

pub struct PeerRegistry {
    peers: HashMap<InstanceId, Peer>,
}

impl PeerRegistry {
    pub fn add_peer(&mut self, info: PeerInfo);
    pub fn remove_peer(&mut self, id: &InstanceId);
    pub fn get_peer(&self, id: &InstanceId) -> Option<&Peer>;
    pub fn set_trust(&mut self, id: &InstanceId, trust: TrustLevel);
    pub fn trusted_peers(&self) -> Vec<&Peer>;
    pub fn save(&self, path: &Path) -> Result<()>;
    pub fn load(path: &Path) -> Result<Self>;
}
```

**Trust policies**:
- `Unknown`: No federation allowed, only discovery visible
- `Verified`: Handshake done; receive metadata broadcasts, don't auto-accept segments
- `Trusted`: Full federation — accept segments at confidence 0.3, share back
- `Blocked`: Reject all requests, suppress from peer list

### 4. Protocol Messages (`protocol.rs`)

REST-like API over HTTP with JSON bodies. All request bodies are signed.

**Endpoints** (served by the local AILF's federation server):

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/federation/handshake` | Initiate/respond to handshake |
| POST | `/federation/handshake/confirm` | Complete handshake |
| POST | `/federation/publish` | Broadcast segment metadata |
| GET | `/federation/segments/{id}` | Request full segment content |
| POST | `/federation/goals` | Share a federated goal |
| POST | `/federation/goals/{id}/status` | Update goal completion status |
| GET | `/federation/peers` | List known peers (debug) |

**Message types**:

```rust
/// Content kind — mirrors Content enum variants for type-safe announcements
pub enum ContentKind {
    Text,
    Structured,
    Binary,
    Reference,
}

/// Broadcast: "I have this knowledge, here's the embedding + metadata"
pub struct SegmentAnnouncement {
    pub segment_id: SegmentId,
    pub embedding: Vec<f32>,
    pub content_kind: ContentKind,
    pub created: DateTime<Utc>,
    pub tags: HashMap<String, String>,
}

/// Response to GET /segments/{id} — full segment content
pub struct SegmentTransfer {
    pub segment: Segment,          // full segment with content + embedding
    pub source_ailf: InstanceId,
    pub signature: Vec<u8>,        // Ed25519 signature over segment bytes
}

/// Federated goal announcement
pub struct GoalAnnouncement {
    pub goal_id: GoalId,
    pub description: String,
    pub priority: Priority,
    pub source_ailf: InstanceId,
}

/// Goal status update
pub struct GoalStatusUpdate {
    pub goal_id: GoalId,
    pub completed: bool,
    pub summary: Option<String>,
}
```

### 5. Knowledge Sharing Pipeline (`knowledge.rs`)

Orchestrates the publish/subscribe flow for segment federation.

**Publishing flow** (outbound):
1. Human creates a federation consent policy (like sensorium consent)
2. Policy specifies which segments can be published: by tag, by source type, by path scope
3. When a segment matches a publish policy, broadcast `SegmentAnnouncement` to all trusted peers
4. Only embedding + metadata sent initially — content stays local until requested

**Receiving flow** (inbound):
1. Receive `SegmentAnnouncement` from trusted peer
2. Check semantic relevance: cosine similarity of announcement embedding against active goal embeddings (computed on-the-fly via `EmbeddingService` and cached on the `Goal` struct)
3. If similarity > threshold (configurable, default 0.5): request full segment via GET
4. Store received segment with `Source::Federation { source_ailf, original_id }`
5. Set initial confidence based on trust level:
   - `Trusted` peer: confidence 0.3
   - `Verified` peer: confidence 0.1
6. Log to federation audit trail

**Privacy enforcement**:
- Only segments where `observable_by` includes `Principal::Ailf(target_instance_id)` or has a matching federation consent policy can be shared
- Default: nothing is federated — human must explicitly create federation policies
- Private segments are NEVER included in announcements

```rust
pub struct KnowledgeSharing<S: VectorStore> {
    store: Arc<S>,
    consent_policies: Vec<FederationPolicy>,
    relevance_threshold: f32,
}

pub struct FederationPolicy {
    pub id: PolicyId,
    pub name: String,
    pub active: bool,
    pub publish_rules: Vec<FederationRule>,
    pub subscribe_rules: Vec<FederationRule>,
}

pub struct FederationRule {
    pub scope: FederationScope,
    pub permission: FederationPermission,
}

pub enum FederationScope {
    ByTag(String, String),         // tag key + value pattern
    BySourceType(String),          // e.g., "observation", "conversation"
    AllNonPrivate,                 // everything not explicitly private
}

pub enum FederationPermission {
    Allow,
    Deny,
}
```

### 6. Federation Server (`server.rs`)

Lightweight axum HTTP server handling inbound federation requests.

- Binds to configurable address and port (default: `0.0.0.0:0` for OS-assigned port, announced via mDNS)
- Validates signed requests using peer's verifying key from PeerRegistry
- Routes requests to appropriate handler (handshake, segment retrieval, etc.)
- Rejects requests from blocked peers
- Rate-limits requests per peer (simple counter: max 100 requests/minute per peer for V0.1)

```rust
pub struct FederationServer<S: VectorStore> {
    peer_registry: Arc<RwLock<PeerRegistry>>,
    auth: Arc<FederationAuth>,
    store: Arc<S>,
    knowledge: Arc<KnowledgeSharing<S>>,
}

impl<S: VectorStore> FederationServer<S> {
    pub async fn start(self, bind_addr: SocketAddr) -> Result<SocketAddr>;
}
```

### 7. Federation Orchestrator (`orchestrator.rs`)

Top-level coordinator that wires discovery, auth, peers, server, and knowledge sharing together.

```rust
pub struct FederationOrchestrator<S: VectorStore> {
    discovery: DiscoveryService,
    auth: FederationAuth,
    peers: Arc<RwLock<PeerRegistry>>,
    server: FederationServer<S>,
    knowledge: KnowledgeSharing<S>,
}

impl<S: VectorStore> FederationOrchestrator<S> {
    pub async fn start(&mut self) -> Result<()>;
    pub async fn stop(&mut self) -> Result<()>;
    pub fn peer_count(&self) -> usize;
    pub fn trusted_peer_count(&self) -> usize;
}
```

### 8. Federation Audit (`audit.rs`)

Federation-specific audit entries, extending the sensorium pattern but stored separately.

```rust
pub struct FederationAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub action: FederationAuditAction,
    pub peer_instance_id: InstanceId,
    pub segment_id: Option<SegmentId>,
    pub goal_id: Option<GoalId>,
}

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
```

Audit trail stored as append-only JSON lines at `{data_dir}/federation-audit.jsonl`.

## Core Types (animus-core additions)

```rust
// In animus-core/src/config.rs
pub struct FederationConfig {
    pub enabled: bool,                      // default: false
    pub bind_address: String,               // default: "0.0.0.0"
    pub port: u16,                          // default: 0 (OS-assigned; set fixed port for production)
    pub static_peers: Vec<String>,          // fallback peer addresses when mDNS unavailable
    pub relevance_threshold: f32,           // default: 0.5
    pub federated_confidence_trusted: f32,  // default: 0.3
    pub federated_confidence_verified: f32, // default: 0.1
    pub max_requests_per_minute: u32,       // default: 100
}

// In animus-core/src/error.rs — extend AnimusError
pub enum AnimusError {
    // ... existing variants ...
    Federation(String),
}

// In animus-cortex/src/telos.rs — BREAKING CHANGE: extend GoalSource
// Update existing GoalSource::Federated unit variant to:
pub enum GoalSource {
    Human,
    System,
    Federated { source_ailf: InstanceId },
}
// All match arms on GoalSource must be updated.

// In animus-cortex/src/telos.rs — add cached embedding to Goal
pub struct Goal {
    // ... existing fields ...
    pub cached_embedding: Option<Vec<f32>>,  // lazily computed for relevance matching
}
```

## Runtime Integration

The runtime (`animus-runtime/src/main.rs`) gains:

**New commands**:
- `/peers` — list discovered and known peers with trust levels
- `/trust <id-prefix>` — upgrade peer to Trusted
- `/block <id-prefix>` — block a peer
- `/federate` — show federation status (enabled, peer count, segments shared/received)

**Startup**:
- If `federation.enabled` in config, start FederationOrchestrator
- Register mDNS service (or connect to static peers)
- Start federation HTTP server
- Begin peer discovery loop

## File Structure

```
crates/animus-federation/
  Cargo.toml
  src/
    lib.rs              # public API, re-exports
    discovery.rs        # mDNS/DNS-SD service announcement and browsing + static fallback
    auth.rs             # Ed25519 challenge-response handshake + request signing
    peers.rs            # peer registry with trust levels and persistence
    protocol.rs         # message types, ContentKind enum, API endpoint definitions
    knowledge.rs        # publish/subscribe knowledge sharing pipeline
    server.rs           # axum HTTP server for inbound federation requests
    orchestrator.rs     # top-level coordinator wiring all components
    audit.rs            # federation-specific audit trail
```

## Testing Strategy

1. **Unit tests** in each module (discovery, auth, peers, protocol, knowledge)
2. **Integration tests** in `animus-tests`:
   - `federation_auth.rs` — handshake flow, signature verification, replay protection, session expiry
   - `federation_peers.rs` — peer registry CRUD, trust transitions, persistence
   - `federation_knowledge.rs` — publish/subscribe pipeline, privacy enforcement, confidence assignment
   - `federation_goals.rs` — federated goal announcement, status updates, orphaned goal handling
   - `federation_e2e.rs` — two in-process AILF instances with mock discovery (injected peers) and in-memory HTTP transport: discover, handshake, share knowledge, coordinate goals

3. **Mock components**: Mock discovery (inject peers without mDNS), mock transport (in-memory HTTP via axum's `TestClient` or direct tower service calls)

4. **Real mDNS test** (optional, `#[ignore]`): Uses actual mDNS on local network. Run manually during development — not suitable for CI.

5. **Rate limiting test**: Verify that a peer exceeding `max_requests_per_minute` gets rejected.

## Deferred to Post-V0.1

- **Collective learning algorithms** — multi-source validation boosting confidence (architecture spec Section 3.6.2 lists this under "organizational coordination"; V0.1 builds the transport/trust layer, not the learning layer)
- **Cross-AILF policy distribution** — sharing federation policies between AILFs (architecture spec mentions "shared knowledge policies"; V0.1 policies are local-only)
- **Conflict resolution** — semantic deduplication across federated segments
- **Revocation protocol** — tombstone propagation for retracted knowledge
- **E2E encryption** — beyond TLS transport encryption
- **Token bucket rate limiting** — replace simple counter
- **Wide-area federation** — beyond local network (requires relay/registry)
- **Federation consent UI** — human-friendly policy creation (currently JSON-based)

## Error Handling

All federation errors are non-fatal to the AILF. Network failures, peer unavailability, handshake failures — all logged and retried without crashing. The AILF functions fully without federation; it's an enhancement, not a dependency.

Federated goals from a peer that disconnects remain in the goal list with their last known status. The human can manually complete or remove orphaned federated goals.
