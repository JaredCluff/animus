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
