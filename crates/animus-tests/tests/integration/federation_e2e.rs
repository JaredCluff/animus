use std::net::SocketAddr;
use std::sync::Arc;

use animus_core::AnimusIdentity;
use animus_federation::auth::FederationAuth;
use animus_federation::peers::PeerRegistry;
use animus_federation::protocol::HandshakeResponse;
use animus_federation::server::FederationServer;
use animus_vectorfs::store::MmapVectorStore;
use serde_json::json;

/// Helper: create a FederationServer backed by a temp-dir MmapVectorStore,
/// start it on 127.0.0.1:0 and return the bound address.
async fn start_server(identity: AnimusIdentity) -> SocketAddr {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let store = Arc::new(
        MmapVectorStore::open(tmp.path(), 128).expect("failed to open vector store"),
    );
    let auth = FederationAuth::new(identity);
    let peers = PeerRegistry::new();
    let server = FederationServer::new(auth, peers, store, 60);
    // Leak the tempdir so it survives for the duration of the test
    let _ = Box::leak(Box::new(tmp));
    server
        .start("127.0.0.1:0".parse().unwrap())
        .await
        .expect("failed to start federation server")
}

/// Full handshake flow: Alice initiates a handshake with Bob's HTTP server,
/// then confirms mutual authentication.
#[tokio::test]
async fn two_ailfs_handshake_over_http() {
    let alice_id = AnimusIdentity::generate("test-model".to_string());
    let bob_id = AnimusIdentity::generate("test-model".to_string());

    let alice_auth = FederationAuth::new(alice_id);

    // Start Bob's federation server
    let bob_addr = start_server(bob_id).await;
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
}

/// List peers via the GET /federation/peers endpoint on a fresh server.
#[tokio::test]
async fn list_peers_via_http() {
    let identity = AnimusIdentity::generate("test-model".to_string());
    let addr = start_server(identity).await;
    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base_url}/federation/peers"))
        .send()
        .await
        .expect("list peers request failed");
    assert_eq!(resp.status(), 200, "list peers should return 200");

    let peers: Vec<serde_json::Value> = resp
        .json()
        .await
        .expect("failed to parse peers response");
    assert!(peers.is_empty(), "fresh server should have no peers");
}
