use animus_core::AnimusIdentity;
use tempfile::TempDir;

#[test]
fn test_identity_generation() {
    let identity = AnimusIdentity::generate("test-model".to_string());
    assert_eq!(identity.generation, 0);
    assert!(identity.parent_id.is_none());
    assert_eq!(identity.base_model, "test-model");
}

#[test]
fn test_identity_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("identity.bin");

    let original = AnimusIdentity::load_or_generate(&path, "test-model").unwrap();
    let loaded = AnimusIdentity::load_or_generate(&path, "test-model").unwrap();

    assert_eq!(original.instance_id, loaded.instance_id);
    assert_eq!(original.generation, loaded.generation);
    assert_eq!(original.base_model, loaded.base_model);
    assert_eq!(
        original.signing_key.to_bytes(),
        loaded.signing_key.to_bytes()
    );
}

#[test]
fn test_identity_corrupted_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("identity.bin");

    // Write garbage data
    std::fs::write(&path, b"this is not valid bincode data").unwrap();

    let result = AnimusIdentity::load_or_generate(&path, "test-model");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("failed to load identity"), "Error should mention identity load failure, got: {err}");
}

#[test]
fn test_identity_verifying_key() {
    let identity = AnimusIdentity::generate("test-model".to_string());
    let vk = identity.verifying_key();
    assert_eq!(vk, identity.signing_key.verifying_key());
}

#[test]
fn test_identity_fork() {
    let parent = AnimusIdentity::generate("test-model".to_string());
    let child = parent.fork();

    // Child should have a different instance ID and keypair
    assert_ne!(child.instance_id, parent.instance_id);
    assert_ne!(child.signing_key.to_bytes(), parent.signing_key.to_bytes());

    // Child should record parent lineage
    assert_eq!(child.parent_id, Some(parent.instance_id));
    assert_eq!(child.generation, parent.generation + 1);
    assert_eq!(child.base_model, parent.base_model);
}

#[test]
fn test_identity_fork_chain() {
    let gen0 = AnimusIdentity::generate("test-model".to_string());
    let gen1 = gen0.fork();
    let gen2 = gen1.fork();

    assert_eq!(gen0.generation, 0);
    assert_eq!(gen1.generation, 1);
    assert_eq!(gen2.generation, 2);
    assert_eq!(gen2.parent_id, Some(gen1.instance_id));
}
