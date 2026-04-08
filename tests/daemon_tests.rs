use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use base64::Engine;
use blossom_rs::auth::Signer;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
struct StoredBlob {
    sha256: String,
    data: Vec<u8>,
    content_type: String,
}

#[derive(Debug, Default)]
struct BlobStore {
    blobs: HashMap<String, StoredBlob>,
}

impl BlobStore {
    fn new() -> Self {
        Self {
            blobs: HashMap::new(),
        }
    }

    fn store(&mut self, data: Vec<u8>, content_type: &str) -> StoredBlob {
        let hash = format!("{:x}", Sha256::digest(&data));
        let blob = StoredBlob {
            sha256: hash.clone(),
            data,
            content_type: content_type.to_string(),
        };
        self.blobs.insert(hash.clone(), blob.clone());
        blob
    }

    fn get(&self, sha256: &str) -> Option<&StoredBlob> {
        self.blobs.get(sha256)
    }

    fn exists(&self, sha256: &str) -> bool {
        self.blobs.contains_key(sha256)
    }
}

type SharedStore = Arc<Mutex<BlobStore>>;

async fn blossom_put_upload(
    axum::extract::State(store): axum::extract::State<SharedStore>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    let blob = store.lock().await.store(body.to_vec(), content_type);
    let desc = serde_json::json!({
        "sha256": blob.sha256,
        "size": blob.data.len(),
        "type": blob.content_type,
        "url": format!("/{}", blob.sha256),
        "uploaded": 0,
    });
    (StatusCode::OK, Json(desc))
}

async fn blossom_get_blob(
    axum::extract::State(store): axum::extract::State<SharedStore>,
    axum::extract::Path(sha256): axum::extract::Path<String>,
) -> impl IntoResponse {
    let store = store.lock().await;
    match store.get(&sha256) {
        Some(blob) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, blob.content_type.clone())],
            blob.data.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn blossom_head_blob(
    axum::extract::State(store): axum::extract::State<SharedStore>,
    axum::extract::Path(sha256): axum::extract::Path<String>,
) -> StatusCode {
    let store = store.lock().await;
    if store.exists(&sha256) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn spawn_blossom_server() -> (String, SharedStore) {
    let store = Arc::new(Mutex::new(BlobStore::new()));

    let app = Router::new()
        .route("/upload", put(blossom_put_upload))
        .route("/{sha256}", get(blossom_get_blob))
        .route("/{sha256}", axum::routing::head(blossom_head_blob))
        .with_state(store.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (format!("http://127.0.0.1:{}", port), store)
}

async fn spawn_lfs_daemon(port: u16) {
    tokio::spawn(blossom_lfs::daemon::run_daemon(port));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

fn make_config_content(server_url: &str, nsec_hex: &str) -> String {
    format!("server={}\nprivate-key={}", server_url, nsec_hex)
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
        make_config_content(server_url, nsec_hex),
    )
    .unwrap();

    dir
}

fn repo_b64(repo_path: &std::path::Path) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(repo_path.to_string_lossy().as_bytes())
}

async fn find_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_upload_returns_urls() {
    let (blossom_url, _) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/{}/objects/batch", daemon_url, repo_b64))
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": "abc123", "size": 1024}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["transfer"], "basic");
    assert_eq!(body["objects"].as_array().unwrap().len(), 1);

    let obj = &body["objects"][0];
    assert_eq!(obj["oid"], "abc123");
    assert!(obj["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .contains("/lfs/"));
    assert!(obj["actions"]["verify"]["href"]
        .as_str()
        .unwrap()
        .ends_with("/verify"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_download_returns_urls() {
    let (blossom_url, _) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/{}/objects/batch", daemon_url, repo_b64))
        .json(&serde_json::json!({
            "operation": "download",
            "objects": [{"oid": "abc123", "size": 1024}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let obj = &body["objects"][0];
    assert!(obj["actions"]["download"]["href"]
        .as_str()
        .unwrap()
        .contains("/objects/"));
    assert!(obj["actions"]["upload"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_upload_and_download_raw_blob() {
    let (blossom_url, _store) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data = b"hello blossom-lfs daemon!";
    let oid = sha256_hex(data);

    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(&downloaded[..], data);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_download_not_found() {
    let (blossom_url, _) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .get(format!(
            "{}/lfs/{}/objects/{}",
            daemon_url,
            repo_b64,
            "f".repeat(64)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_existing_object() {
    let (blossom_url, _store) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data = b"verify me";
    let oid = sha256_hex(data);

    http.put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.to_vec())
        .send()
        .await
        .unwrap();

    let resp = http
        .post(format!(
            "{}/lfs/{}/objects/{}/verify",
            daemon_url, repo_b64, oid
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_verify_missing_object() {
    let (blossom_url, _) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!(
            "{}/lfs/{}/objects/{}/verify",
            daemon_url,
            repo_b64,
            "f".repeat(64)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_upload_dedup_skips_existing() {
    let (blossom_url, store) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data = b"already here";
    let oid = sha256_hex(data);

    store
        .lock()
        .await
        .store(data.to_vec(), "application/octet-stream");

    let resp = http
        .put(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .body(data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = http
        .get(format!("{}/lfs/{}/objects/{}", daemon_url, repo_b64, oid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(&downloaded[..], data);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_multiple_objects() {
    let (blossom_url, _) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/{}/objects/batch", daemon_url, repo_b64))
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [
                {"oid": "aaa", "size": 100},
                {"oid": "bbb", "size": 200},
                {"oid": "ccc", "size": 300}
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let objects = body["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 3);
    for obj in objects {
        assert!(obj["actions"]["upload"]["href"].is_string());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_invalid_base64() {
    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{}/lfs/!!!invalid!!!/objects/batch", daemon_url))
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_full_lfs_workflow() {
    let (blossom_url, _store) = spawn_blossom_server().await;
    let signer = Signer::generate();
    let repo_dir = setup_git_repo(&blossom_url, &signer.secret_key_hex());
    let repo_b64 = repo_b64(repo_dir.path());

    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    let daemon_url = format!("http://127.0.0.1:{}", daemon_port);
    let http = reqwest::Client::new();

    let data = b"full workflow test data for blossom-lfs";
    let oid = sha256_hex(data);

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

    // Upload via PUT
    let resp = http
        .put(&upload_url)
        .body(data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify
    let resp = http.post(&verify_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Batch download
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

    // Download via GET
    let resp = http.get(&download_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let downloaded = resp.bytes().await.unwrap();
    assert_eq!(&downloaded[..], data);
}
