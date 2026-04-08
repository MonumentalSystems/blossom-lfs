// End-to-end test with mock Blossom server

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, head, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobDescriptor {
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub url: String,
    pub uploaded: u64,
}

#[derive(Debug, Default)]
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
        if expected_hash
            .to_str()
            .map(|h| h != descriptor.sha256)
            .unwrap_or(true)
        {
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

    store.get(sha256).cloned().ok_or(StatusCode::NOT_FOUND)
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
        .route("/{path}", get(get_blob_handler))
        .route("/{path}", head(head_blob_handler))
        .with_state(store.clone());

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    (format!("http://127.0.0.1:{}", port), store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_lfs::chunking::{Chunker, Manifest};
    use blossom_rs::{auth::Signer, BlossomClient};

    fn create_test_client(server_url: String) -> BlossomClient {
        let signer = Signer::generate();
        BlossomClient::new(vec![server_url], signer)
    }

    #[tokio::test]
    async fn test_full_upload_download_cycle() {
        let (server_url, _store) = create_test_server().await;
        let client = create_test_client(server_url);

        // Upload a blob
        let data = b"hello world from blossom";

        let result = client.upload(data, "application/octet-stream").await;
        assert!(result.is_ok(), "Upload should succeed: {:?}", result.err());

        let descriptor = result.unwrap();
        let hash = descriptor.sha256.clone();

        // Download the blob
        let downloaded: Vec<u8> = client.download(&hash).await.unwrap();
        assert_eq!(downloaded, data.to_vec());

        // Check blob exists
        let exists = client.exists(&hash).await.unwrap();
        assert!(exists, "Blob should exist");
    }

    #[tokio::test]
    async fn test_chunked_file_workflow() {
        let (server_url, _store) = create_test_server().await;
        let client = create_test_client(server_url);

        use std::io::Write;
        use tempfile::NamedTempFile;

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
            let chunk_data = chunker
                .read_chunk(file.path(), chunk.offset, chunk.size)
                .await
                .unwrap();

            let result = client.upload(&chunk_data, "application/octet-stream").await;
            assert!(
                result.is_ok(),
                "Chunk upload should succeed: {:?}",
                result.err()
            );
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
        )
        .unwrap();

        let manifest_json = manifest.to_json().unwrap();
        let manifest_data = manifest_json.as_bytes();

        let result = client.upload(manifest_data, "application/json").await;
        assert!(
            result.is_ok(),
            "Manifest upload should succeed: {:?}",
            result.err()
        );

        let manifest_hash = result.unwrap().sha256;

        // Download and verify manifest
        let downloaded_manifest: Vec<u8> = client.download(&manifest_hash).await.unwrap();

        let parsed_manifest =
            Manifest::from_json(&String::from_utf8_lossy(&downloaded_manifest)).unwrap();

        assert_eq!(parsed_manifest.file_size, 2048);
        assert_eq!(parsed_manifest.chunks, 4);
        assert!(parsed_manifest.verify().unwrap(), "Manifest should verify");
    }

    #[tokio::test]
    async fn test_auth_is_handled_by_client() {
        let (server_url, _store) = create_test_server().await;
        let client = create_test_client(server_url);

        // Auth is handled internally by the client — just verify operations succeed
        let data = b"auth test data";
        let result = client.upload(data, "application/octet-stream").await;
        assert!(result.is_ok(), "Upload with internal auth should succeed");
    }
}
