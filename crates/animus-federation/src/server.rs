use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use animus_core::{AnimusError, InstanceId, Result, SegmentId};
use animus_vectorfs::VectorStore;
use axum::extract::{ConnectInfo, Extension, Path as AxumPath, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use ed25519_dalek::VerifyingKey;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use animus_cortex::telos::{GoalManager, GoalSource};
use crate::auth::FederationAuth;
use crate::knowledge::KnowledgeSharing;
use crate::peers::{Peer, PeerInfo, PeerRegistry, TrustLevel};
use crate::protocol::{GoalAnnouncement, HandshakeConfirm, HandshakeRequest, SegmentAnnouncement};

/// Verified peer identity, inserted into request extensions by auth middleware.
#[derive(Clone)]
struct AuthenticatedPeer {
    instance_id: InstanceId,
}

/// Data stored for a pending handshake (between handshake and confirm steps).
struct PendingHandshake {
    counter_nonce: [u8; 32],
    initiator_vk_hex: String,
}

/// Shared state accessible by all axum handlers.
struct ServerState<S: VectorStore> {
    auth: FederationAuth,
    peers: Arc<RwLock<PeerRegistry>>,
    store: Arc<S>,
    knowledge: KnowledgeSharing,
    pending_handshakes: Mutex<HashMap<InstanceId, PendingHandshake>>,
    goals: Option<Arc<parking_lot::Mutex<GoalManager>>>,
    goals_path: Option<std::path::PathBuf>,
    max_rpm: u32,
    /// Per-peer request timestamps for rate limiting (sliding window).
    rate_limits: Mutex<HashMap<InstanceId, Vec<i64>>>,
}

/// Lightweight axum HTTP server for inbound federation requests.
pub struct FederationServer<S: VectorStore> {
    state: Arc<ServerState<S>>,
}

impl<S: VectorStore + 'static> FederationServer<S> {
    /// Create a new federation server.
    ///
    /// - `auth`: the federation auth identity for this instance
    /// - `peers`: shared peer registry (same Arc used by orchestrator — H4 fix)
    /// - `store`: the vector store for segment retrieval
    /// - `knowledge`: knowledge sharing policies for privacy enforcement
    /// - `max_rpm`: maximum requests per minute (stored for future rate limiting)
    pub fn new(
        auth: FederationAuth,
        peers: Arc<RwLock<PeerRegistry>>,
        store: Arc<S>,
        knowledge: KnowledgeSharing,
        max_rpm: u32,
    ) -> Self {
        let state = Arc::new(ServerState {
            auth,
            peers,
            store,
            knowledge,
            pending_handshakes: Mutex::new(HashMap::new()),
            goals: None,
            goals_path: None,
            max_rpm,
            rate_limits: Mutex::new(HashMap::new()),
        });
        Self { state }
    }

    /// Set the goal manager for handling inbound goal announcements.
    pub fn set_goals(
        &mut self,
        goals: Arc<parking_lot::Mutex<GoalManager>>,
        goals_path: std::path::PathBuf,
    ) {
        let state = Arc::get_mut(&mut self.state)
            .expect("set_goals must be called before start()");
        state.goals = Some(goals);
        state.goals_path = Some(goals_path);
    }

    /// Start the HTTP server, binding to the given address.
    ///
    /// Spawns the server as a background tokio task and returns the actual
    /// bound address (useful when binding to port 0 for tests).
    pub async fn start(&self, bind_addr: SocketAddr) -> Result<SocketAddr> {
        // Unauthenticated routes — handshake IS the authentication ceremony
        let public_routes = Router::new()
            .route("/federation/handshake", post(handle_handshake::<S>))
            .route(
                "/federation/handshake/confirm",
                post(handle_handshake_confirm::<S>),
            );

        // Authenticated routes — require completed handshake + signed request (H1)
        let protected_routes = Router::new()
            .route("/federation/segments/{id}", get(handle_get_segment::<S>))
            .route("/federation/peers", get(handle_list_peers::<S>))
            .route("/federation/publish", post(handle_publish::<S>))
            .route("/federation/goals", post(handle_goals::<S>))
            .layer(middleware::from_fn_with_state(
                self.state.clone(),
                auth_middleware::<S>,
            ));

        let app = public_routes
            .merge(protected_routes)
            .with_state(self.state.clone());

        let listener = tokio::net::TcpListener::bind(bind_addr).await.map_err(|e| {
            AnimusError::Federation(format!("failed to bind to {bind_addr}: {e}"))
        })?;

        let actual_addr = listener.local_addr().map_err(|e| {
            AnimusError::Federation(format!("failed to get local address: {e}"))
        })?;

        tracing::info!("Federation HTTP server listening on {actual_addr}");

        tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                tracing::error!("Federation HTTP server error: {e}");
            }
        });

        Ok(actual_addr)
    }

    /// Get the shared peer registry.
    pub fn peers(&self) -> Arc<RwLock<PeerRegistry>> {
        self.state.peers.clone()
    }
}

