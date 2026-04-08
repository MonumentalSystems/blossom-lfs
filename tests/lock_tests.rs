use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use base64::Engine;
use blossom_lfs::lock_client::LockClient;
use blossom_rs::auth::Signer;
use blossom_rs::BlossomSigner;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
struct StoredLock {
    id: String,
    path: String,
    pubkey: String,
    locked_at: u64,
}

#[derive(Debug, Default)]
struct LockStore {
    locks: HashMap<String, StoredLock>,
    next_id: u64,
}

impl LockStore {
    fn new() -> Self {
        Self {
            locks: HashMap::new(),
            next_id: 1,
        }
    }

    fn create(
        &mut self,
        repo_id: &str,
        path: &str,
        pubkey: &str,
    ) -> Result<StoredLock, StoredLock> {
        let key = format!("{}:{}", repo_id, path);
        if let Some(existing) = self.locks.get(&key) {
            return Err(existing.clone());
        }
        let lock = StoredLock {
            id: format!("lock-{}", self.next_id),
            path: path.to_string(),
            pubkey: pubkey.to_string(),
            locked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        self.next_id += 1;
        self.locks.insert(key, lock.clone());
        Ok(lock)
    }

    fn delete(
        &mut self,
        _repo_id: &str,
        lock_id: &str,
        force: bool,
        pubkey: &str,
    ) -> Result<StoredLock, String> {
        let entry = self
            .locks
            .iter()
            .find(|(_, l)| l.id == lock_id)
            .map(|(k, v)| (k.clone(), v.clone()));

        match entry {
            Some((key, lock)) => {
                if !force && lock.pubkey != pubkey {
                    return Err("only the lock owner or an admin can unlock".to_string());
                }
                self.locks.remove(&key);
                Ok(lock)
            }
            None => Err("not found".to_string()),
        }
    }

    fn list(&self, repo_id: &str, path_filter: Option<&str>) -> Vec<StoredLock> {
        let prefix = format!("{}:", repo_id);
        let mut locks: Vec<_> = self
            .locks
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v.clone())
            .collect();
        if let Some(p) = path_filter {
            locks.retain(|l| l.path == p);
        }
        locks
    }
}

type SharedLockStore = Arc<Mutex<LockStore>>;

fn extract_pubkey(headers: &HeaderMap) -> Result<String, StatusCode> {
    let header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if !header.starts_with("Nostr ") {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let b64 = &header["Nostr ".len()..];
    let json_bytes =
        blossom_rs::protocol::base64url_decode(b64).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let event: blossom_rs::protocol::NostrEvent =
        serde_json::from_slice(&json_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    Ok(event.pubkey)
}

async fn handle_create_lock(
    axum::extract::State(store): axum::extract::State<SharedLockStore>,
    Path(repo_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let pubkey = match extract_pubkey(&headers) {
        Ok(pk) => pk,
        Err(code) => return (code, Json(serde_json::json!({"message": "auth required"}))),
    };

    let path = body["path"].as_str().unwrap_or("");
    let mut store = store.lock().await;

    match store.create(&repo_id, path, &pubkey) {
        Ok(lock) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "lock": {
                    "id": lock.id,
                    "path": lock.path,
                    "locked_at": format!("{}Z", lock.locked_at),
                    "owner": { "name": lock.pubkey },
                }
            })),
        ),
        Err(existing) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "lock": {
                    "id": existing.id,
                    "path": existing.path,
                    "locked_at": format!("{}Z", existing.locked_at),
                    "owner": { "name": existing.pubkey },
                },
                "message": "path already locked",
            })),
        ),
    }
}

