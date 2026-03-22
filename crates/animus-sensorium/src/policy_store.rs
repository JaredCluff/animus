use animus_core::sensorium::ConsentPolicy;
use std::path::Path;

/// Persists consent policies as JSON.
pub struct PolicyStore;

impl PolicyStore {
    pub fn save(path: &Path, policies: &[ConsentPolicy]) -> animus_core::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(policies)?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> animus_core::Result<Vec<ConsentPolicy>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(path)?;
        let policies: Vec<ConsentPolicy> = serde_json::from_str(&data)?;
        Ok(policies)
    }
}
