use blossom_lfs::chunking::Manifest;
use blossom_rs::{auth::Signer, BlossomClient};
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn create_test_client(server_url: String) -> BlossomClient {
    let signer = Signer::generate();
    BlossomClient::new(vec![server_url], signer)
}

#[tokio::test]
async fn test_blossom_client_upload() {
    let mock_server = MockServer::start().await;
    let client = create_test_client(mock_server.uri());

    let data = b"hello world";
    let hash = format!("{:x}", Sha256::digest(data));

    Mock::given(method("PUT"))
        .and(path("/upload"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha256": hash,
            "size": data.len(),
            "url": format!("{}/{}", mock_server.uri(), hash),
            "uploaded": 1234567890
        })))
        .mount(&mock_server)
        .await;

    let result = client.upload(data, "application/octet-stream").await;
    assert!(result.is_ok(), "Upload should succeed: {:?}", result.err());

    let descriptor = result.unwrap();
    assert_eq!(descriptor.sha256, hash);
    assert_eq!(descriptor.size, data.len() as u64);
}

#[tokio::test]
async fn test_blossom_client_download() {
    let mock_server = MockServer::start().await;
    let client = create_test_client(mock_server.uri());

    let test_data = b"test blob content";
    let hash = format!("{:x}", Sha256::digest(test_data));

    Mock::given(method("GET"))
        .and(path(format!("/{}", hash)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(test_data.to_vec()))
        .mount(&mock_server)
        .await;

    let result = client.download(&hash).await;
    assert!(
        result.is_ok(),
        "Download should succeed: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap(), test_data.to_vec());
}

#[tokio::test]
async fn test_blossom_client_exists() {
    let mock_server = MockServer::start().await;
    let client = create_test_client(mock_server.uri());

    Mock::given(method("HEAD"))
        .and(path("/exists_hash"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    Mock::given(method("HEAD"))
        .and(path("/missing_hash"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let exists = client.exists("exists_hash").await.unwrap();
    assert!(exists, "Should find existing blob");

    let not_exists = client.exists("missing_hash").await.unwrap();
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

    let json = manifest.to_json().unwrap();
    let parsed = Manifest::from_json(&json).unwrap();
    assert_eq!(parsed.merkle_root, manifest.merkle_root);
}
