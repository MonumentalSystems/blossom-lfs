//! Iroh transport integration tests for blossom-lfs daemon.
//!
//! Tests iroh-only mode and dual-transport mode (iroh upload + HTTP download)
//! with shared backend storage.

#![cfg(feature = "iroh")]

use base64::Engine;
use blossom_rs::auth::Signer;
use blossom_rs::db::MemoryDatabase;
use blossom_rs::locks::MemoryLockDatabase;
use blossom_rs::protocol::BlobDescriptor;
use blossom_rs::server::BlobServer;
use blossom_rs::storage::{BlobBackend, MemoryBackend};
use blossom_rs::transport::{BlossomProtocol, BLOSSOM_ALPN};
use blossom_rs::MemoryLfsVersionDatabase;
use iroh::endpoint::presets::N0;
use iroh::protocol::Router;
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn repo_b64(repo_path: &std::path::Path) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(repo_path.to_string_lossy().as_bytes())
}

fn setup_git_repo_dual(
    server_url: &str,
    endpoint_id_str: &str,
    nsec_hex: &str,
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

    let config = format!(
        "server={}\niroh-endpoint={}\nprivate-key={}",
        server_url, endpoint_id_str, nsec_hex
    );
    std::fs::write(repo_path.join(".lfsdalconfig"), config).unwrap();

    dir
}

struct SharedBackend {
    inner: Arc<std::sync::Mutex<MemoryBackend>>,
}

impl SharedBackend {
    fn new(inner: Arc<std::sync::Mutex<MemoryBackend>>) -> Self {
        Self { inner }
    }
}

impl BlobBackend for SharedBackend {
    fn insert(&mut self, data: Vec<u8>, base_url: &str) -> BlobDescriptor {
        self.inner.lock().unwrap().insert(data, base_url)
    }

    fn insert_with_hash(
        &mut self,
        data: Vec<u8>,
        hash: &str,
        original_size: u64,
        base_url: &str,
    ) -> BlobDescriptor {
        self.inner
            .lock()
            .unwrap()
            .insert_with_hash(data, hash, original_size, base_url)
    }

