use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use animus_core::{AnimusError, InstanceId, Result, SegmentId};
use animus_vectorfs::VectorStore;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::auth::FederationAuth;
use crate::peers::{Peer, PeerRegistry};
use crate::protocol::{HandshakeConfirm, HandshakeRequest};

/// Data stored for a pending handshake (between handshake and confirm steps).
struct PendingHandshake {
    counter_nonce: [u8; 32],
    initiator_vk_hex: String,
}

/// Shared state accessible by all axum handlers.
struct ServerState<S: VectorStore> {
    auth: FederationAuth,
    peers: RwLock<PeerRegistry>,
    store: Arc<S>,
    pending_handshakes: Mutex<HashMap<InstanceId, PendingHandshake>>,
    #[allow(dead_code)]
    max_rpm: u32,
}

/// Lightweight axum HTTP server for inbound federation requests.
pub struct FederationServer<S: VectorStore> {
    state: Arc<ServerState<S>>,
}

impl<S: VectorStore + 'static> FederationServer<S> {
    /// Create a new federation server.
    ///
    /// - `auth`: the federation auth identity for this instance
    /// - `peers`: the peer registry (shared, behind RwLock)
    /// - `store`: the vector store for segment retrieval
    /// - `max_rpm`: maximum requests per minute (stored for future rate limiting)
    pub fn new(
        auth: FederationAuth,
        peers: PeerRegistry,
        store: Arc<S>,
        max_rpm: u32,
    ) -> Self {
        let state = Arc::new(ServerState {
            auth,
            peers: RwLock::new(peers),
            store,
            pending_handshakes: Mutex::new(HashMap::new()),
            max_rpm,
        });
        Self { state }
    }

    /// Start the HTTP server, binding to the given address.
    ///
    /// Spawns the server as a background tokio task and returns the actual
    /// bound address (useful when binding to port 0 for tests).
    pub async fn start(&self, bind_addr: SocketAddr) -> Result<SocketAddr> {
        let app = Router::new()
            .route("/federation/handshake", post(handle_handshake::<S>))
            .route(
                "/federation/handshake/confirm",
                post(handle_handshake_confirm::<S>),
            )
            .route("/federation/segments/{id}", get(handle_get_segment::<S>))
            .route("/federation/peers", get(handle_list_peers::<S>))
            .with_state(self.state.clone());

        let listener = tokio::net::TcpListener::bind(bind_addr).await.map_err(|e| {
            AnimusError::Federation(format!("failed to bind to {bind_addr}: {e}"))
        })?;

        let actual_addr = listener.local_addr().map_err(|e| {
            AnimusError::Federation(format!("failed to get local address: {e}"))
        })?;

        tracing::info!("Federation HTTP server listening on {actual_addr}");

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("Federation HTTP server error: {e}");
            }
        });

        Ok(actual_addr)
    }

    /// Get a reference to the peer registry (for external access).
    pub fn peers(&self) -> &RwLock<PeerRegistry> {
        &self.state.peers
    }
}

// ---------------------------------------------------------------------------
// Axum handlers
// ---------------------------------------------------------------------------

/// Error response body returned as JSON.
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// POST /federation/handshake
///
/// Receives a handshake request from an initiating peer, signs the challenge
/// nonce, and returns a response with a counter-nonce challenge.
async fn handle_handshake<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    Json(request): Json<HandshakeRequest>,
) -> impl IntoResponse {
    tracing::info!(
        peer = %request.instance_id,
        "Received federation handshake request"
    );

    match state.auth.respond_to_handshake(&request) {
        Ok((response, counter_nonce)) => {
            // Store the pending handshake so we can verify the confirm later
            let pending = PendingHandshake {
                counter_nonce,
                initiator_vk_hex: request.verifying_key_hex.clone(),
            };
            state
                .pending_handshakes
                .lock()
                .await
                .insert(request.instance_id, pending);

            (StatusCode::OK, Json(serde_json::to_value(&response).unwrap())).into_response()
        }
        Err(e) => {
            tracing::warn!(peer = %request.instance_id, error = %e, "Handshake failed");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(&ErrorResponse {
                    error: e.to_string(),
                })
                .unwrap()),
            )
                .into_response()
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
async fn handle_handshake_confirm<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    Json(request): Json<HandshakeConfirmRequest>,
) -> impl IntoResponse {
    tracing::info!(
        peer = %request.instance_id,
        "Received federation handshake confirm"
    );

    // Look up the pending handshake
    let pending = state
        .pending_handshakes
        .lock()
        .await
        .remove(&request.instance_id);

    let pending = match pending {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(&ErrorResponse {
                    error: "no pending handshake for this instance".to_string(),
                })
                .unwrap()),
            )
                .into_response();
        }
    };

    // Verify the confirm signature
    match state.auth.verify_confirm(
        &request.confirm,
        &pending.counter_nonce,
        &pending.initiator_vk_hex,
    ) {
        Ok(()) => {
            tracing::info!(peer = %request.instance_id, "Handshake confirmed successfully");

            #[derive(Serialize)]
            struct ConfirmResponse {
                status: String,
            }

            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(&ConfirmResponse {
                        status: "confirmed".to_string(),
                    })
                    .unwrap(),
                ),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(peer = %request.instance_id, error = %e, "Handshake confirm failed");
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::to_value(&ErrorResponse {
                    error: e.to_string(),
                })
                .unwrap()),
            )
                .into_response()
        }
    }
}

/// GET /federation/segments/{id}
///
/// Returns a segment by its UUID. Used by peers to fetch full segment data
/// after receiving an announcement.
async fn handle_get_segment<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(&ErrorResponse {
                    error: format!("invalid segment ID: {e}"),
                })
                .unwrap()),
            )
                .into_response();
        }
    };

    let segment_id = SegmentId(uuid);

    match state.store.get(segment_id) {
        Ok(Some(segment)) => {
            (StatusCode::OK, Json(serde_json::to_value(&segment).unwrap())).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(
                serde_json::to_value(&ErrorResponse {
                    error: format!("segment {id} not found"),
                })
                .unwrap(),
            ),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::to_value(&ErrorResponse {
                error: format!("failed to retrieve segment: {e}"),
                })
                .unwrap(),
            ),
        )
            .into_response(),
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
/// Debug endpoint: returns a list of all known peers as JSON.
async fn handle_list_peers<S: VectorStore + 'static>(
    State(state): State<Arc<ServerState<S>>>,
) -> impl IntoResponse {
    let peers = state.peers.read();
    let summaries: Vec<PeerSummary> = peers.all_peers().iter().map(|p| PeerSummary::from(*p)).collect();

    (StatusCode::OK, Json(serde_json::to_value(&summaries).unwrap()))
}