async fn handle_list_locks(
    axum::extract::State(store): axum::extract::State<SharedLockStore>,
    Path(repo_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let path_filter = params.get("path").map(|s| s.as_str());
    let store = store.lock().await;
    let locks: Vec<_> = store
        .list(&repo_id, path_filter)
        .into_iter()
        .map(|l| {
            serde_json::json!({
                "id": l.id,
                "path": l.path,
                "locked_at": format!("{}Z", l.locked_at),
                "owner": { "name": l.pubkey },
            })
        })
        .collect();
    (StatusCode::OK, Json(serde_json::json!({ "locks": locks })))
}

async fn handle_verify_locks(
    axum::extract::State(store): axum::extract::State<SharedLockStore>,
    Path(repo_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let pubkey = match extract_pubkey(&headers) {
        Ok(pk) => pk,
        Err(code) => return (code, Json(serde_json::json!({"message": "auth required"}))),
    };
    let store = store.lock().await;
    let locks = store.list(&repo_id, None);
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    for l in locks {
        let json = serde_json::json!({
            "id": l.id,
            "path": l.path,
            "locked_at": format!("{}Z", l.locked_at),
            "owner": { "name": l.pubkey },
        });
        if l.pubkey == pubkey {
            ours.push(json);
        } else {
            theirs.push(json);
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ours": ours, "theirs": theirs })),
    )
}

async fn handle_unlock(
    axum::extract::State(store): axum::extract::State<SharedLockStore>,
    Path((repo_id, lock_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let pubkey = match extract_pubkey(&headers) {
        Ok(pk) => pk,
        Err(code) => return (code, Json(serde_json::json!({"message": "auth required"}))),
    };
    let force = body["force"].as_bool().unwrap_or(false);
    let mut store = store.lock().await;

    match store.delete(&repo_id, &lock_id, force, &pubkey) {
        Ok(lock) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "lock": {
                    "id": lock.id,
                    "path": lock.path,
                    "locked_at": format!("{}Z", lock.locked_at),
                    "owner": { "name": lock.pubkey },
                }
            })),
        ),
        Err(msg) if msg.contains("not found") => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"message": msg})),
        ),
        Err(msg) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"message": msg})),
        ),
    }
}

async fn spawn_lock_server() -> String {
    let store = Arc::new(Mutex::new(LockStore::new()));

    let app = Router::new()
        .route(
            "/lfs/{repo_id}/locks",
            post(handle_create_lock).get(handle_list_locks),
        )
        .route("/lfs/{repo_id}/locks/verify", post(handle_verify_locks))
        .route("/lfs/{repo_id}/locks/{lock_id}/unlock", post(handle_unlock))
        .with_state(store);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    url
}

fn make_client(server_url: &str, signer: &Signer) -> LockClient {
    LockClient::new(server_url.to_string(), signer.secret_key_hex())
}

#[tokio::test]
async fn test_lock_client_create() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    let lock = client
        .create_lock("myrepo", "assets/big-file.bin")
        .await
        .unwrap();
    assert_eq!(lock.path, "assets/big-file.bin");
    assert!(lock.id.starts_with("lock-"));
    assert_eq!(lock.owner.name, signer.public_key_hex());
}

#[tokio::test]
async fn test_lock_client_create_conflict() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    client.create_lock("myrepo", "file.txt").await.unwrap();

    let err = client.create_lock("myrepo", "file.txt").await.unwrap_err();
    assert!(err.to_string().contains("already locked"));
}

#[tokio::test]
async fn test_lock_client_create_different_repos() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    client.create_lock("repo1", "file.txt").await.unwrap();
    let lock2 = client.create_lock("repo2", "file.txt").await.unwrap();
    assert_eq!(lock2.path, "file.txt");
}

#[tokio::test]
async fn test_lock_client_unlock_by_owner() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    let lock = client.create_lock("myrepo", "file.txt").await.unwrap();
    let unlocked = client.unlock("myrepo", &lock.id, false).await.unwrap();
    assert_eq!(unlocked.id, lock.id);
}

#[tokio::test]
async fn test_lock_client_unlock_by_non_owner() {
    let url = spawn_lock_server().await;
    let owner = Signer::generate();
    let other = Signer::generate();

    let owner_client = make_client(&url, &owner);
    let lock = owner_client
        .create_lock("myrepo", "file.txt")
        .await
        .unwrap();

    let other_client = make_client(&url, &other);
    let err = other_client
        .unlock("myrepo", &lock.id, false)
        .await
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(msg.contains("owner") || msg.contains("forbidden"));
}

#[tokio::test]
async fn test_lock_client_unlock_not_found() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    let err = client
        .unlock("myrepo", "nonexistent-id", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found") || err.to_string().contains("404"));
}

#[tokio::test]
async fn test_lock_client_list_locks() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    client.create_lock("myrepo", "a.txt").await.unwrap();
    client.create_lock("myrepo", "b.txt").await.unwrap();

    let (locks, _) = client.list_locks("myrepo", None, None, None).await.unwrap();
    assert_eq!(locks.len(), 2);
}

#[tokio::test]
async fn test_lock_client_list_locks_with_path_filter() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    client.create_lock("myrepo", "a.txt").await.unwrap();
    client.create_lock("myrepo", "b.txt").await.unwrap();

    let (locks, _) = client
        .list_locks("myrepo", Some("a.txt"), None, None)
        .await
        .unwrap();
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].path, "a.txt");
}

