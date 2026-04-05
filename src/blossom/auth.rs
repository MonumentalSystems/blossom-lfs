use crate::error::{BlossomLfsError, Result};
use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// NIP-01 Nostr event (minimal, just what Blossom needs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NostrEvent {
    pub id: String,
    pub pubkey: String,
    #[serde(rename = "created_at")]
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// Compute NIP-01 event ID: SHA256([0, pubkey, created_at, kind, tags, content]).
pub fn compute_event_id(
    pubkey: &str,
    created_at: u64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
) -> [u8; 32] {
    let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
    let serialized = format!(
        "[0,\"{}\",{},{},{},\"{}\"]",
        pubkey,
        created_at,
        kind,
        tags_json,
        content.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Base64url encoding.
fn base64url_encode(data: &[u8]) -> String {
    const BASE64_CHARS: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        }
    }
    result.replace('+', "-").replace('/', "_")
}

#[derive(Debug, Clone)]
pub struct AuthToken {
    pub event: NostrEvent,
}

impl AuthToken {
    pub fn new(
        secret_key: &[u8; 32],
        action: ActionType,
        server_domain: Option<&str>,
        blob_hashes: Option<Vec<&str>>,
        expiration_seconds: u64,
    ) -> Result<Self> {
        // Reconstruct keypair from secret key
        let secp = Secp256k1::signing_only();
        let secret_key_obj = SecretKey::from_slice(secret_key)
            .map_err(|e| BlossomLfsError::NostrSigning(e.to_string()))?;
        let keypair = Keypair::from_secret_key(&secp, &secret_key_obj);
        let (x_only_pubkey, _parity) = keypair.x_only_public_key();

        // Get 32-byte x-only pubkey
        let pubkey_hex = hex::encode(x_only_pubkey.serialize());

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BlossomLfsError::NostrSigning(e.to_string()))?
            .as_secs();

        let kind = 24242;

        let mut tags = vec![vec!["t".to_string(), action.to_string()]];

        if let Some(domain) = server_domain {
            tags.push(vec!["server".to_string(), domain.to_string()]);
        }

        if let Some(hashes) = blob_hashes {
            for hash in hashes {
                tags.push(vec!["x".to_string(), hash.to_string()]);
            }
        }

        let expiration = created_at + expiration_seconds;
        tags.push(vec!["expiration".to_string(), expiration.to_string()]);

        let content = action.description();

        // Compute event ID
        let id_bytes = compute_event_id(&pubkey_hex, created_at, kind, &tags, content);
        let id = hex::encode(id_bytes);

        // Sign with BIP-340 Schnorr
        let message = Message::from_digest_slice(&id_bytes)
            .map_err(|e| BlossomLfsError::NostrSigning(e.to_string()))?;
        let sig = secp.sign_schnorr_no_aux_rand(&message, &keypair);
        let sig_hex = hex::encode(sig.serialize());

        let event = NostrEvent {
            id,
            pubkey: pubkey_hex,
            created_at,
            kind,
            tags,
            content: content.to_string(),
            sig: sig_hex,
        };

        Ok(AuthToken { event })
    }

    pub fn to_authorization_header(&self) -> Result<String> {
        let json = serde_json::to_string(&self.event).map_err(BlossomLfsError::Serialization)?;
        let encoded = base64url_encode(json.as_bytes());
        Ok(format!("Nostr {}", encoded))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ActionType {
    Get,
    Upload,
    List,
    Delete,
    Media,
}

impl std::fmt::Display for ActionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionType::Get => write!(f, "get"),
            ActionType::Upload => write!(f, "upload"),
            ActionType::List => write!(f, "list"),
            ActionType::Delete => write!(f, "delete"),
            ActionType::Media => write!(f, "media"),
        }
    }
}

impl ActionType {
    fn description(&self) -> &'static str {
        match self {
            ActionType::Get => "Download Blob",
            ActionType::Upload => "Upload Blob",
            ActionType::List => "List Blobs",
            ActionType::Delete => "Delete Blob",
            ActionType::Media => "Upload Media",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_token_creation() {
        let secret_key = [1u8; 32];
        let auth = AuthToken::new(
            &secret_key,
            ActionType::Upload,
            Some("localhost:12345"),
            None,
            3600,
        );
        assert!(auth.is_ok());
    }
}
