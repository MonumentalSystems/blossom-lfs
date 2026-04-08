//! Cross-repo lock isolation integration tests.
//!
//! Verifies that locks in one repository are invisible to another repository,
//! even when both share the same blossom-rs server. Repo identity is derived
//! from the git remote URL, so different remotes produce different namespaces.

use base64::Engine;
use blossom_rs::access::RoleBasedAccess;
use blossom_rs::auth::Signer;
use blossom_rs::server::BlobServer;
use blossom_rs::storage::MemoryBackend;
use blossom_rs::{BlossomSigner, MemoryDatabase, MemoryLockDatabase};
use std::collections::HashSet;

fn repo_b64(repo_path: &std::path::Path) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(repo_path.to_string_lossy().as_bytes())
}

fn setup_git_repo_with_remote(
    server_url: &str,
    nsec_hex: &str,
    remote_url: &str,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(repo_path)
        .output()
        .expect("git init failed");

    std::process::Command::new("git")
        .args(["remote", "add", "origin", remote_url])
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

/// Two repos with different remotes can each lock the same path independently.
/// Repo A locks "file.bin" → 201. Repo B locks "file.bin" → 201 (different
/// namespace). Each repo's lock list shows only its own lock.
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_repo_lock_isolation() {
    let signer = Signer::generate();
    let blossom_url = spawn_blossom_server_with_locks(&signer).await;

    let repo_dir_a =
        setup_git_repo_with_remote(&blossom_url, &signer.secret_key_hex(), "https://repo-a");
    let rb64_a = repo_b64(repo_dir_a.path());
    let repo_dir_b =
        setup_git_repo_with_remote(&blossom_url, &signer.secret_key_hex(), "https://repo-b");
    let rb64_b = repo_b64(repo_dir_b.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, rb64_a))
        .json(&serde_json::json!({"path": "file.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "repo A lock should succeed");

    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, rb64_b))
        .json(&serde_json::json!({"path": "file.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "repo B lock on same path should succeed (different repo namespace)"
    );

    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, rb64_a))
        .send()
        .await
        .unwrap();
    let body_a: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body_a["locks"].as_array().unwrap().len(),
        1,
        "repo A should see exactly 1 lock"
    );
    assert_eq!(body_a["locks"][0]["path"], "file.bin");

    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, rb64_b))
        .send()
        .await
        .unwrap();
    let body_b: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body_b["locks"].as_array().unwrap().len(),
        1,
        "repo B should see exactly 1 lock"
    );
    assert_eq!(body_b["locks"][0]["path"], "file.bin");
}

/// Repo A locks "file.bin". Repo B tries to unlock Repo A's lock by lock_id
/// → 404 (lock doesn't exist in repo B's namespace).
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_repo_unlock_isolation() {
    let signer = Signer::generate();
    let blossom_url = spawn_blossom_server_with_locks(&signer).await;

    let repo_dir_a =
        setup_git_repo_with_remote(&blossom_url, &signer.secret_key_hex(), "https://repo-a");
    let rb64_a = repo_b64(repo_dir_a.path());
    let repo_dir_b =
        setup_git_repo_with_remote(&blossom_url, &signer.secret_key_hex(), "https://repo-b");
    let rb64_b = repo_b64(repo_dir_b.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, rb64_a))
        .json(&serde_json::json!({"path": "file.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let lock_id = resp.json::<serde_json::Value>().await.unwrap()["lock"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, rb64_b, lock_id
        ))
        .json(&serde_json::json!({"force": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "repo B should not find repo A's lock in its namespace"
    );
}