#[tokio::test]
async fn test_lock_client_list_locks_empty() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    let (locks, _) = client
        .list_locks("empty-repo", None, None, None)
        .await
        .unwrap();
    assert!(locks.is_empty());
}

#[tokio::test]
async fn test_lock_client_verify_locks() {
    let url = spawn_lock_server().await;
    let owner = Signer::generate();
    let other = Signer::generate();

    let owner_client = make_client(&url, &owner);
    owner_client
        .create_lock("myrepo", "owner-file.txt")
        .await
        .unwrap();

    let other_client = make_client(&url, &other);
    other_client
        .create_lock("myrepo", "other-file.txt")
        .await
        .unwrap();

    let (ours, theirs, _) = owner_client
        .verify_locks("myrepo", None, None)
        .await
        .unwrap();
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].path, "owner-file.txt");
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].path, "other-file.txt");
}

#[tokio::test]
async fn test_lock_client_lifecycle() {
    let url = spawn_lock_server().await;
    let signer = Signer::generate();
    let client = make_client(&url, &signer);

    let lock = client
        .create_lock("lifecycle", "big-file.bin")
        .await
        .unwrap();

    let (ours, _, _) = client.verify_locks("lifecycle", None, None).await.unwrap();
    assert_eq!(ours.len(), 1);

    let (locks, _) = client
        .list_locks("lifecycle", None, None, None)
        .await
        .unwrap();
    assert_eq!(locks.len(), 1);

    client.unlock("lifecycle", &lock.id, false).await.unwrap();

    let (locks_after, _) = client
        .list_locks("lifecycle", None, None, None)
        .await
        .unwrap();
    assert!(locks_after.is_empty());
}

mod daemon_tests {
    use super::*;

    async fn spawn_lock_server() -> String {
        super::spawn_lock_server().await
    }

    fn repo_path_b64(path: &std::path::Path) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path.to_string_lossy().as_bytes())
    }

    async fn setup_git_repo(nsec_hex: &str, server_url: &str) -> tempfile::TempDir {
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

        let config_content = format!("server={}\nprivate-key={}", server_url, nsec_hex);
        std::fs::write(repo_path.join(".lfsdalconfig"), config_content).unwrap();

        dir
    }

    async fn find_free_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn test_daemon_create_and_list_locks() {
        let blossom_url = spawn_lock_server().await;
        let signer = Signer::generate();
        let repo_dir = setup_git_repo(&signer.secret_key_hex(), &blossom_url).await;
        let repo_b64 = repo_path_b64(repo_dir.path());

        let port = find_free_port().await;
        tokio::spawn(blossom_lfs::run_daemon(port));
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let daemon_url = format!("http://127.0.0.1:{}", port);
        let http = reqwest::Client::new();

        let resp = http
            .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
            .json(&serde_json::json!({"path": "file.txt"}))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(status, 201, "daemon create failed: {:?}", body);
        assert_eq!(body["lock"]["path"], "file.txt");

        let resp = http
            .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["locks"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_daemon_create_conflict() {
        let blossom_url = spawn_lock_server().await;
        let signer = Signer::generate();
        let repo_dir = setup_git_repo(&signer.secret_key_hex(), &blossom_url).await;
        let repo_b64 = repo_path_b64(repo_dir.path());

        let port = find_free_port().await;
        tokio::spawn(blossom_lfs::run_daemon(port));
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let daemon_url = format!("http://127.0.0.1:{}", port);
        let http = reqwest::Client::new();

        let resp = http
            .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
            .json(&serde_json::json!({"path": "file.txt"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        let resp = http
            .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
            .json(&serde_json::json!({"path": "file.txt"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);
    }

    #[tokio::test]
    async fn test_daemon_unlock_and_verify() {
        let blossom_url = spawn_lock_server().await;
        let signer = Signer::generate();
        let repo_dir = setup_git_repo(&signer.secret_key_hex(), &blossom_url).await;
        let repo_b64 = repo_path_b64(repo_dir.path());

        let port = find_free_port().await;
        tokio::spawn(blossom_lfs::run_daemon(port));
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let daemon_url = format!("http://127.0.0.1:{}", port);
        let http = reqwest::Client::new();

        let resp = http
            .post(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
            .json(&serde_json::json!({"path": "big-file.bin"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        let lock_id = body["lock"]["id"].as_str().unwrap();

        let resp = http
            .post(format!("{}/lfs/{}/locks/verify", daemon_url, repo_b64))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ours"].as_array().unwrap().len(), 1);

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

        let resp = http
            .get(format!("{}/lfs/{}/locks", daemon_url, repo_b64))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["locks"].as_array().unwrap().is_empty());
    }
}
