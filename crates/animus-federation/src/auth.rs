use animus_core::{AnimusError, AnimusIdentity, Result};
use crate::protocol::{HandshakeConfirm, HandshakeRequest, HandshakeResponse, PROTOCOL_VERSION};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

const REPLAY_WINDOW_SECS: i64 = 30;

pub struct FederationAuth {
    identity: AnimusIdentity,
}

impl FederationAuth {
    pub fn new(identity: AnimusIdentity) -> Self {
        Self { identity }
    }

    pub fn instance_id(&self) -> animus_core::InstanceId {
        self.identity.instance_id
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.identity.verifying_key()
    }

    pub fn create_handshake(&self) -> (HandshakeRequest, [u8; 32]) {
        let mut nonce = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce);

        let vk_hex = hex::encode(self.identity.verifying_key().to_bytes());
        let request = HandshakeRequest {
            instance_id: self.identity.instance_id,
            verifying_key_hex: vk_hex,
            nonce,
            protocol_version: PROTOCOL_VERSION,
        };
        (request, nonce)
    }

    pub fn respond_to_handshake(&self, req: &HandshakeRequest) -> Result<(HandshakeResponse, [u8; 32])> {
        if req.protocol_version != PROTOCOL_VERSION {
            return Err(AnimusError::Federation(
                format!("protocol version mismatch: expected {}, got {}", PROTOCOL_VERSION, req.protocol_version)
            ));
        }

        // Sign the initiator's nonce
        let sig = self.identity.signing_key.sign(&req.nonce);
        let sig_hex = hex::encode(sig.to_bytes());

        let mut counter_nonce = [0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut counter_nonce);

        let vk_hex = hex::encode(self.identity.verifying_key().to_bytes());
        let response = HandshakeResponse {
            instance_id: self.identity.instance_id,
            verifying_key_hex: vk_hex,
            signature_hex: sig_hex,
            counter_nonce,
            protocol_version: PROTOCOL_VERSION,
        };
        Ok((response, counter_nonce))
    }

    pub fn verify_response_and_confirm(
        &self,
        resp: &HandshakeResponse,
        original_nonce: &[u8; 32],
    ) -> Result<HandshakeConfirm> {
        // Verify responder signed our nonce
        let vk_bytes = hex::decode(&resp.verifying_key_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key hex: {e}")))?;
        let vk_arr: [u8; 32] = vk_bytes.try_into()
            .map_err(|_| AnimusError::Federation("verifying key must be 32 bytes".to_string()))?;
        let vk = VerifyingKey::from_bytes(&vk_arr)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key: {e}")))?;

        let sig_bytes = hex::decode(&resp.signature_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid signature hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into()
            .map_err(|_| AnimusError::Federation("signature must be 64 bytes".to_string()))?;
        let sig = Signature::from_bytes(&sig_arr);

        vk.verify(original_nonce, &sig)
            .map_err(|e| AnimusError::Federation(format!("handshake signature verification failed: {e}")))?;

        // Sign the counter nonce
        let counter_sig = self.identity.signing_key.sign(&resp.counter_nonce);
        let confirm = HandshakeConfirm {
            signature_hex: hex::encode(counter_sig.to_bytes()),
        };
        Ok(confirm)
    }

    pub fn verify_confirm(
        &self,
        confirm: &HandshakeConfirm,
        counter_nonce: &[u8; 32],
        initiator_vk_hex: &str,
    ) -> Result<()> {
        let vk_bytes = hex::decode(initiator_vk_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key hex: {e}")))?;
        let vk_arr: [u8; 32] = vk_bytes.try_into()
            .map_err(|_| AnimusError::Federation("verifying key must be 32 bytes".to_string()))?;
        let vk = VerifyingKey::from_bytes(&vk_arr)
            .map_err(|e| AnimusError::Federation(format!("invalid verifying key: {e}")))?;

        let sig_bytes = hex::decode(&confirm.signature_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid signature hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into()
            .map_err(|_| AnimusError::Federation("signature must be 64 bytes".to_string()))?;
        let sig = Signature::from_bytes(&sig_arr);

        vk.verify(counter_nonce, &sig)
            .map_err(|e| AnimusError::Federation(format!("handshake confirm verification failed: {e}")))?;

        Ok(())
    }

    /// Sign a request: signature over "{timestamp}:{path}:{sha256_of_body}".
    pub fn sign_request(&self, timestamp: i64, path: &str, body: &[u8]) -> String {
        let body_hash = Sha256::digest(body);
        let message = format!("{}:{}:{}", timestamp, path, hex::encode(body_hash));
        let sig = self.identity.signing_key.sign(message.as_bytes());
        hex::encode(sig.to_bytes())
    }

    /// Verify a signed request. Rejects if timestamp is older than REPLAY_WINDOW_SECS.
    pub fn verify_signed_request(
        timestamp: i64,
        path: &str,
        body: &[u8],
        signature_hex: &str,
        peer_vk: &VerifyingKey,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let skew = now - timestamp;
        if skew > REPLAY_WINDOW_SECS {
            return Err(AnimusError::Federation(
                format!("request timestamp too old: {skew}s (max {REPLAY_WINDOW_SECS}s)")
            ));
        }
        // Reject timestamps more than 5 seconds in the future (clock skew tolerance).
        if skew < -5 {
            return Err(AnimusError::Federation(
                format!("request timestamp is {}s in the future", -skew)
            ));
        }

        let body_hash = Sha256::digest(body);
        let message = format!("{}:{}:{}", timestamp, path, hex::encode(body_hash));

        let sig_bytes = hex::decode(signature_hex)
            .map_err(|e| AnimusError::Federation(format!("invalid signature hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into()
            .map_err(|_| AnimusError::Federation("signature must be 64 bytes".to_string()))?;
        let sig = Signature::from_bytes(&sig_arr);

        peer_vk.verify(message.as_bytes(), &sig)
            .map_err(|e| AnimusError::Federation(format!("request signature verification failed: {e}")))?;

        Ok(())
    }
}
