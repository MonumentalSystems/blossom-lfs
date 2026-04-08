//! Integration tests for chunked uploads with streaming and manifest verification.
//!
//! Uses a real blossom-rs `BlobServer` with `MemoryBackend` — no mocks.
//! Chunk size is set to 4 KB to exercise chunking on small test data.

use blossom_lfs::chunking::{Chunker, Manifest};
use blossom_rs::{auth::Signer, server::BlobServer, storage::MemoryBackend, BlossomClient};
use sha2::{Digest, Sha256};
use std::io::Write;
use tempfile::NamedTempFile;

const CHUNK_SIZE: usize = 4096; // 4 KB

/// Spin up a real blossom-rs server backed by in-memory storage.
async fn spawn_test_server() -> String {
    let server = BlobServer::new(MemoryBackend::new(), "http://localhost:0");
    let app = server.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    url
}

fn test_client(server_url: &str) -> BlossomClient {
    let signer = Signer::generate();
    BlossomClient::new(vec![server_url.to_string()], signer)
}

/// Create a temp file with deterministic data of the given size.
fn create_test_file(size: usize) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    file.write_all(&data).unwrap();
    file.flush().unwrap();
    file
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_chunked_upload_with_4k_chunks() {
    let url = spawn_test_server().await;
    let client = test_client(&url);

    // 10 KB file → 3 chunks at 4 KB (4K + 4K + 2K)
    let file = create_test_file(10_000);
    let chunker = Chunker::new(CHUNK_SIZE).unwrap();

    let (chunks, file_size) = chunker.chunk_file(file.path()).await.unwrap();
    assert_eq!(file_size, 10_000);
    assert_eq!(chunks.len(), 3, "10KB / 4KB = 3 chunks");
    assert_eq!(chunks[0].size, 4096);
    assert_eq!(chunks[1].size, 4096);
    assert_eq!(chunks[2].size, 10_000 - 2 * 4096);

    // Upload each chunk
    for chunk in &chunks {
        let data = chunker
            .read_chunk(file.path(), chunk.offset, chunk.size)
            .await
            .unwrap();
        client
            .upload(&data, "application/octet-stream")
            .await
            .expect("chunk upload failed");
    }

    // Verify each chunk exists on server
    for chunk in &chunks {
        assert!(
            client.exists(&chunk.hash).await.unwrap(),
            "chunk {} should exist",
            chunk.index
        );
    }
}

#[tokio::test]
async fn test_manifest_upload_and_download() {
    let url = spawn_test_server().await;
    let client = test_client(&url);

    let file = create_test_file(10_000);
    let chunker = Chunker::new(CHUNK_SIZE).unwrap();

    let (chunks, file_size) = chunker.chunk_file(file.path()).await.unwrap();

    // Upload chunks
    let mut chunk_hashes = Vec::new();
    for chunk in &chunks {
        let data = chunker
            .read_chunk(file.path(), chunk.offset, chunk.size)
            .await
            .unwrap();
        client
            .upload(&data, "application/octet-stream")
            .await
            .unwrap();
        chunk_hashes.push(chunk.hash.clone());
    }

    // Build and upload manifest
    let manifest = Manifest::new(
        file_size,
        CHUNK_SIZE,
        chunk_hashes.clone(),
        Some("test.bin".to_string()),
        Some("application/octet-stream".to_string()),
        None,
    )
    .unwrap();

    assert!(manifest.verify().unwrap());

    let manifest_json = manifest.to_json().unwrap();
    let manifest_desc = client
        .upload(manifest_json.as_bytes(), "application/json")
        .await
        .expect("manifest upload failed");

    // Download manifest and verify
    let downloaded: Vec<u8> = client.download(&manifest_desc.sha256).await.unwrap();
    let parsed = Manifest::from_json(&String::from_utf8_lossy(&downloaded)).unwrap();

    assert_eq!(parsed.file_size, 10_000);
    assert_eq!(parsed.chunk_size, CHUNK_SIZE);
    assert_eq!(parsed.chunks, 3);
    assert_eq!(parsed.chunk_hashes, chunk_hashes);
    assert!(parsed.verify().unwrap(), "merkle root should verify");
}