// ---------------------------------------------------------------------------
// Auth middleware (H1 + H2)
// ---------------------------------------------------------------------------

/// Middleware that verifies signed requests on protected endpoints.
///
/// Expects these headers:
/// - `X-Animus-Instance-Id`: the requesting peer's InstanceId (UUID)
/// - `X-Animus-Timestamp`: Unix timestamp (seconds)
/// - `X-Animus-Signature`: hex-encoded Ed25519 signature
///
/// The signature is verified against the peer's registered verifying key.
/// Blocked peers are rejected (H2). Unknown/unverified peers are rejected.
async fn auth_middleware<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    mut request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let headers = request.headers();

    let instance_id_str = match headers
        .get("x-animus-instance-id")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_string(),
        None => return error_response(StatusCode::UNAUTHORIZED, "missing X-Animus-Instance-Id header"),
    };

    let timestamp_str = match headers
        .get("x-animus-timestamp")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_string(),
        None => return error_response(StatusCode::UNAUTHORIZED, "missing X-Animus-Timestamp header"),
    };

    let signature_hex = match headers
        .get("x-animus-signature")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_string(),
        None => return error_response(StatusCode::UNAUTHORIZED, "missing X-Animus-Signature header"),
    };

    // Parse instance ID
    let uuid = match Uuid::parse_str(&instance_id_str) {
        Ok(u) => u,
        Err(_) => return error_response(StatusCode::UNAUTHORIZED, "invalid X-Animus-Instance-Id"),
    };
    let instance_id = InstanceId(uuid);

    // Parse timestamp
    let timestamp: i64 = match timestamp_str.parse() {
        Ok(t) => t,
        Err(_) => return error_response(StatusCode::UNAUTHORIZED, "invalid X-Animus-Timestamp"),
    };

    // Look up peer, check blocked status (H2), get verifying key.
    // The RwLock guard is scoped so it drops before any .await.
    let peer_vk = {
        let peers = state.peers.read();
        match peers.get_peer(&instance_id) {
            Some(peer) => {
                if peer.trust == TrustLevel::Blocked {
                    return error_response(StatusCode::FORBIDDEN, "peer is blocked");
                }
                if peer.trust == TrustLevel::Unknown {
                    return error_response(
                        StatusCode::UNAUTHORIZED,
                        "peer has not completed handshake",
                    );
                }
                peer.info.to_peer_info().map(|info| info.verifying_key)
            }
            None => None,
        }
    };

    let peer_vk = match peer_vk {
        Some(vk) => vk,
        None => return error_response(StatusCode::UNAUTHORIZED, "unknown or invalid peer"),
    };

    // Extract body bytes for signature verification.
    // For GET/DELETE the body is empty; for POST/PUT we read the full body.
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    let body_bytes = if method == axum::http::Method::GET || method == axum::http::Method::DELETE {
        Vec::new()
    } else {
        let (parts, body) = request.into_parts();
        let bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
            Ok(b) => b,
            Err(e) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &format!("failed to read request body: {e}"),
                );
            }
        };
        let body_vec = bytes.to_vec();
        // Reconstruct request with the body so downstream handlers can read it
        request = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
        body_vec
    };

    // Verify request signature (H1).
    if let Err(e) =
        FederationAuth::verify_signed_request(timestamp, &path, &body_bytes, &signature_hex, &peer_vk)
    {
        return error_response(
            StatusCode::UNAUTHORIZED,
            &format!("signature verification failed: {e}"),
        );
    }

    // Rate limiting: enforce max_rpm per peer
    if state.max_rpm > 0 {
        let now = chrono::Utc::now().timestamp();
        let window_start = now - 60;
        let mut rate_map = state.rate_limits.lock().await;
        // Prune peers with no recent activity to bound HashMap growth.
        if rate_map.len() > 1000 {
            rate_map.retain(|_, v| !v.is_empty());
        }
        let timestamps = rate_map.entry(instance_id).or_default();
        // Remove entries older than 1 minute
        timestamps.retain(|&t| t > window_start);
        if timestamps.len() >= state.max_rpm as usize {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate limit exceeded",
            );
        }
        timestamps.push(now);
    }

    // Inject authenticated peer identity for downstream handlers
    request.extensions_mut().insert(AuthenticatedPeer { instance_id });

    next.run(request).await
}

// ---------------------------------------------------------------------------
// Axum handlers
// ---------------------------------------------------------------------------

/// Error response body returned as JSON.
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Build a JSON error response.
fn error_response(status: StatusCode, msg: &str) -> Response {
    (status, Json(ErrorResponse { error: msg.to_string() })).into_response()
}

