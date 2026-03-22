use std::net::SocketAddr;
use std::sync::Arc;

use animus_core::{AnimusIdentity, PolicyId};
use animus_federation::auth::FederationAuth;
use animus_federation::knowledge::{
    FederationPermission, FederationPolicy, FederationRule, FederationScope, KnowledgeSharing,
};
use animus_federation::peers::PeerRegistry;
use animus_federation::protocol::HandshakeResponse;
use animus_federation::server::FederationServer;
use animus_vectorfs::store::MmapVectorStore;
use parking_lot::RwLock;
use serde_json::json;

/// Helper: create a FederationServer backed by a temp-dir MmapVectorStore,
/// start it on 127.0.0.1:0 and return the bound address and shared peer registry.
async fn start_server(identity: AnimusIdentity) -> (SocketAddr, Arc<RwLock<PeerRegistry>>) {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let store = Arc::new(
        MmapVectorStore::open(tmp.path(), 128).expect("failed to open vector store"),
    );
    let auth = FederationAuth::new(identity);
    let peers = Arc::new(RwLock::new(PeerRegistry::new()));
    let knowledge = KnowledgeSharing::new(
        vec![FederationPolicy {
            id: PolicyId::new(),
            name: "test-default".to_string(),
            active: true,
            publish_rules: vec![FederationRule {
                scope: FederationScope::AllNonPrivate,
                permission: FederationPermission::Allow,
            }],
            subscribe_rules: vec![],
        }],
        0.5,
    );
    let server = FederationServer::new(auth, peers.clone(), store, knowledge, 60);
    // Leak the tempdir so it survives for the duration of the test
    let _ = Box::leak(Box::new(tmp));
    let addr = server
        .start("127.0.0.1:0".parse().unwrap())
        .await
        .expect("failed to start federation server");
    (addr, peers)
}

/// Full handshake flow: Alice initiates a handshake with Bob's HTTP server,
/// then confirms mutual authentication. After handshake, Alice can access
/// protected endpoints with signed requests.
#[tokio::test]
async fn two_ailfs_handshake_over_http() {
    let alice_id = AnimusIdentity::generate("test-model".to_string());
    let bob_id = AnimusIdentity::generate("test-model".to_string());

    let alice_auth = FederationAuth::new(alice_id);

    // Start Bob's federation server
    let (bob_addr, _bob_peers) = start_server(bob_id).await;
    let base_url = format!("http://{bob_addr}");
    let client = reqwest::Client::new();

    // Step 1: Alice creates and sends a handshake request
    let (request, alice_nonce) = alice_auth.create_handshake();
    let resp = client
        .post(format!("{base_url}/federation/handshake"))
        .json(&request)
        .send()
        .await
        .expect("handshake request failed");
    assert_eq!(resp.status(), 200, "handshake should return 200");

    // Step 2: Parse Bob's response
    let handshake_resp: HandshakeResponse = resp
        .json()
        .await
        .expect("failed to parse HandshakeResponse");

    // Step 3: Alice verifies Bob's signature on her nonce and creates confirm
    let confirm = alice_auth
        .verify_response_and_confirm(&handshake_resp, &alice_nonce)
        .expect("verify_response_and_confirm failed");

    // Step 4: Send confirm with instance_id (the server expects
    // HandshakeConfirmRequest { instance_id, ...confirm })
    let confirm_body = json!({
        "instance_id": request.instance_id,
        "signature_hex": confirm.signature_hex,
    });

    let resp = client
        .post(format!("{base_url}/federation/handshake/confirm"))
        .json(&confirm_body)
        .send()
        .await
        .expect("handshake confirm request failed");
    assert_eq!(resp.status(), 200, "handshake confirm should return 200");

    // Verify the response body indicates success
    let body: serde_json::Value = resp.json().await.expect("failed to parse confirm response");
    assert_eq!(body["status"], "confirmed");

    // Step 5: After handshake, Alice can access protected endpoints with signed requests
    let timestamp = chrono::Utc::now().timestamp();
    let path = "/federation/peers";
    let signature = alice_auth.sign_request(timestamp, path, b"");

    let resp = client
        .get(format!("{base_url}{path}"))
        .header("X-Animus-Instance-Id", request.instance_id.to_string())
        .header("X-Animus-Timestamp", timestamp.to_string())
        .header("X-Animus-Signature", &signature)
        .send()
        .await
        .expect("authenticated peers request failed");
    assert_eq!(
        resp.status(),
        200,
        "authenticated peers request should return 200"
    );

    let peers: Vec<serde_json::Value> = resp
        .json()
        .await
        .expect("failed to parse peers response");
    // Alice should appear as a Verified peer (M6)
    assert_eq!(peers.len(), 1, "should have 1 peer (Alice)");
    assert_eq!(peers[0]["trust"], "Verified");
}

