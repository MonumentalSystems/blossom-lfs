//! Full-stack integration tests for BUD-20 (LFS-aware storage efficiency).
//!
//! These tests start a real blossom-rs BlobServer (with compression + delta
//! enabled) alongside the blossom-lfs daemon, then exercise the complete
//! round-trip: daemon → real server (compresses) → daemon (downloads).

use base64::Engine;
use blossom_rs::auth::Signer;
use blossom_rs::server::BlobServer;
use blossom_rs::storage::MemoryBackend;
use blossom_rs::{MemoryDatabase, MemoryLfsVersionDatabase};
use sha2::{Digest, Sha256};

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn repo_b64(repo_path: &std::path::Path) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(repo_path.to_string_lossy().as_bytes())
}

fn setup_git_repo(server_url: &str, nsec_hex: &str) -> tempfile::TempDir {
    setup_git_repo_with_chunk_size(server_url, nsec_hex, None)
}

fn setup_git_repo_with_chunk_size(
    server_url: &str,
    nsec_hex: &str,
    chunk_size: Option<usize>,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo_path)
        .output()
        .expect("git init failed");

    std::process::Command::new("git")
        .args(["remote", "add", "origin", "https://myrepo"])
        .current_dir(repo_path)
        .output()
        .expect("git remote add failed");

    let mut config = format!("server={}\nprivate-key={}", server_url, nsec_hex);
    if let Some(cs) = chunk_size {
        config.push_str(&format!("\nchunk-size={}", cs));
    }
    std::fs::write(repo_path.join(".lfsdalconfig"), config).unwrap();

    dir
}

async fn spawn_blossom_server() -> String {
    let server = BlobServer::builder(MemoryBackend::new(), "http://localhost:3000")
        .database(MemoryDatabase::new())
        .require_auth()
        .lfs_version_database(MemoryLfsVersionDatabase::new())
        .build();

    let app = server.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    url
}

async fn spawn_lfs_daemon(port: u16) {
    tokio::spawn(blossom_lfs::daemon::run_daemon(port));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

async fn find_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// Upload data through the daemon and download it back, verifying the
/// round-trip is byte-for-byte identical even though the blossom-rs server
/// compressed the blob internally.
#[tokio::test(flavor = "multi_thread")]
async fn test_bud20_compressed_roundtrip() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
    let oid = sha256_hex(&data);

    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload should succeed");

    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "download should succeed");
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(
        downloaded.len(),
        data.len(),
        "downloaded size should match original"
    );
    assert_eq!(
        &downloaded[..],
        &data[..],
        "downloaded content should match original (server transparently decompressed)"
    );
}

/// Full LFS workflow through the daemon against a real blossom-rs server:
/// batch → upload → verify → batch download → GET download.
#[tokio::test(flavor = "multi_thread")]
async fn test_bud20_full_lfs_workflow() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data: Vec<u8> = (0..8_000).map(|i| (i % 128) as u8).collect();
    let oid = sha256_hex(&data);

    // 1. Batch upload
    let resp = http
        .post(format!("{}/lfs/{}/objects/batch", daemon_url, repo_b64))
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid, "size": data.len()}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let batch: serde_json::Value = resp.json().await.unwrap();
    let upload_url = batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();
    let verify_url = batch["objects"][0]["actions"]["verify"]["href"]
        .as_str()
        .unwrap()
        .to_string();

    // 2. Upload via PUT
    let resp = http
        .put(&upload_url)
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 3. Verify
    let resp = http.post(&verify_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // 4. Batch download
    let resp = http
        .post(format!("{}/lfs/{}/objects/batch", daemon_url, repo_b64))
        .json(&serde_json::json!({
            "operation": "download",
            "objects": [{"oid": oid, "size": data.len()}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let download_url = resp.json::<serde_json::Value>().await.unwrap()["objects"][0]["actions"]
        ["download"]["href"]
        .as_str()
        .unwrap()
        .to_string();

    // 5. Download via GET
    let resp = http.get(&download_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(&downloaded[..], &data[..]);
}

/// Upload the same blob twice. The second upload should succeed (dedup) and
/// the download should still return the original content.
#[tokio::test(flavor = "multi_thread")]
async fn test_bud20_dedup_then_download() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data: Vec<u8> = vec![0xAB; 5_000];
    let oid = sha256_hex(&data);

    // First upload
    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Second upload (dedup — server already has it)
    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Download should still return original data
    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(&downloaded[..], &data[..]);
}

/// Upload multiple distinct blobs through the daemon and verify each one
/// downloads correctly. This catches issues where the LFS version DB might
/// confuse different blobs.
#[tokio::test(flavor = "multi_thread")]
async fn test_bud20_multiple_blobs() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let blobs: Vec<Vec<u8>> = vec![vec![0x11; 6_000], vec![0x22; 8_000], vec![0x33; 4_000]];

    let mut oids = Vec::new();
    for data in &blobs {
        let oid = sha256_hex(data);
        oids.push(oid.clone());

        let resp = http
            .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
            .body(data.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    for (i, oid) in oids.iter().enumerate() {
        let resp = http
            .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let downloaded = resp.bytes().await.unwrap();
        assert_eq!(
            &downloaded[..],
            &blobs[i][..],
            "blob {} content mismatch",
            i
        );
    }
}

/// Upload a file large enough to trigger chunking (chunk-size=1024, file > 1024
/// bytes) through the daemon against a real blossom-rs server with BUD-20
/// compression enabled, then download and verify the reassembled content.
#[tokio::test(flavor = "multi_thread")]
async fn test_bud20_chunked_upload_download() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir =
        setup_git_repo_with_chunk_size(&blossom_url, &signer.secret_key_hex(), Some(1024));
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // 4096 bytes with chunk-size=1024 → 4 chunks + manifest + full file upload_lfs
    let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
    let oid = sha256_hex(&data);

    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "chunked upload should succeed");

    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "chunked download should succeed");
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(
        downloaded.len(),
        data.len(),
        "downloaded size should match original"
    );
    assert_eq!(
        &downloaded[..],
        &data[..],
        "downloaded content should match original after chunked reassembly + server decompression"
    );
}