/// Build a JSON success response, falling back to 500 if serialization fails.
fn json_response<T: Serialize>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_value(body) {
        Ok(json) => (status, Json(json)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("response serialization failed: {e}"),
        ),
    }
}

/// POST /federation/handshake
///
/// Receives a handshake request from an initiating peer, signs the challenge
/// nonce, and returns a response with a counter-nonce challenge.
/// Rejects handshakes from blocked peers (H2).
async fn handle_handshake<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    Json(request): Json<HandshakeRequest>,
) -> Response {
    tracing::info!(
        peer = %request.instance_id,
        "Received federation handshake request"
    );

    // H2: reject handshakes from blocked peers
    {
        let peers = state.peers.read();
        if let Some(peer) = peers.get_peer(&request.instance_id) {
            if peer.trust == TrustLevel::Blocked {
                tracing::warn!(peer = %request.instance_id, "Rejecting handshake from blocked peer");
                return error_response(StatusCode::FORBIDDEN, "peer is blocked");
            }
        }
    }

    match state.auth.respond_to_handshake(&request) {
        Ok((response, counter_nonce)) => {
            let pending = PendingHandshake {
                counter_nonce,
                initiator_vk_hex: request.verifying_key_hex.clone(),
            };
            state
                .pending_handshakes
                .lock()
                .await
                .insert(request.instance_id, pending);

            json_response(StatusCode::OK, &response)
        }
        Err(e) => {
            tracing::warn!(peer = %request.instance_id, error = %e, "Handshake failed");
            error_response(StatusCode::BAD_REQUEST, &e.to_string())
        }
    }
}

/// Request body for the handshake confirm endpoint.
#[derive(Deserialize)]
struct HandshakeConfirmRequest {
    instance_id: InstanceId,
    #[serde(flatten)]
    confirm: HandshakeConfirm,
}

/// POST /federation/handshake/confirm
///
/// Receives the initiator's counter-nonce signature, verifying mutual authentication.
/// On success, registers or upgrades the peer to Verified trust level (M6).
async fn handle_handshake_confirm<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<HandshakeConfirmRequest>,
) -> Response {
    tracing::info!(
        peer = %request.instance_id,
        "Received federation handshake confirm"
    );

    let pending = state
        .pending_handshakes
        .lock()
        .await
        .remove(&request.instance_id);

    let pending = match pending {
        Some(p) => p,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "no pending handshake for this instance",
            );
        }
    };

    match state.auth.verify_confirm(
        &request.confirm,
        &pending.counter_nonce,
        &pending.initiator_vk_hex,
    ) {
        Ok(()) => {
            tracing::info!(peer = %request.instance_id, "Handshake confirmed successfully");

            // M6: Register peer as Verified after successful mutual authentication
            let vk_bytes = hex::decode(&pending.initiator_vk_hex).ok();
            {
                let mut peers = state.peers.write();
                if peers.get_peer(&request.instance_id).is_some() {
                    // Peer already known — upgrade trust and record handshake time
                    peers.set_trust(&request.instance_id, TrustLevel::Verified);
                    if let Some(peer) = peers.get_peer_mut(&request.instance_id) {
                        peer.last_handshake = Some(Utc::now());
                        if let Some(ref vk) = vk_bytes {
                            if let Ok(vk_arr) = <[u8; 32]>::try_from(vk.as_slice()) {
                                peer.info.verifying_key_bytes = vk_arr;
                            }
                        }
                    }
                } else if let Some(ref vk) = vk_bytes {
                    // New peer — register with Verified trust
                    if let Ok(vk_arr) = <[u8; 32]>::try_from(vk.as_slice()) {
                        if let Ok(vk) = VerifyingKey::from_bytes(&vk_arr) {
                            let info = PeerInfo {
                                instance_id: request.instance_id,
                                verifying_key: vk,
                                address: peer_addr,
                            };
                            peers.add_peer(info);
                            peers.set_trust(&request.instance_id, TrustLevel::Verified);
                            if let Some(peer) = peers.get_peer_mut(&request.instance_id) {
                                peer.last_handshake = Some(Utc::now());
                            }
                        }
                    }
                }
            }

            #[derive(Serialize)]
            struct ConfirmResponse {
                status: String,
            }

            json_response(StatusCode::OK, &ConfirmResponse {
                status: "confirmed".to_string(),
            })
        }
        Err(e) => {
            tracing::warn!(peer = %request.instance_id, error = %e, "Handshake confirm failed");
            error_response(StatusCode::UNAUTHORIZED, &e.to_string())
        }
    }
}

