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
    let _eve_id = AnimusIdentity::generate("test-model".to_string());
    let alice = FederationAuth::new(alice_id);
    let bob = FederationAuth::new(bob_id);

    let (request, _alice_nonce) = alice.create_handshake();
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
