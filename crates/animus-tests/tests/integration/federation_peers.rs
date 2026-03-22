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