/// GET /federation/segments/{id}
///
/// Returns a segment by its UUID. Requires authentication (H1) and checks
/// privacy policies before returning data (H3).
async fn handle_get_segment<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    Extension(auth_peer): Extension<AuthenticatedPeer>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid segment ID: {e}"));
        }
    };

    let segment_id = SegmentId(uuid);

    match state.store.get(segment_id) {
        Ok(Some(segment)) => {
            // H3: Check privacy/federation policies before returning segment
            if !state.knowledge.can_publish(&segment, &auth_peer.instance_id) {
                tracing::warn!(
                    peer = %auth_peer.instance_id,
                    segment = %id,
                    "Denied segment access — privacy policy"
                );
                return error_response(
                    StatusCode::FORBIDDEN,
                    "segment not available for federation",
                );
            }

            json_response(StatusCode::OK, &segment)
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, &format!("segment {id} not found")),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("failed to retrieve segment: {e}"),
        ),
    }
}

/// A JSON-friendly peer summary for the debug endpoint.
#[derive(Serialize)]
struct PeerSummary {
    instance_id: InstanceId,
    trust: String,
    last_seen: String,
    segments_received: u64,
    segments_sent: u64,
}

impl From<&Peer> for PeerSummary {
    fn from(peer: &Peer) -> Self {
        Self {
            instance_id: peer.info.instance_id,
            trust: format!("{:?}", peer.trust),
            last_seen: peer.last_seen.to_rfc3339(),
            segments_received: peer.segments_received,
            segments_sent: peer.segments_sent,
        }
    }
}

/// GET /federation/peers
///
/// Returns a list of all known peers as JSON. Protected endpoint —
/// only authenticated (Verified+) peers can enumerate peers.
async fn handle_list_peers<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    Extension(_auth_peer): Extension<AuthenticatedPeer>,
) -> Response {
    let peers = state.peers.read();
    let summaries: Vec<PeerSummary> = peers
        .all_peers()
        .iter()
        .map(|p| PeerSummary::from(*p))
        .collect();

    json_response(StatusCode::OK, &summaries)
}

/// POST /federation/publish
///
/// Receives a segment announcement from an authenticated peer.
/// Validates the announcement and stores a reference to the remote segment
/// in the local VectorFS as a federation-sourced segment.
async fn handle_publish<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    Extension(auth_peer): Extension<AuthenticatedPeer>,
    Json(announcement): Json<SegmentAnnouncement>,
) -> Response {
    tracing::info!(
        peer = %auth_peer.instance_id,
        segment = %announcement.segment_id,
        "Received segment announcement"
    );

    // Store a federation-sourced segment with the announced embedding
    let mut segment = animus_core::Segment::new(
        animus_core::Content::Text(format!(
            "Federation segment from {} (kind: {:?})",
            auth_peer.instance_id, announcement.content_kind
        )),
        announcement.embedding,
        animus_core::Source::Federation {
            source_ailf: auth_peer.instance_id,
            original_id: announcement.segment_id,
        },
    );
    segment.infer_decay_class();

    match state.store.store(segment) {
        Ok(id) => {
            // Update peer stats
            {
                let mut peers = state.peers.write();
                if let Some(peer) = peers.get_peer_mut(&auth_peer.instance_id) {
                    peer.segments_received += 1;
                    peer.last_seen = Utc::now();
                }
            }

            #[derive(Serialize)]
            struct PublishResponse {
                status: String,
                local_segment_id: SegmentId,
            }

            json_response(StatusCode::OK, &PublishResponse {
                status: "accepted".to_string(),
                local_segment_id: id,
            })
        }
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("failed to store announcement: {e}"),
        ),
    }
}

/// POST /federation/goals
///
/// Receives a goal announcement from an authenticated peer.
/// Returns acknowledgement. Goal integration with the local Telos
/// goal manager happens at the orchestrator level.
async fn handle_goals<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    Extension(auth_peer): Extension<AuthenticatedPeer>,
    Json(announcement): Json<GoalAnnouncement>,
) -> Response {
    tracing::info!(
        peer = %auth_peer.instance_id,
        goal = %announcement.goal_id,
        priority = ?announcement.priority,
        "Received goal announcement: {}",
        announcement.description
    );

    // Create goal in local GoalManager if available
    let local_goal_id = if let Some(ref goals) = state.goals {
        let mut goals = goals.lock();
        let id = goals.create_goal(
            announcement.description.clone(),
            GoalSource::Federated {
                source_ailf: auth_peer.instance_id,
            },
            announcement.priority,
        );
        // Persist goals
        if let Some(ref path) = state.goals_path {
            if let Err(e) = goals.save(path) {
                tracing::warn!("Failed to persist federated goal: {e}");
            }
        }
        Some(id)
    } else {
        None
    };

    #[derive(Serialize)]
    struct GoalResponse {
        status: String,
        goal_id: animus_core::GoalId,
        local_goal_id: Option<animus_core::GoalId>,
    }

    json_response(StatusCode::OK, &GoalResponse {
        status: "accepted".to_string(),
        goal_id: announcement.goal_id,
        local_goal_id,
    })
}
