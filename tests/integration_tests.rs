use blossom_lfs::{blossom::BlossomClient, chunking::Manifest, config::Config};
use secp256k1::SecretKey;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn generate_test_config() -> Config {
    let mut rng = secp256k1::rand::thread_rng();
    let secret_key = SecretKey::new(&mut rng);
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&secret_key.secret_bytes());

    Config {
        server_url: "http://localhost:8080".to_string(),
        secret_key: key_bytes,
        chunk_size: 1024 * 1024, // 1MB for tests
        max_concurrent_uploads: 4,
        max_concurrent_downloads: 4,
        auth_expiration: 3600,
    }
}

#[tokio::test]
async fn test_blossom_client_upload() {
    let mock_server = MockServer::start().await;
    let config = Config {
        server_url: mock_server.uri(),
        ..generate_test_config()
    };

    let client = BlossomClient::new(config.server_url).unwrap();

    // Mock upload endpoint
    Mock::given(method("PUT"))
        .and(path("/upload"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha256": "abc123",
            "size": 11,
            "url": "http://localhost:8080/abc123",
            "uploaded": 1234567890
        })))
        .mount(&mock_server)
        .await;

    let data = b"hello world".to_vec();
    let sha256 = "abc123";

    let result = client.upload_blob(data, sha256, None, None).await;

    assert!(result.is_ok(), "Upload should succeed");
    let descriptor = result.unwrap();
    assert_eq!(descriptor.sha256, "abc123");
    assert_eq!(descriptor.size, 11);
}

#[tokio::test]
async fn test_blossom_client_download() {
    let mock_server = MockServer::start().await;
    let config = Config {
        server_url: mock_server.uri(),
        ..generate_test_config()
    };

    let client = BlossomClient::new(config.server_url).unwrap();

    let test_data = b"test blob content".to_vec();

    // Mock download endpoint
    Mock::given(method("GET"))
        .and(path("/testhash123"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(test_data.clone()))
        .mount(&mock_server)
        .await;

    let result = client.download_blob("testhash123", None).await;

    assert!(result.is_ok(), "Download should succeed");
    let downloaded = result.unwrap();
    assert_eq!(downloaded, test_data);
}

#[tokio::test]
async fn test_blossom_client_has_blob() {
    let mock_server = MockServer::start().await;
    let config = Config {
        server_url: mock_server.uri(),
        ..generate_test_config()
    };

    let client = BlossomClient::new(config.server_url).unwrap();

    // Mock HEAD endpoint - blob exists
    Mock::given(method("HEAD"))
        .and(path("/exists"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    // Mock HEAD endpoint - blob doesn't exist
    Mock::given(method("HEAD"))
        .and(path("/notexists"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let exists = client.has_blob("exists", None).await.unwrap();
    assert!(exists, "Should find existing blob");

    let not_exists = client.has_blob("notexists", None).await.unwrap();
    assert!(!not_exists, "Should not find non-existent blob");
}

#[test]
fn test_chunker_integration() {
    use blossom_lfs::chunking::Chunker;
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut file = NamedTempFile::new().unwrap();
    let data: Vec<u8> = (0..2048).map(|i| (i % 256) as u8).collect();
    file.write_all(&data).unwrap();
    file.flush().unwrap();

    let chunker = Chunker::new(512).unwrap();
    let (chunks, size) = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async { chunker.chunk_file(file.path()).await.unwrap() });

    assert_eq!(size, 2048);
    assert_eq!(chunks.len(), 4, "Should have 4 chunks");

    // All chunks except possibly the last should be chunk_size
    for chunk in &chunks[..chunks.len() - 1] {
        assert_eq!(chunk.size, 512);
    }
}

#[test]
fn test_manifest_integration() {
    let hashes = vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)];

    let manifest = Manifest::new(
        2048,
        512,
        hashes.clone(),
        Some("integration_test.bin".to_string()),
        Some("application/octet-stream".to_string()),
        Some("https://test.server.com".to_string()),
    )
    .unwrap();

    assert_eq!(manifest.version, "1.0");
    assert_eq!(manifest.file_size, 2048);
    assert_eq!(manifest.chunks, 3);
    assert!(manifest.verify().unwrap());

    // Test serialization roundtrip
    let json = manifest.to_json().unwrap();
    let parsed = Manifest::from_json(&json).unwrap();
    assert_eq!(parsed.merkle_root, manifest.merkle_root);
}
