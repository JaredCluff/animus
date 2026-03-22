use animus_core::{AnimusError, GoalId, InstanceId, Result, Segment, SegmentId};
use animus_cortex::Priority;
use reqwest::Client;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::auth::FederationAuth;
use crate::protocol::{ContentKind, GoalAnnouncement, SegmentAnnouncement};

/// HTTP client for outbound federation requests to peers.
pub struct FederationClient {
    client: Client,
    auth: FederationAuth,
}

impl FederationClient {
    pub fn new(auth: FederationAuth) -> Self {
        Self {
            client: Client::new(),
            auth,
        }
    }

    /// Announce a segment to a remote peer.
    pub async fn publish_segment(
        &self,
        peer_addr: SocketAddr,
        segment: &Segment,
    ) -> Result<SegmentId> {
        let content_kind = match &segment.content {
            animus_core::Content::Text(_) => ContentKind::Text,
            animus_core::Content::Structured(_) => ContentKind::Structured,
            animus_core::Content::Binary { .. } => ContentKind::Binary,
            animus_core::Content::Reference { .. } => ContentKind::Reference,
        };

        let announcement = SegmentAnnouncement {
            segment_id: segment.id,
            embedding: segment.embedding.clone(),
            content_kind,
            created: segment.created,
            tags: HashMap::new(),
        };

        let body = serde_json::to_vec(&announcement)?;
        let path = "/federation/publish";
        let timestamp = chrono::Utc::now().timestamp();
        let signature = self.auth.sign_request(timestamp, path, &body);

        let url = format!("http://{peer_addr}{path}");
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Animus-Instance-Id", self.auth.instance_id().0.to_string())
            .header("X-Animus-Timestamp", timestamp.to_string())
            .header("X-Animus-Signature", signature)
            .body(body)
            .send()
            .await
            .map_err(|e| AnimusError::Federation(format!("publish request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Federation(format!(
                "publish rejected by peer ({status}): {body}"
            )));
        }

        #[derive(serde::Deserialize)]
        struct PublishResponse {
            local_segment_id: SegmentId,
        }

        let result: PublishResponse = resp
            .json()
            .await
            .map_err(|e| AnimusError::Federation(format!("invalid publish response: {e}")))?;

        Ok(result.local_segment_id)
    }

    /// Announce a goal to a remote peer.
    pub async fn publish_goal(
        &self,
        peer_addr: SocketAddr,
        goal_id: GoalId,
        description: &str,
        priority: Priority,
    ) -> Result<()> {
        let announcement = GoalAnnouncement {
            goal_id,
            description: description.to_string(),
            priority,
            source_ailf: self.auth.instance_id(),
        };

        let body = serde_json::to_vec(&announcement)?;
        let path = "/federation/goals";
        let timestamp = chrono::Utc::now().timestamp();
        let signature = self.auth.sign_request(timestamp, path, &body);

        let url = format!("http://{peer_addr}{path}");
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Animus-Instance-Id", self.auth.instance_id().0.to_string())
            .header("X-Animus-Timestamp", timestamp.to_string())
            .header("X-Animus-Signature", signature)
            .body(body)
            .send()
            .await
            .map_err(|e| AnimusError::Federation(format!("goal publish failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Federation(format!(
                "goal publish rejected by peer ({status}): {body}"
            )));
        }

        Ok(())
    }

    /// Fetch a full segment from a peer by ID.
    pub async fn fetch_segment(
        &self,
        peer_addr: SocketAddr,
        segment_id: SegmentId,
    ) -> Result<Segment> {
        let path = format!("/federation/segments/{}", segment_id.0);
        let timestamp = chrono::Utc::now().timestamp();
        let signature = self.auth.sign_request(timestamp, &path, &[]);

        let url = format!("http://{peer_addr}{path}");
        let resp = self
            .client
            .get(&url)
            .header("X-Animus-Instance-Id", self.auth.instance_id().0.to_string())
            .header("X-Animus-Timestamp", timestamp.to_string())
            .header("X-Animus-Signature", signature)
            .send()
            .await
            .map_err(|e| AnimusError::Federation(format!("fetch segment failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AnimusError::Federation(format!(
                "fetch segment rejected ({status}): {body}"
            )));
        }

        let segment: Segment = resp
            .json()
            .await
            .map_err(|e| AnimusError::Federation(format!("invalid segment response: {e}")))?;

        Ok(segment)
    }

    /// Broadcast a segment announcement to all provided peer addresses.
    pub async fn broadcast_segment(
        &self,
        peers: &[(InstanceId, SocketAddr)],
        segment: &Segment,
    ) -> Vec<(InstanceId, Result<SegmentId>)> {
        let mut results = Vec::new();
        for (id, addr) in peers {
            let result = self.publish_segment(*addr, segment).await;
            results.push((*id, result));
        }
        results
    }
}
