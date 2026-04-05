use crate::error::{BlossomLfsError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimal Nostr event for Blossom auth (kind 24242).
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

/// Compute SHA256 event ID as per NIP-01.
pub fn compute_event_id(
    pubkey: &str,
    created_at: u64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
) -> [u8; 32] {
    let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
    let serialized = format!(
        "[0,\"{}\",{},{},{} ,\"{}\"]",
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

/// Base64url encoding (no external crate dependency).
fn base64url_encode(data: &[u8]) -> String {
    const BASE64_CHARS: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
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
        use secp256k1::{Keypair, PublicKey, Secp256k1, SecretKey, Signing};

        let secp = Secp256k1::signing_only();
        let secret_key_obj = SecretKey::from_slice(secret_key)
            .map_err(|e| BlossomLfsError::NostrSigning(e.to_string()))?;
        let keypair = Keypair::from_secret_key(&secp, &secret_key_obj);
        let public_key = PublicKey::from_keypair(&keypair);
        let pubkey_hex = hex::encode(public_key.serialize());

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

        let id_bytes = compute_event_id(&pubkey_hex, created_at, kind, &tags, content);
        let id = hex::encode(id_bytes);

        let message = secp256k1::Message::from_digest_slice(&id_bytes)
            .map_err(|e| BlossomLfsError::NostrSigning(e.to_string()))?;
        let sig = secp.sign_schnorr(&message, &keypair);
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
    fn test_base64url_encode() {
        let input = b"hello world";
        let encoded = base64url_encode(input);
        assert!(encoded.contains('-') || !encoded.contains('+'));
        assert!(!encoded.contains('/'));
    }
}