#[tokio::test]
async fn test_full_roundtrip_chunked_upload_download_reassemble() {
    let url = spawn_test_server().await;
    let client = test_client(&url);

    // Create file with known content
    let original_data: Vec<u8> = (0..20_000).map(|i| (i % 251) as u8).collect();
    let file = create_test_file(20_000);

    let chunker = Chunker::new(CHUNK_SIZE).unwrap();
    let (chunks, file_size) = chunker.chunk_file(file.path()).await.unwrap();
    assert_eq!(file_size, 20_000);
    assert_eq!(chunks.len(), 5, "20KB / 4KB = 5 chunks");

    // Upload chunks
    let mut chunk_hashes = Vec::new();
    for chunk in &chunks {
        let data = chunker
            .read_chunk(file.path(), chunk.offset, chunk.size)
            .await
            .unwrap();
        client
            .upload(&data, "application/octet-stream")
            .await
            .unwrap();
        chunk_hashes.push(chunk.hash.clone());
    }

    // Upload manifest
    let manifest = Manifest::new(
        file_size,
        CHUNK_SIZE,
        chunk_hashes,
        Some("roundtrip.bin".to_string()),
        None,
        None,
    )
    .unwrap();
    let manifest_json = manifest.to_json().unwrap();
    let manifest_desc = client
        .upload(manifest_json.as_bytes(), "application/json")
        .await
        .unwrap();

    // --- Download side: simulate what the agent does ---

    // Download manifest
    let manifest_blob: Vec<u8> = client.download(&manifest_desc.sha256).await.unwrap();
    let dl_manifest = Manifest::from_json(&String::from_utf8_lossy(&manifest_blob)).unwrap();
    assert!(dl_manifest.verify().unwrap());

    // Download and reassemble chunks
    let mut reassembled = Vec::new();
    for chunk_info in dl_manifest.all_chunk_info().unwrap() {
        let chunk_data: Vec<u8> = client.download(&chunk_info.hash).await.unwrap();
        assert_eq!(
            chunk_data.len(),
            chunk_info.size,
            "chunk {} size mismatch",
            chunk_info.index
        );

        // Verify chunk hash
        let actual_hash = format!("{:x}", Sha256::digest(&chunk_data));
        assert_eq!(
            actual_hash, chunk_info.hash,
            "chunk {} hash mismatch",
            chunk_info.index
        );

        reassembled.extend_from_slice(&chunk_data);
    }

    assert_eq!(reassembled.len(), original_data.len());
    assert_eq!(
        reassembled, original_data,
        "reassembled data must match original"
    );
}

#[tokio::test]
async fn test_dedup_skips_existing_chunks() {
    let url = spawn_test_server().await;
    let client = test_client(&url);

    let file = create_test_file(8192); // exactly 2 chunks
    let chunker = Chunker::new(CHUNK_SIZE).unwrap();
    let (chunks, _) = chunker.chunk_file(file.path()).await.unwrap();
    assert_eq!(chunks.len(), 2);

    // Upload first chunk
    let data0 = chunker
        .read_chunk(file.path(), chunks[0].offset, chunks[0].size)
        .await
        .unwrap();
    client
        .upload(&data0, "application/octet-stream")
        .await
        .unwrap();

    // Verify first chunk exists, second doesn't
    assert!(client.exists(&chunks[0].hash).await.unwrap());
    assert!(!client.exists(&chunks[1].hash).await.unwrap());

    // Upload second chunk
    let data1 = chunker
        .read_chunk(file.path(), chunks[1].offset, chunks[1].size)
        .await
        .unwrap();
    client
        .upload(&data1, "application/octet-stream")
        .await
        .unwrap();

    // Both exist now
    assert!(client.exists(&chunks[0].hash).await.unwrap());
    assert!(client.exists(&chunks[1].hash).await.unwrap());

    // Re-uploading the same data is idempotent
    let desc = client
        .upload(&data0, "application/octet-stream")
        .await
        .unwrap();
    assert_eq!(desc.sha256, chunks[0].hash);
}