/// Helper: perform full handshake and return (base_url, auth, instance_id)
/// so tests can make authenticated requests.
async fn handshake_and_auth(
    server_identity: AnimusIdentity,
) -> (String, FederationAuth, animus_core::InstanceId) {
    let client_id = AnimusIdentity::generate("test-model".to_string());
    let client_auth = FederationAuth::new(client_id);
    let (addr, _peers) = start_server(server_identity).await;
    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Handshake
    let (request, nonce) = client_auth.create_handshake();
    let instance_id = request.instance_id;
    let resp = client
        .post(format!("{base_url}/federation/handshake"))
        .json(&request)
        .send()
        .await
        .unwrap();
    let handshake_resp: HandshakeResponse = resp.json().await.unwrap();
    let confirm = client_auth
        .verify_response_and_confirm(&handshake_resp, &nonce)
        .unwrap();
    let confirm_body = json!({
        "instance_id": instance_id,
        "signature_hex": confirm.signature_hex,
    });
    client
        .post(format!("{base_url}/federation/handshake/confirm"))
        .json(&confirm_body)
        .send()
        .await
        .unwrap();

    (base_url, client_auth, instance_id)
}

/// POST /federation/publish accepts segment announcements from authenticated peers.
#[tokio::test]
async fn publish_segment_announcement() {
    let server_id = AnimusIdentity::generate("test-model".to_string());
    let (base_url, auth, instance_id) = handshake_and_auth(server_id).await;
    let client = reqwest::Client::new();

    let announcement = json!({
        "segment_id": animus_core::SegmentId::new(),
        "embedding": vec![0.1f32; 128],
        "content_kind": "Text",
        "created": chrono::Utc::now().to_rfc3339(),
        "tags": {}
    });
    let body_bytes = serde_json::to_vec(&announcement).unwrap();

    let timestamp = chrono::Utc::now().timestamp();
    let path = "/federation/publish";
    let signature = auth.sign_request(timestamp, path, &body_bytes);

    let resp = client
        .post(format!("{base_url}{path}"))
        .header("X-Animus-Instance-Id", instance_id.to_string())
        .header("X-Animus-Timestamp", timestamp.to_string())
        .header("X-Animus-Signature", &signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .expect("publish request failed");
    assert_eq!(resp.status(), 200, "publish should return 200");

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
    assert!(body["local_segment_id"].is_string());
}

/// POST /federation/goals accepts goal announcements from authenticated peers.
#[tokio::test]
async fn goals_announcement() {
    let server_id = AnimusIdentity::generate("test-model".to_string());
    let (base_url, auth, instance_id) = handshake_and_auth(server_id).await;
    let client = reqwest::Client::new();

    let announcement = json!({
        "goal_id": animus_core::GoalId::new(),
        "description": "Learn about quantum computing",
        "priority": "Normal",
        "source_ailf": instance_id,
    });
    let body_bytes = serde_json::to_vec(&announcement).unwrap();

    let timestamp = chrono::Utc::now().timestamp();
    let path = "/federation/goals";
    let signature = auth.sign_request(timestamp, path, &body_bytes);

    let resp = client
        .post(format!("{base_url}{path}"))
        .header("X-Animus-Instance-Id", instance_id.to_string())
        .header("X-Animus-Timestamp", timestamp.to_string())
        .header("X-Animus-Signature", &signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .expect("goals request failed");
    assert_eq!(resp.status(), 200, "goals should return 200");

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
}

/// Unauthenticated access to protected endpoints should be rejected.
#[tokio::test]
async fn unauthenticated_access_rejected() {
    let identity = AnimusIdentity::generate("test-model".to_string());
    let (addr, _peers) = start_server(identity).await;
    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Try to access peers endpoint without auth headers
    let resp = client
        .get(format!("{base_url}/federation/peers"))
        .send()
        .await
        .expect("request failed");
    assert_eq!(
        resp.status(),
        401,
        "unauthenticated request should return 401"
    );
}
