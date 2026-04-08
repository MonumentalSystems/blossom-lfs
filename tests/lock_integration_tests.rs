//! Full-stack lock integration tests (BUD-19).
//!
//! Tests the complete lock workflow through the real blossom-rs server +
//! blossom-lfs daemon: create, conflict, list, verify, unlock, admin force
//! unlock, multi-user scenarios.

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

async fn spawn_blossom_server_with_access(admin_signer: &Signer, member_signer: &Signer) -> String {
    let mut admins = HashSet::new();
    admins.insert(admin_signer.public_key_hex());
    let mut members = HashSet::new();
    members.insert(member_signer.public_key_hex());

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

async fn find_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

async fn spawn_lfs_daemon(port: u16) {
    tokio::spawn(blossom_lfs::daemon::run_daemon(port));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

/// User A locks a file, User B tries to lock the same file → 409 conflict.
#[tokio::test(flavor = "multi_thread")]
async fn test_lock_conflict_two_users() {
    let admin = Signer::generate();
    let member = Signer::generate();
    let blossom_url = spawn_blossom_server_with_access(&admin, &member).await;

    let repo_dir_a = setup_git_repo(&blossom_url, &admin.secret_key_hex());
    let repo_b64_a = repo_b64(repo_dir_a.path());
    let repo_dir_b = setup_git_repo(&blossom_url, &member.secret_key_hex());
    let repo_b64_b = repo_b64(repo_dir_b.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;
    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // User A locks file
    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64_a))
        .json(&serde_json::json!({"path": "assets/model.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "user A lock should succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["lock"]["path"], "assets/model.bin");

    // User B tries to lock same file (different repo dir → same server repo slug via git remote)
    // Both repos have remote "origin https://myrepo" so they get the same slug.
    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64_b))
        .json(&serde_json::json!({"path": "assets/model.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        409,
        "user B should get conflict on already-locked file"
    );
}

/// User A locks a file, User B tries to unlock it → 403 forbidden.
#[tokio::test(flavor = "multi_thread")]
async fn test_unlock_denied_for_non_owner() {
    let admin = Signer::generate();
    let member = Signer::generate();
    let blossom_url = spawn_blossom_server_with_access(&admin, &member).await;

    let repo_dir_a = setup_git_repo(&blossom_url, &admin.secret_key_hex());
    let repo_b64_a = repo_b64(repo_dir_a.path());
    let repo_dir_b = setup_git_repo(&blossom_url, &member.secret_key_hex());
    let repo_b64_b = repo_b64(repo_dir_b.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;
    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // User A locks
    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64_a))
        .json(&serde_json::json!({"path": "big-file.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let lock_id = resp.json::<serde_json::Value>().await.unwrap()["lock"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // User B tries to unlock (without force)
    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, repo_b64_b, lock_id
        ))
        .json(&serde_json::json!({"force": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "user B should be forbidden from unlocking user A's lock"
    );
}

/// User A locks, User A unlocks → lock removed. Verify via list.
#[tokio::test(flavor = "multi_thread")]
async fn test_owner_unlock_and_list() {
    let admin = Signer::generate();
    let member = Signer::generate();
    let blossom_url = spawn_blossom_server_with_access(&admin, &member).await;

    let repo_dir = setup_git_repo(&blossom_url, &admin.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;
    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // Lock
    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .json(&serde_json::json!({"path": "data.csv"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let lock_id = resp.json::<serde_json::Value>().await.unwrap()["lock"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Verify lock exists
    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["locks"].as_array().unwrap().len(), 1);

    // Owner unlocks
    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, repo_b64, lock_id
        ))
        .json(&serde_json::json!({"force": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify lock removed
    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["locks"].as_array().unwrap().is_empty(),
        "lock should be removed after unlock"
    );
}

/// Admin forces unlock of User A's lock → success.
#[tokio::test(flavor = "multi_thread")]
async fn test_admin_force_unlock() {
    let admin = Signer::generate();
    let member = Signer::generate();
    let blossom_url = spawn_blossom_server_with_access(&admin, &member).await;

    // Member locks
    let member_repo = setup_git_repo(&blossom_url, &member.secret_key_hex());
    let member_b64 = repo_b64(member_repo.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;
    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, member_b64))
        .json(&serde_json::json!({"path": "protected.dat"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let lock_id = resp.json::<serde_json::Value>().await.unwrap()["lock"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Admin repo (same server, same slug)
    let admin_repo = setup_git_repo(&blossom_url, &admin.secret_key_hex());
    let admin_b64 = repo_b64(admin_repo.path());

    // Admin forces unlock (admin role means force=true is implicit on server side)
    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, admin_b64, lock_id
        ))
        .json(&serde_json::json!({"force": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "admin should be able to force-unlock any lock"
    );

    // Verify lock removed
    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, member_b64))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["locks"].as_array().unwrap().is_empty(),
        "lock should be gone after admin force-unlock"
    );
}

/// Verify locks: User A and User B each lock a file. User A calls verify →
/// sees their lock in "ours" and User B's lock in "theirs".
#[tokio::test(flavor = "multi_thread")]
async fn test_verify_locks_ours_and_theirs() {
    let admin = Signer::generate();
    let member = Signer::generate();
    let blossom_url = spawn_blossom_server_with_access(&admin, &member).await;

    let repo_dir_a = setup_git_repo(&blossom_url, &admin.secret_key_hex());
    let repo_b64_a = repo_b64(repo_dir_a.path());
    let repo_dir_b = setup_git_repo(&blossom_url, &member.secret_key_hex());
    let repo_b64_b = repo_b64(repo_dir_b.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;
    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // User A locks file-a
    http.post(format!("{}/lfs/{}/locks", daemon_url, repo_b64_a))
        .json(&serde_json::json!({"path": "file-a.txt"}))
        .send()
        .await
        .unwrap()
        .status();

    // User B locks file-b
    http.post(format!("{}/lfs/{}/locks", daemon_url, repo_b64_b))
        .json(&serde_json::json!({"path": "file-b.txt"}))
        .send()
        .await
        .unwrap();

    // User A verifies → ours has file-a, theirs has file-b
    let resp = http
        .post(format!("{}/lfs/{}/locks/verify", daemon_url, repo_b64_a))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let ours = body["ours"].as_array().unwrap();
    let theirs = body["theirs"].as_array().unwrap();
    assert_eq!(ours.len(), 1, "user A should see 1 lock in ours");
    assert_eq!(ours[0]["path"], "file-a.txt");
    assert_eq!(theirs.len(), 1, "user A should see 1 lock in theirs");
    assert_eq!(theirs[0]["path"], "file-b.txt");
}

/// Lock lifecycle: create → list → verify → unlock → list empty.
#[tokio::test(flavor = "multi_thread")]
async fn test_full_lock_lifecycle() {
    let admin = Signer::generate();
    let member = Signer::generate();
    let blossom_url = spawn_blossom_server_with_access(&admin, &member).await;

    let repo_dir = setup_git_repo(&blossom_url, &admin.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;
    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    // 1. Create lock
    let resp = http
        .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .json(&serde_json::json!({"path": "lifecycle.bin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let lock_id = resp.json::<serde_json::Value>().await.unwrap()["lock"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 2. List locks → 1 lock
    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["locks"].as_array().unwrap().len(), 1);
    assert_eq!(body["locks"][0]["path"], "lifecycle.bin");

    // 3. Verify → ours has 1
    let resp = http
        .post(format!("{}/lfs/{}/locks/verify", daemon_url, repo_b64))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ours"].as_array().unwrap().len(), 1);
    assert!(body["theirs"].as_array().unwrap().is_empty());

    // 4. Unlock
    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/{}/unlock",
            daemon_url, repo_b64, lock_id
        ))
        .json(&serde_json::json!({"force": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 5. List locks → empty
    let resp = http
        .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["locks"].as_array().unwrap().is_empty());
}

/// Unlock a nonexistent lock → 404.
#[tokio::test(flavor = "multi_thread")]
async fn test_unlock_nonexistent_lock() {
    let admin = Signer::generate();
    let member = Signer::generate();
    let blossom_url = spawn_blossom_server_with_access(&admin, &member).await;

    let repo_dir = setup_git_repo(&blossom_url, &admin.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;
    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!(
            "{}/lfs/{}/locks/fake-lock-id/unlock",
            daemon_url, repo_b64
        ))
        .json(&serde_json::json!({"force": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
