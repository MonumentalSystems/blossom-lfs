// Test utilities and mock Blossom server

mod mock_server {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path, header};
    use serde_json::json;

    /// In-memory blob store for testing
    #[derive(Debug, Default)]
    pub struct BlobStore {
        blobs: HashMap<String, Vec<u8>>,
        descriptors: HashMap<String, MockBlobDescriptor>,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct MockBlobDescriptor {
        pub sha256: String,
        pub size: u64,
        #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
        pub content_type: Option<String>,
        pub url: String,
        pub uploaded: u64,
    }

    impl BlobStore {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn insert(&mut self, data: Vec<u8>) -> MockBlobDescriptor {
            use sha2::{Digest, Sha256};
            let hash = format!("{:x}", Sha256::digest(&data));
            let size = data.len() as u64;
            let url = format!("/{}", hash);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let descriptor = MockBlobDescriptor {
                sha256: hash.clone(),
                size,
                content_type: Some("application/octet-stream".into()),
                url: format!("http://localhost:{}", hash),
                uploaded: ts,
            };

            self.blobs.insert(hash.clone(), data);
            self.descriptors.insert(hash, descriptor.clone());
            
            descriptor
        }

        pub fn get(&self, sha256: &str) -> Option<&Vec<u8>> {
            self.blobs.get(sha256)
        }

        pub fn exists(&self, sha256: &str) -> bool {
            self.blobs.contains_key(sha256)
        }
    }

    /// Mock Blossom server for testing
    pub struct MockBlossomServer {
        pub server: MockServer,
        pub store: Arc<Mutex<BlobStore>>,
    }

    impl MockBlossomServer {
        pub async fn start() -> Self {
            let server = MockServer::start().await;
            let store = Arc::new(Mutex::new(BlobStore::new()));
            
            Self { server, store }
        }

        pub fn url(&self) -> String {
            self.server.uri()
        }

        pub async fn setup_upload_endpoint(&self) {
            let store = Arc::clone(&self.store);
            
            Mock::given(method("PUT"))
                .and(path("/upload"))
                .respond_with(move |request: &wiremock::Request| {
                    let store = store.clone();
                    let body = request.body.clone();
                    
                    // In a real implementation, we'd handle this async
                    // For now, return a simple response
                    ResponseTemplate::new(200)
                        .set_body_json(json!({
                            "sha256": "abc123",
                            "size": body.len() as u64,
                            "url": "http://localhost/abc123",
                            "uploaded": 1234567890
                        }))
                })
                .mount(&self.server)
                .await;
        }

        pub async fn setup_get_endpoint(&self, sha256: &str, data: Vec<u8>) {
            let data_clone = data.clone();
            Mock::given(method("GET"))
                .and(path(format!("/{}", sha256)))
                .respond_with(move |_request: &wiremock::Request| {
                    ResponseTemplate::new(200)
                        .set_body_bytes(data_clone.clone())
                })
                .mount(&self.server)
                .await;
        }

        pub async fn setup_head_endpoint(&self, sha256: &str, exists: bool) {
            let status = if exists {
                wiremock::http::StatusCode::OK
            } else {
                wiremock::http::StatusCode::NOT_FOUND
            };

            Mock::given(method("HEAD"))
                .and(path(format!("/{}", sha256)))
                .respond_with(ResponseTemplate::new(status))
                .mount(&self.server)
                .await;
        }
    }
}

pub use mock_server::{MockBlossomServer, BlobStore, MockBlobDescriptor};