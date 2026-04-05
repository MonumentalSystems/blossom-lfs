// End-to-end test with mock Blossom server

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use axum::{
    Router,
    routing::{get, put, head},
    Json,
    http::{StatusCode, HeaderMap},
    body::Bytes,
    extract::State,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobDescriptor {
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub url: String,
    pub uploaded: u64,
}

#[derive(Debug)]
pub struct BlobStore {
    blobs: HashMap<String, Vec<u8>>,
}

impl BlobStore {
    pub fn new() -> Self {
        Self {
            blobs: HashMap::new(),
        }
    }

    pub fn insert(&mut self, data: Vec<u8>) -> BlobDescriptor {
        let hash = format!("{:x}", Sha256::digest(&data));
        let size = data.len() as u64;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.blobs.insert(hash.clone(), data);

        BlobDescriptor {
            sha256: hash.clone(),
            size,
            content_type: Some("application/octet-stream".into()),
            url: format!("/{}", hash),
            uploaded: ts,
        }
    }

    pub fn get(&self, sha256: &str) -> Option<&Vec<u8>> {
        self.blobs.get(sha256)
    }

    pub fn exists(&self, sha256: &str) -> bool {
        self.blobs.contains_key(sha256)
    }
}

type SharedBlobStore = Arc<Mutex<BlobStore>>;

async fn upload_blob(
    State(store): State<SharedBlobStore>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<BlobDescriptor>, (StatusCode, String)> {
    let mut store = store.lock().await;
    
    let descriptor = store.insert(body.to_vec());
    
    // Verify hash if provided
    if let Some(expected_hash) = headers.get("X-SHA-256") {
        if expected_hash.to_str().map(|h| h != descriptor.sha256).unwrap_or(true) {
            return Err((StatusCode::BAD_REQUEST, "Hash mismatch".to_string()));
        }
    }
    
    Ok(Json(descriptor))
}

async fn get_blob_handler(
    State(store): State<SharedBlobStore>,
    path: axum::extract::Path<String>,
) -> Result<Vec<u8>, StatusCode> {
    let store = store.lock().await;
    let sha256 = path.trim_start_matches('/');
    
    store.get(sha256)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)
}

async fn head_blob_handler(
    State(store): State<SharedBlobStore>,
    path: axum::extract::Path<String>,
) -> Result<(), StatusCode> {
    let store = store.lock().await;
    let sha256 = path.trim_start_matches('/');
    
    if store.exists(sha256) {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub async fn create_test_server() -> (String, SharedBlobStore) {
    let store = Arc::new(Mutex::new(BlobStore::new()));
    
    let app = Router::new()
        .route("/upload", put(upload_blob))
        .route("/:path", get(get_blob_handler))
        .route("/:path", head(head_blob_handler))
        .with_state(store.clone());

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    (format!("http://127.0.0.1:{}", port), store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_lfs::{
        blossom::BlossomClient,
        chunking::{Chunker, Manifest},
        blossom::{AuthToken, ActionType},
    };
    use secp256k1::{SecretKey, Secp256k1};

    fn generate_test_key() -> [u8; 32] {
        let secp = Secp256k1::new();
        let mut rng = secp256k1::rand::thread_rng();
        let secret_key = SecretKey::new(&mut rng);
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&secret_key.secret_bytes());
        key_bytes
    }

    #[tokio::test]
    async fn test_full_upload_download_cycle() {
        let (server_url, _store) = create_test_server().await;
        let client = BlossomClient::new(server_url).unwrap();
        let secret_key = generate_test_key();

        // Upload a blob
        let data = b"hello world from blossom".to_vec();
        let hash = format!("{:x}", Sha256::digest(&data));
        
        let auth_token = AuthToken::new(
            &secret_key,
            ActionType::Upload,
            None,
            Some(vec![&hash]),
            3600,
        ).unwrap();

        let result = client.upload_blob(
            data.clone(),
            &hash,
            None,
            Some(&auth_token),
        ).await;

        assert!(result.is_ok(), "Upload should succeed");
        let descriptor = result.unwrap();
        assert_eq!(descriptor.sha256, hash);

        // Download the blob
        let auth_token = AuthToken::new(
            &secret_key,
            ActionType::Get,
            None,
            None,
            3600,
        ).unwrap();

        let downloaded = client.download_blob(&hash, Some(&auth_token)).await;
        assert!(downloaded.is_ok(), "Download should succeed");
        assert_eq!(downloaded.unwrap(), data);

        // Check blob exists
        let exists = client.has_blob(&hash, Some(&auth_token)).await;
        assert!(exists.is_ok(), "Has blob should succeed");
        assert!(exists.unwrap(), "Blob should exist");
    }

    #[tokio::test]
    async fn test_chunked_file_workflow() {
        let (server_url, _store) = create_test_server().await;
        let client = BlossomClient::new(server_url).unwrap();
        let secret_key = generate_test_key();
        
        use tempfile::NamedTempFile;
        use std::io::Write;

        // Create a file larger than chunk size
        let mut file = NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..2048).map(|i| (i % 256) as u8).collect();
        file.write_all(&data).unwrap();
        file.flush().unwrap();

        let chunker = Chunker::new(512).unwrap();
        let (chunks, file_size) = chunker.chunk_file(file.path()).await.unwrap();

        assert_eq!(file_size, 2048);
        assert_eq!(chunks.len(), 4);

        // Upload each chunk
        for chunk in &chunks {
            let chunk_data = chunker.read_chunk(file.path(), chunk.offset, chunk.size).await.unwrap();

            let auth_token = AuthToken::new(
                &secret_key,
                ActionType::Upload,
                None,
                Some(vec![&chunk.hash]),
                3600,
            ).unwrap();

            let result = client.upload_blob(
                chunk_data,
                &chunk.hash,
                None,
                Some(&auth_token),
            ).await;

            assert!(result.is_ok(), "Chunk upload should succeed");
        }

        // Create and upload manifest
        let hashes: Vec<String> = chunks.iter().map(|c| c.hash.clone()).collect();
        let manifest = Manifest::new(
            2048,
            512,
            hashes,
            Some("test_file.bin".to_string()),
            None,
            None,
        ).unwrap();

        let manifest_json = manifest.to_json().unwrap();
        let manifest_data = manifest_json.into_bytes();
        let manifest_hash = format!("{:x}", sha2::Sha256::digest(&manifest_data));

        let auth_token = AuthToken::new(
            &secret_key,
            ActionType::Upload,
            None,
            Some(vec![&manifest_hash]),
            3600,
        ).unwrap();

        let result = client.upload_blob(
            manifest_data,
            &manifest_hash,
            Some("application/json"),
            Some(&auth_token),
        ).await;

        assert!(result.is_ok(), "Manifest upload should succeed");

        // Download and verify manifest
        let auth_token = AuthToken::new(
            &secret_key,
            ActionType::Get,
            None,
            None,
            3600,
        ).unwrap();

        let downloaded_manifest = client.download_blob(&manifest_hash, Some(&auth_token)).await;
        assert!(downloaded_manifest.is_ok(), "Manifest download should succeed");

        let parsed_manifest = Manifest::from_json(
            &String::from_utf8_lossy(&downloaded_manifest.unwrap())
        ).unwrap();

        assert_eq!(parsed_manifest.file_size, 2048);
        assert_eq!(parsed_manifest.chunks, 4);
        assert!(parsed_manifest.verify().unwrap(), "Manifest should verify");
    }

    #[tokio::test]
    async fn test_auth_token_workflow() {
        let secret_key = generate_test_key();
        
        // Create token for upload
        let upload_token = AuthToken::new(
            &secret_key,
            ActionType::Upload,
            Some("localhost"),
            Some(vec!["abc123"]),
            3600,
        ).unwrap();

        let header = upload_token.to_authorization_header().unwrap();
        assert!(header.starts_with("Nostr "), "Should have Nostr prefix");

        // Create token for download
        let get_token = AuthToken::new(
            &secret_key,
            ActionType::Get,
            Some("localhost"),
            None,
            3600,
        ).unwrap();

        // Tokens should have different 't' tags
        let upload_t_tags: Vec<_> = upload_token.event.tags.iter()
            .filter(|t| t[0] == "t")
            .collect();
        let get_t_tags: Vec<_> = get_token.event.tags.iter()
            .filter(|t| t[0] == "t")
            .collect();

        assert!(upload_t_tags.iter().any(|t| t[1] == "upload"));
        assert!(get_t_tags.iter().any(|t| t[1] == "get"));
    }
}