    fn get(&self, sha256: &str) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().get(sha256)
    }

    fn exists(&self, sha256: &str) -> bool {
        self.inner.lock().unwrap().exists(sha256)
    }

    fn delete(&mut self, sha256: &str) -> bool {
        self.inner.lock().unwrap().delete(sha256)
    }

    fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    fn total_bytes(&self) -> u64 {
        self.inner.lock().unwrap().total_bytes()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_dual_transport_iroh_upload_http_download() {
    let shared_mem: Arc<std::sync::Mutex<MemoryBackend>> =
        Arc::new(std::sync::Mutex::new(MemoryBackend::new()));
    let http_server =
        BlobServer::builder(SharedBackend::new(shared_mem.clone()), "http://localhost:0")
            .database(MemoryDatabase::new())
            .build();
    let app = http_server.router();
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();
    let http_url = format!("http://{}", http_addr);
    tokio::spawn(async move { axum::serve(http_listener, app).await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let iroh_server =
        BlobServer::builder(SharedBackend::new(shared_mem.clone()), "iroh://test").build();
    let iroh_state = iroh_server.shared_state();
    let iroh_endpoint = iroh::Endpoint::builder(N0)
        .bind()
        .await
        .expect("bind iroh endpoint");
    let iroh_addr = iroh_endpoint.addr();
    let _iroh_router = Router::builder(iroh_endpoint)
        .accept(BLOSSOM_ALPN, Arc::new(BlossomProtocol::new(iroh_state)))
        .spawn();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let signer = Signer::generate();
    let endpoint_id_str = iroh_addr.id.to_string();
    let repo_dir = setup_git_repo_dual(&http_url, &endpoint_id_str, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data: Vec<u8> = (0..5000).map(|i| (i % 256) as u8).collect();
    let oid = sha256_hex(&data);

    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "dual-transport upload should succeed");

    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "dual-transport download should succeed");
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(
        &downloaded[..],
        &data[..],
        "dual-transport round-trip content mismatch"
    );
}

/// BUD-20 compression round-trip via iroh-only daemon. The daemon sends LFS
/// tags in the upload, the iroh server compresses, and on download the server
/// decompresses transparently. Verifies byte-for-byte identity.
#[tokio::test(flavor = "multi_thread")]
async fn test_iroh_bud20_compressed_roundtrip() {
    let (server_addr, _router) = spawn_iroh_lfs_server().await;
    let signer = Signer::generate();

    let endpoint_id_str = server_addr.id.to_string();
    let repo_dir = setup_git_repo_iroh_only(&endpoint_id_str, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // Highly compressible data — server will apply zstd
    let data: Vec<u8> = vec![42u8; 10_000];
    let oid = sha256_hex(&data);

    // Upload via daemon → iroh server (compresses internally)
    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "BUD-20 upload should succeed");

    // Download via daemon → iroh server (decompresses transparently)
    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "BUD-20 download should succeed");
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(downloaded.len(), data.len(), "size should match original");
    assert_eq!(
        &downloaded[..],
        &data[..],
        "BUD-20 round-trip content mismatch"
    );
}

fn setup_git_repo_iroh_only(endpoint_id_str: &str, nsec_hex: &str) -> tempfile::TempDir {
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

    let config = format!(
        "iroh-endpoint={}\ntransport=iroh\nprivate-key={}",
        endpoint_id_str, nsec_hex
    );
    std::fs::write(repo_path.join(".lfsdalconfig"), config).unwrap();

    dir
}

async fn spawn_iroh_lfs_server() -> (iroh::EndpointAddr, Router) {
    let server = BlobServer::builder(MemoryBackend::new(), "iroh://test")
        .lock_database(MemoryLockDatabase::new())
        .lfs_version_database(MemoryLfsVersionDatabase::new())
        .build();
    let state = server.shared_state();

    let endpoint = iroh::Endpoint::builder(N0)
        .bind()
        .await
        .expect("bind server endpoint");

    let addr = endpoint.addr();

    let router = Router::builder(endpoint)
        .accept(BLOSSOM_ALPN, Arc::new(BlossomProtocol::new(state)))
        .spawn();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    (addr, router)
}

async fn spawn_lfs_daemon(port: u16) {
    tokio::spawn(blossom_lfs::daemon::run_daemon(port));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

async fn find_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_iroh_upload_and_download() {
    let (server_addr, _router) = spawn_iroh_lfs_server().await;
    let signer = Signer::generate();

    let endpoint_id_str = server_addr.id.to_string();
    let repo_dir = setup_git_repo_iroh_only(&endpoint_id_str, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data: Vec<u8> = (0..5000).map(|i| (i % 256) as u8).collect();
    let oid = sha256_hex(&data);

    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.clone())
        .send()
        .await
        .unwrap();
    if resp.status() != 200 {
        let body = resp.text().await.unwrap();
        eprintln!("UPLOAD FAILED: status body = {}", body);
        panic!("iroh upload should succeed");
    }

    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "iroh download should succeed");
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(
        &downloaded[..],
        &data[..],
        "iroh round-trip content mismatch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_iroh_batch_upload_download() {
    let (server_addr, _router) = spawn_iroh_lfs_server().await;
    let signer = Signer::generate();

    let endpoint_id_str = server_addr.id.to_string();
    let repo_dir = setup_git_repo_iroh_only(&endpoint_id_str, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data: Vec<u8> = (0..8000).map(|i| (i % 128) as u8).collect();
    let oid = sha256_hex(&data);

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

    let resp = http
        .put(&upload_url)
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "batch upload via iroh should succeed");

    let resp = http.post(&verify_url).send().await.unwrap();
    assert_eq!(resp.status(), 200, "verify via iroh should succeed");

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

    let resp = http.get(&download_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(
        &downloaded[..],
        &data[..],
        "batch download via iroh mismatch"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_iroh_lock_lifecycle() {
    let (server_addr, _router) = spawn_iroh_lfs_server().await;
    let signer = Signer::generate();

    let endpoint_id_str = server_addr.id.to_string();
    let repo_dir = setup_git_repo_iroh_only(&endpoint_id_str, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .json(&serde_json::json!({"path": "big-file.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "lock create via iroh should succeed");
    let lock_resp: serde_json::Value = resp.json().await.unwrap();
    let lock_id = lock_resp["lock"]["id"].as_str().unwrap().to_string();
    assert!(!lock_id.is_empty());

    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let locks: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(locks["locks"].as_array().unwrap().len(), 1);

    let resp = http
        .post(format!("{}/lfs/{}/locks/verify", daemon_url, repo_b64))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let verify: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(verify["ours"].as_array().unwrap().len(), 1);
    assert_eq!(verify["theirs"].as_array().unwrap().len(), 0);

    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, repo_b64, lock_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "unlock via iroh should succeed");

    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let locks: serde_json::Value = resp.json().await.unwrap();
    assert!(
        locks["locks"].as_array().unwrap().is_empty(),
        "lock should be gone after unlock"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_iroh_multiple_blobs() {
    let (server_addr, _router) = spawn_iroh_lfs_server().await;
    let signer = Signer::generate();

    let endpoint_id_str = server_addr.id.to_string();
    let repo_dir = setup_git_repo_iroh_only(&endpoint_id_str, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let blobs: Vec<Vec<u8>> = vec![vec![0x11; 6000], vec![0x22; 8000], vec![0x33; 4000]];

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
        assert_eq!(resp.status(), 200, "upload blob via iroh should succeed");
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
            "blob {} content mismatch via iroh",
            i
        );
    }
}

/// User A creates a lock. User B cannot unlock (403), but can force-unlock.
#[tokio::test(flavor = "multi_thread")]
async fn test_iroh_lock_force_unlock_by_other_user() {
    let (server_addr, _router) = spawn_iroh_lfs_server().await;
    let signer_a = Signer::generate();
    let signer_b = Signer::generate();

    let endpoint_id_str = server_addr.id.to_string();

    // User A's repo
    let repo_dir_a = setup_git_repo_iroh_only(&endpoint_id_str, &signer_a.secret_key_hex());
    let repo_b64_a = repo_b64(repo_dir_a.path());

    // User B's repo (same iroh endpoint, different signer)
    let repo_dir_b = setup_git_repo_iroh_only(&endpoint_id_str, &signer_b.secret_key_hex());
    let repo_b64_b = repo_b64(repo_dir_b.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // User A creates a lock
    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64_a))
        .json(&serde_json::json!({"path": "protected.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "user A lock create should succeed");
    let lock_resp: serde_json::Value = resp.json().await.unwrap();
    let lock_id = lock_resp["lock"]["id"].as_str().unwrap().to_string();

    // User B tries to unlock without force — should fail
    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, repo_b64_b, lock_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "user B unlock without force should be forbidden"
    );

    // User B force-unlocks — should succeed
    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, repo_b64_b, lock_id
        ))
        .json(&serde_json::json!({"force": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "user B force unlock should succeed");

    // Verify lock is gone
    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64_a))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let locks: serde_json::Value = resp.json().await.unwrap();
    assert!(
        locks["locks"].as_array().unwrap().is_empty(),
        "lock should be gone after force unlock"
    );
}