#[tokio::test]
async fn test_single_chunk_file_no_chunking_needed() {
    let url = spawn_test_server().await;
    let client = test_client(&url);

    // File smaller than chunk size — should not chunk
    let file = create_test_file(2000);
    let chunker = Chunker::new(CHUNK_SIZE).unwrap();

    assert!(!chunker.should_chunk(2000));

    // Upload as single blob
    let data = tokio::fs::read(file.path()).await.unwrap();
    let hash = format!("{:x}", Sha256::digest(&data));
    let desc = client
        .upload(&data, "application/octet-stream")
        .await
        .unwrap();
    assert_eq!(desc.sha256, hash);

    // Download and verify
    let downloaded: Vec<u8> = client.download(&hash).await.unwrap();
    assert_eq!(downloaded, data);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_streaming_upload_file() {
    let url = spawn_test_server().await;
    let client = test_client(&url);

    // Create a file and upload via upload_file (streaming, not buffered)
    let file = create_test_file(16_000);
    let original_data = tokio::fs::read(file.path()).await.unwrap();
    let expected_hash = format!("{:x}", Sha256::digest(&original_data));

    use blossom_rs::BlobClient;
    let desc = BlobClient::upload_file(&client, &(), file.path(), "application/octet-stream")
        .await
        .expect("streaming upload_file failed");

    assert_eq!(desc.sha256, expected_hash);
    assert_eq!(desc.size, 16_000);

    // Download and verify
    let downloaded: Vec<u8> = client.download(&expected_hash).await.unwrap();
    assert_eq!(downloaded, original_data);
}

#[tokio::test]
async fn test_various_chunk_sizes() {
    // Verify chunking works correctly at different chunk sizes
    let url = spawn_test_server().await;
    let client = test_client(&url);

    let file = create_test_file(10_000);
    let original_data = tokio::fs::read(file.path()).await.unwrap();

    for &cs in &[1024, 2048, 4096, 5000, 9999, 10_000] {
        let chunker = Chunker::new(cs).unwrap();
        let (chunks, size) = chunker.chunk_file(file.path()).await.unwrap();
        assert_eq!(size, 10_000);

        let expected_chunks = 10_000usize.div_ceil(cs);
        assert_eq!(
            chunks.len(),
            expected_chunks,
            "chunk_size={} should produce {} chunks",
            cs,
            expected_chunks
        );

        // Upload all chunks
        let mut hashes = Vec::new();
        for chunk in &chunks {
            let data = chunker
                .read_chunk(file.path(), chunk.offset, chunk.size)
                .await
                .unwrap();
            client
                .upload(&data, "application/octet-stream")
                .await
                .unwrap();
            hashes.push(chunk.hash.clone());
        }

        // Build manifest, upload, download, reassemble
        let manifest = Manifest::new(size, cs, hashes, None, None, None).unwrap();
        assert!(manifest.verify().unwrap());

        let mj = manifest.to_json().unwrap();
        let md = client
            .upload(mj.as_bytes(), "application/json")
            .await
            .unwrap();

        let dl: Vec<u8> = client.download(&md.sha256).await.unwrap();
        let parsed = Manifest::from_json(&String::from_utf8_lossy(&dl)).unwrap();
        assert!(parsed.verify().unwrap());

        let mut reassembled = Vec::new();
        for ci in parsed.all_chunk_info().unwrap() {
            let cd: Vec<u8> = client.download(&ci.hash).await.unwrap();
            reassembled.extend_from_slice(&cd);
        }
        assert_eq!(
            reassembled, original_data,
            "roundtrip failed at chunk_size={}",
            cs
        );
    }
}
