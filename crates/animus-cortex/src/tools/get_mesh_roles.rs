//! Introspective tool: get_mesh_roles
//!
//! Allows the AILF reasoning thread to inspect the current Role-Capability Mesh:
//! which roles are assigned to which instances, and all known capability attestations.
//! Use this after a tier-change Signal or role-yield Signal to understand the mesh state.

use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct GetMeshRolesTool;

#[async_trait::async_trait]
impl Tool for GetMeshRolesTool {
    fn name(&self) -> &str { "get_mesh_roles" }

    fn description(&self) -> &str {
        "Return the current Role-Capability Mesh: role assignments (which instance holds each role), \
         all known peer attestations (tier, load, domains), and this instance's held roles. \
         Use this after a cognitive tier-change Signal or role-yield notification to understand \
         the current federation state."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, _params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let Some(role_mesh) = &ctx.role_mesh else {
            return Ok(ToolResult {
                content: "Role mesh not initialized (federation not configured).".to_string(),
                is_error: true,
            });
        };

        let mesh = role_mesh.read();

        let mut lines = Vec::new();

        // Role assignments
        lines.push("## Role Assignments".to_string());
        if mesh.assignments.is_empty() {
            lines.push("  (no roles assigned)".to_string());
        } else {
            let mut sorted: Vec<_> = mesh.assignments.iter().collect();
            sorted.sort_by_key(|(r, _)| r.label());
            for (role, instance_id) in sorted {
                lines.push(format!("  {} → {}", role.label(), instance_id));
            }
        }

        lines.push(String::new());
        lines.push("## Known Attestations".to_string());

        if mesh.attestations.is_empty() {
            lines.push("  (no attestations received)".to_string());
        } else {
            let mut sorted: Vec<_> = mesh.attestations.values().collect();
            sorted.sort_by_key(|a| a.attestation.instance_id.to_string());
            for va in sorted {
                let att = &va.attestation;
                let roles: Vec<&str> = att.active_roles.iter().map(|r| r.label()).collect();
                lines.push(format!(
                    "  Instance {} — tier: {} | load: {:.0}% | roles: [{}] | domains: [{}]",
                    att.instance_id,
                    att.cognitive_tier.label(),
                    att.load * 100.0,
                    roles.join(", "),
                    att.available_domains.join(", "),
                ));
            }
        }

        tracing::debug!(
            "get_mesh_roles: {} assignments, {} attestations",
            mesh.assignments.len(),
            mesh.attestations.len()
        );

        Ok(ToolResult { content: lines.join("\n"), is_error: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use animus_core::mesh::{AttestationFields, CapabilityAttestation, MeshRole, RoleMesh, VerifiedAttestation};
    use animus_core::capability::CognitiveTier;
    use animus_core::identity::InstanceId;
    use chrono::Utc;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use std::sync::Arc;

    fn make_ctx_no_mesh() -> ToolContext {
        let (signal_tx, _rx) = tokio::sync::mpsc::channel(8);
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(animus_vectorfs::store::MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(animus_embed::SyntheticEmbedding::new(4));
        ToolContext {
            data_dir: tmp.path().to_path_buf(),
            snapshot_dir: tmp.path().join("snapshots"),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: Some(signal_tx),
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: None,
            self_event_filter: None,
            api_tracker: None,
            nats_client: None,
            federation_tx: None,
            smart_router: None,
            capability_state: None,
            role_mesh: None,
        }
    }

    fn make_ctx_with_mesh(mesh: RoleMesh) -> ToolContext {
        let mut ctx = make_ctx_no_mesh();
        ctx.role_mesh = Some(Arc::new(parking_lot::RwLock::new(mesh)));
        ctx
    }

    #[tokio::test]
    async fn no_mesh_returns_error() {
        let ctx = make_ctx_no_mesh();
        let result = GetMeshRolesTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not initialized"));
    }

    #[tokio::test]
    async fn empty_mesh_shows_no_assignments() {
        let ctx = make_ctx_with_mesh(RoleMesh::new());
        let result = GetMeshRolesTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("no roles assigned"));
        assert!(result.content.contains("no attestations received"));
    }

    #[tokio::test]
    async fn mesh_with_assignment_and_attestation() {
        let mut mesh = RoleMesh::new();
        let id = InstanceId::new();

        // Add a role assignment
        mesh.assign_role(MeshRole::Analyst, id);

        // Add a verified attestation
        let signing_key = SigningKey::generate(&mut OsRng);
        let fields = AttestationFields {
            instance_id: id,
            cognitive_tier: CognitiveTier::Strong,
            active_roles: vec![MeshRole::Analyst],
            available_domains: vec!["reasoning".to_string()],
            load: 0.3,
            signed_at: Utc::now(),
        };
        let att = CapabilityAttestation::sign(fields, &signing_key);
        mesh.insert_verified(VerifiedAttestation { attestation: att, verified_at: Utc::now() });

        let ctx = make_ctx_with_mesh(mesh);
        let result = GetMeshRolesTool.execute(serde_json::json!({}), &ctx).await.unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("Analyst"));
        assert!(result.content.contains("Strong"));
        assert!(result.content.contains("30%"));
    }
}
