//! Concurrent operation integration tests.
//!
//! Tests concurrent uploads and lock attempts under parallel load through the
//! real blossom-rs server + blossom-lfs daemon.

use base64::Engine;
use blossom_rs::access::RoleBasedAccess;
use blossom_rs::auth::Signer;
use blossom_rs::server::BlobServer;
use blossom_rs::storage::MemoryBackend;
use blossom_rs::{BlossomSigner, MemoryDatabase, MemoryLfsVersionDatabase, MemoryLockDatabase};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn repo_b64(repo_path: &std::path::Path) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(repo_path.to_string_lossy().as_bytes())
}

fn setup_git_repo(server_url: &str, nsec_hex: &str) -> tempfile::TempDir {
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

    std::fs::write(
        repo_path.join(".lfsdalconfig"),
        format!("server={}\nprivate-key={}", server_url, nsec_hex),
    )
    .unwrap();

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

async fn spawn_blossom_server_with_locks(signer: &Signer) -> String {
    let mut admins = HashSet::new();
    admins.insert(signer.public_key_hex());
    let mut members = HashSet::new();
    members.insert(signer.public_key_hex());

    let server = BlobServer::builder(MemoryBackend::new(), "http://localhost:3000")
        .database(MemoryDatabase::new())
        .access_control(RoleBasedAccess::new(admins, members))
        .require_auth()
        .lock_database(MemoryLockDatabase::new())
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

/// 8 concurrent uploads of the same blob. At least one should succeed (200)
/// and the rest should return either 200 or 500 (temp file contention on the
/// same OID path). Then download once and verify content matches.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_uploads_same_blob() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let rb64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data: Vec<u8> = (0..5_000).map(|i| (i % 256) as u8).collect();
    let oid = sha256_hex(&data);

    let mut handles = Vec::new();
    for _ in 0..8 {
        let http = http.clone();
        let url = format!("{}/lfs/{}/objects/{}", daemon_url, rb64, oid);
        let body = data.clone();
        handles.push(tokio::spawn(async move {
            http.put(&url).body(body).send().await.unwrap().status()
        }));
    }

    let mut ok_count = 0u32;
    for handle in handles {
        let status = handle.await.unwrap();
        assert!(
            status == 200 || status == 500,
            "concurrent upload should return 200 or 500, got {}",
            status
        );
        if status == 200 {
            ok_count += 1;
        }
    }
    assert!(ok_count >= 1, "at least one upload should succeed");

    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, rb64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(&downloaded[..], &data[..]);
}

/// 8 concurrent uploads of DIFFERENT blobs. Download each and verify content.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_uploads_different_blobs() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let rb64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let blobs: Vec<Vec<u8>> = (0..8)
        .map(|i| (0..1_000).map(|j| ((i * 37 + j) % 256) as u8).collect())
        .collect();

    let mut handles = Vec::new();
    for data in &blobs {
        let http = http.clone();
        let url_base = daemon_url.clone();
        let rb = rb64.clone();
        let oid = sha256_hex(data);
        let body = data.clone();
        handles.push(tokio::spawn(async move {
            http.put(format!("{}/lfs/{}/objects/{}", url_base, rb, oid))
                .body(body)
                .send()
                .await
                .unwrap()
                .status()
        }));
    }

    for handle in handles {
        let status = handle.await.unwrap();
        assert_eq!(status, 200, "concurrent upload should succeed");
    }

    for data in &blobs {
        let oid = sha256_hex(data);
        let resp = http
            .get(format!("{}/lfs/{}/objects/{}", daemon_url, rb64, oid))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let downloaded = resp.bytes().await.unwrap();
        assert_eq!(
            &downloaded[..],
            &data[..],
            "downloaded content should match"
        );
    }
}

/// 5 concurrent lock attempts on the SAME path. Exactly 1 should succeed (201),
/// the other 4 should get 409. Then verify list shows exactly 1 lock.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_lock_unlock() {
    let signer = Signer::generate();
    let blossom_url = spawn_blossom_server_with_locks(&signer).await;
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let rb64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let mut handles = Vec::new();
    for _ in 0..5 {
        let http = http.clone();
        let url = format!("{}/lfs/{}/locks", daemon_url, rb64);
        handles.push(tokio::spawn(async move {
            http.post(&url)
                .json(&serde_json::json!({"path": "concurrent.bin"}))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }

    let mut created_count = 0u32;
    let mut conflict_count = 0u32;
    for handle in handles {
        let status = handle.await.unwrap();
        if status == 201 {
            created_count += 1;
        } else if status == 409 {
            conflict_count += 1;
        } else {
            panic!("unexpected lock status: {}", status);
        }
    }
    assert_eq!(created_count, 1, "exactly one lock should be created");
    assert_eq!(conflict_count, 4, "four requests should conflict");

    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, rb64))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["locks"].as_array().unwrap().len(),
        1,
        "list should show exactly one lock"
    );
}
