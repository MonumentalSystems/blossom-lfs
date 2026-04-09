use anyhow::{Context as _, Result};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use blossom_rs::auth::Signer;
use bytes::Bytes;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument, warn};

use crate::chunking::{Chunker, Manifest};
use crate::config::{Config, ForceTransport};
use crate::lock_client::{LockClient, LockTransport};
use crate::transport::Transport;

#[derive(Clone)]
struct DaemonState {
    port: u16,
}

pub async fn run_daemon(port: u16) -> Result<()> {
    let state = DaemonState { port };

    let app = Router::new()
        .route("/lfs/{repo_b64}/objects/batch", post(handle_batch))
        .route(
            "/lfs/{repo_b64}/objects/{oid}",
            get(handle_download).put(handle_upload),
        )
        .route("/lfs/{repo_b64}/objects/{oid}/verify", post(handle_verify))
        .route(
            "/lfs/{repo_b64}/locks",
            post(handle_create_lock).get(handle_list_locks),
        )
        .route("/lfs/{repo_b64}/locks/verify", post(handle_verify_locks))
        .route(
            "/lfs/{repo_b64}/locks/{lock_id}/unlock",
            post(handle_unlock),
        )
        .with_state(Arc::new(state));

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!(daemon.port = port, "blossom-lfs daemon starting");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind daemon to 127.0.0.1:{}", port))?;

    info!(daemon.addr = %addr, "blossom-lfs daemon listening");
    axum::serve(listener, app).await?;

    Ok(())
}

fn decode_repo_path(repo_b64: &str) -> Result<PathBuf> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(repo_b64)
        .map_err(|e| anyhow::anyhow!("invalid base64url repo path: {}", e))?;
    let path_str = String::from_utf8(bytes)
        .map_err(|e| anyhow::anyhow!("repo path is not valid UTF-8: {}", e))?;
    let path = PathBuf::from(&path_str);

    if !path.is_absolute() {
        anyhow::bail!("repo path must be absolute: {:?}", path);
    }

    if !path.join(".git").exists() {
        anyhow::bail!("path does not appear to be a git repository: {:?}", path);
    }

    Ok(path)
}

async fn make_transport(config: &Config) -> Result<Transport> {
    let key = config
        .secret_key_hex
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("private key required for this operation — set in .lfsdalconfig, .git/config, or NOSTR_PRIVATE_KEY env"))?;
    let signer =
        Signer::from_secret_hex(key).map_err(|e| anyhow::anyhow!("invalid secret key: {}", e))?;

    let server_url = config
        .server_url
        .clone()
        .unwrap_or_else(|| "http://localhost:0".to_string());

    let mut transport = match config.iroh_endpoint {
        Some(ref _endpoint_id) => {
            #[cfg(feature = "iroh")]
            {
                let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to create iroh endpoint: {}", e))?;
                let eid: iroh::EndpointId = _endpoint_id
                    .parse()
                    .map_err(|e| anyhow::anyhow!("invalid iroh endpoint ID: {}", e))?;
                let iroh_client =
                    blossom_rs::transport::IrohBlossomClient::new(endpoint, signer.clone());
                let peer = iroh::EndpointAddr::from(eid);
                Transport::multi(
                    server_url,
                    signer.clone(),
                    std::time::Duration::from_secs(300),
                    iroh_client,
                    peer,
                )
            }
            #[cfg(not(feature = "iroh"))]
            {
                Transport::http_only(
                    server_url,
                    signer.clone(),
                    std::time::Duration::from_secs(300),
                )
            }
        }
        None => Transport::http_only(
            server_url,
            signer.clone(),
            std::time::Duration::from_secs(300),
        ),
    };

    match config.force_transport {
        Some(ForceTransport::Http) => transport = transport.force_http(),
        Some(ForceTransport::Iroh) => {
            #[cfg(feature = "iroh")]
            {
                transport = transport.force_iroh();
            }
        }
        None => {}
    }

    Ok(transport)
}

fn load_config(repo_path: &std::path::Path) -> Result<Config> {
    Config::from_repo_path(repo_path)
}

#[allow(dead_code)] // Retained for future protected/authenticated downloads
async fn load_transport(repo_path: &std::path::Path) -> Result<Transport> {
    let config = load_config(repo_path)?;
    make_transport(&config).await
}

/// Load transport for read-only operations (downloads).
/// Uses a dummy signer if no private key is configured — downloads
/// don't require authentication on Blossom servers.
async fn load_read_transport(repo_path: &std::path::Path) -> Result<Transport> {
    let config = load_config(repo_path)?;
    if config.secret_key_hex.is_some() {
        make_transport(&config).await
    } else {
        let server_url = config
            .server_url
            .clone()
            .unwrap_or_else(|| "http://localhost:0".to_string());
        // Generate a throwaway signer for download-only transport
        let signer = Signer::generate();
        Ok(Transport::http_only(
            server_url,
            signer,
            std::time::Duration::from_secs(300),
        ))
    }
}

async fn load_client(repo_path: &std::path::Path) -> Result<(Config, String, LockTransport)> {
    let config = Config::from_repo_path(repo_path)?;
    let repo_slug = derive_repo_slug(repo_path).await?;
    let lock_transport = make_lock_transport(&config).await?;
    Ok((config, repo_slug, lock_transport))
}

async fn make_lock_transport(config: &Config) -> Result<LockTransport> {
    let use_iroh =
        config.force_transport == Some(ForceTransport::Iroh) && config.iroh_endpoint.is_some();

    if use_iroh {
        #[cfg(feature = "iroh")]
        {
            let signer = Signer::from_secret_hex(&config.secret_key_hex)
                .map_err(|e| anyhow::anyhow!("invalid secret key: {}", e))?;
            let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
                .await
                .map_err(|e| anyhow::anyhow!("failed to create iroh endpoint: {}", e))?;
            let endpoint_id = config.iroh_endpoint.as_deref().unwrap();
            let eid: iroh::EndpointId = endpoint_id
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid iroh endpoint ID: {}", e))?;
            let iroh_client = blossom_rs::transport::IrohBlossomClient::new(endpoint, signer);
            let addr = iroh::EndpointAddr::from(eid);
            return Ok(LockTransport::Iroh {
                client: iroh_client,
                addr,
            });
        }
        #[cfg(not(feature = "iroh"))]
        {
            anyhow::bail!("iroh transport requested but iroh feature not enabled");
        }
    }

    let server_url = config
        .server_url
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No server URL configured and iroh not enabled"))?;
    let key = config
        .secret_key_hex
        .clone()
        .ok_or_else(|| anyhow::anyhow!("private key required for lock operations"))?;
    Ok(LockTransport::Http(LockClient::new(
        server_url.clone(),
        key,
    )))
}

async fn derive_repo_slug(repo_path: &std::path::Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "remote",
            "get-url",
            "origin",
        ])
        .output()
        .context("failed to run git remote get-url")?;

    if !output.status.success() {
        let fallback = repo_path.to_string_lossy().to_string();
        warn!(path = %fallback, "no git remote configured, using path as slug");
        return Ok(fallback);
    }

    let remote_url = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let slug = remote_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("git@")
        .replace(':', "/")
        .trim_end_matches(".git")
        .to_string();

    Ok(slug)
}

fn object_url(port: u16, repo_b64: &str, oid: &str) -> String {
    format!("http://localhost:{}/lfs/{}/objects/{}", port, repo_b64, oid)
}

fn error_response(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "message": msg })))
}

fn internal_error(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    error!(error.message = %msg, "daemon error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "message": msg })),
    )
}

// ---------------------------------------------------------------------------
// LFS Batch API
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Upload,
    Download,
}

#[derive(Debug, Deserialize)]
struct BatchRequest {
    operation: Operation,
    #[serde(default)]
    #[allow(dead_code)]
    transfers: Vec<String>,
    objects: Vec<BatchObject>,
}

#[derive(Debug, Deserialize)]
struct BatchObject {
    oid: String,
    size: u64,
}

#[derive(Debug, Serialize)]
struct BatchResponse {
    transfer: &'static str,
    objects: Vec<BatchObjectResponse>,
}

#[derive(Debug, Serialize)]
struct BatchObjectResponse {
    oid: String,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    actions: Option<BatchActions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<BatchError>,
}

#[derive(Debug, Serialize)]
struct BatchActions {
    #[serde(skip_serializing_if = "Option::is_none")]
    upload: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    download: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<Action>,
}

#[derive(Debug, Serialize)]
struct Action {
    href: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    header: HashMap<String, String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Serialize)]
struct BatchError {
    code: u16,
    message: String,
}

#[instrument(name = "daemon.batch", skip_all, fields(repo_b64 = %repo_b64))]
async fn handle_batch(
    State(state): State<Arc<DaemonState>>,
    Path(repo_b64): Path<String>,
    body: axum::body::Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let req: BatchRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                &format!("invalid batch request: {}", e),
            )
        }
    };
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let config = match load_config(&repo_path) {
        Ok(c) => c,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    let _chunk_size = config.chunk_size;

    let objects: Vec<BatchObjectResponse> = req
        .objects
        .into_iter()
        .map(|obj| {
            let url = object_url(state.port, &repo_b64, &obj.oid);
            let verify_url = format!("{}/verify", url);

            let (actions, error) = match &req.operation {
                Operation::Upload => (
                    Some(BatchActions {
                        upload: Some(Action {
                            href: url,
                            header: HashMap::new(),
                            expires_in: Some(3600),
                        }),
                        download: None,
                        verify: Some(Action {
                            href: verify_url,
                            header: HashMap::new(),
                            expires_in: Some(3600),
                        }),
                    }),
                    None,
                ),
                Operation::Download => (
                    Some(BatchActions {
                        upload: None,
                        download: Some(Action {
                            href: url,
                            header: HashMap::new(),
                            expires_in: Some(3600),
                        }),
                        verify: None,
                    }),
                    None,
                ),
            };

            BatchObjectResponse {
                oid: obj.oid,
                size: obj.size,
                actions,
                error,
            }
        })
        .collect();

    (
        StatusCode::OK,
        Json(
            serde_json::to_value(BatchResponse {
                transfer: "basic",
                objects,
            })
            .unwrap_or_default(),
        ),
    )
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

#[instrument(name = "daemon.download", skip_all, fields(oid = %oid))]
async fn handle_download(
    State(_state): State<Arc<DaemonState>>,
    Path((repo_b64, oid)): Path<(String, String)>,
) -> axum::response::Response {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()).into_response(),
    };

    let transport = match load_read_transport(&repo_path).await {
        Ok(t) => t,
        Err(e) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
                .into_response()
        }
    };

    let blob_data = match transport.download(&oid).await {
        Ok(d) => d,
        Err(e) => return error_response(StatusCode::NOT_FOUND, &e.to_string()).into_response(),
    };

    let manifest_result = std::str::from_utf8(&blob_data)
        .ok()
        .and_then(|s| Manifest::from_json(s).ok());

    if let Some(manifest) = manifest_result {
        match manifest.verify() {
            Ok(true) => {}
            _ => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "merkle verification failed",
                )
                .into_response()
            }
        }

        let file_size = manifest.file_size;

        if manifest.chunks == 1 {
            let chunk_data = match transport.download(&manifest.chunk_hashes[0]).await {
                Ok(d) => d,
                Err(e) => {
                    return error_response(StatusCode::NOT_FOUND, &e.to_string()).into_response()
                }
            };
            return (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                    (header::CONTENT_LENGTH, file_size.to_string()),
                ],
                Body::from(chunk_data),
            )
                .into_response();
        }

        let stream = chunk_download_stream(transport, manifest);
        let body = Body::from_stream(stream);

        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (header::CONTENT_LENGTH, file_size.to_string()),
            ],
            body,
        )
            .into_response();
    }

    let content_length = blob_data.len().to_string();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, content_length),
        ],
        Body::from(blob_data),
    )
        .into_response()
}

fn chunk_download_stream(
    transport: Transport,
    manifest: Manifest,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::convert::Infallible>> + Send>> {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::convert::Infallible>>(4);

    tokio::spawn(async move {
        for chunk_info in manifest.all_chunk_info().unwrap_or_default() {
            match transport.download(&chunk_info.hash).await {
                Ok(data) => {
                    debug!(chunk.sha256 = %chunk_info.hash, chunk.size = data.len(), "chunk downloaded for stream");
                    if tx.send(Ok(Bytes::from(data))).await.is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = tx.send(Ok(Bytes::new())).await;
                    break;
                }
            }
        }
    });

    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

#[instrument(name = "daemon.upload", skip_all, fields(oid = %oid))]
async fn handle_upload(
    State(_state): State<Arc<DaemonState>>,
    Path((repo_b64, oid)): Path<(String, String)>,
    body: Body,
) -> impl IntoResponse {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let config = match load_config(&repo_path) {
        Ok(c) => c,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    let transport = match make_transport(&config).await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    let chunker = match Chunker::new(config.chunk_size) {
        Ok(c) => c,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    let tmp_dir = repo_path.join(".blossom-lfs-tmp");
    tokio::fs::create_dir_all(&tmp_dir).await.ok();
    let tmp_file = tmp_dir.join(format!("upload-{}", &oid[..16]));

    {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&tmp_file)
            .await
            .map_err(|e| {
                error!(error.message = %e, "failed to create temp file");
                std::io::Error::other(e)
            })
            .unwrap();

        let mut stream = body.into_data_stream();
        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap_or_default();
            if let Err(e) = file.write_all(&chunk).await {
                tokio::fs::remove_file(&tmp_file).await.ok();
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
            }
        }
        file.flush().await.ok();
    }

    let metadata = match tokio::fs::metadata(&tmp_file).await {
        Ok(m) => m,
        Err(e) => {
            tokio::fs::remove_file(&tmp_file).await.ok();
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
    };
    let file_size = metadata.len();
    let file_path = &tmp_file;

    if chunker.should_chunk(file_size) {
        match upload_chunked(&transport, &chunker, file_path, file_size).await {
            Ok(_) => {}
            Err(e) => {
                tokio::fs::remove_file(&tmp_file).await.ok();
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
            }
        }
    }

    let already_exists = transport.exists(&oid).await.unwrap_or(false);
    if !already_exists {
        let repo_slug = match derive_repo_slug(&repo_path).await {
            Ok(s) => s,
            Err(_) => "unknown".to_string(),
        };
        if let Err(e) = transport
            .upload_lfs(
                &std::fs::read(&tmp_file).unwrap_or_default(),
                "application/octet-stream",
                &oid,
                &repo_slug,
                None,
                false,
            )
            .await
        {
            tokio::fs::remove_file(&tmp_file).await.ok();
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
        info!(blob.oid = %oid, blob.size = file_size, "blob uploaded");
    } else {
        info!(blob.oid = %oid, "blob already exists, skipped upload");
    }

    tokio::fs::remove_file(&tmp_file).await.ok();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "oid": oid, "size": file_size })),
    )
}

async fn upload_chunked(
    transport: &Transport,
    chunker: &Chunker,
    file_path: &std::path::Path,
    file_size: u64,
) -> Result<()> {
    let (chunks, _) = chunker.chunk_file(file_path).await?;

    let mut chunk_hashes = Vec::new();
    for chunk in &chunks {
        let chunk_data = chunker
            .read_chunk(file_path, chunk.offset, chunk.size)
            .await?;
        let chunk_hash = hex::encode(Sha256::digest(&chunk_data));

        let already_exists = transport.exists(&chunk_hash).await.unwrap_or(false);
        if !already_exists {
            transport
                .upload(&chunk_data, "application/octet-stream")
                .await?;
            debug!(chunk.sha256 = %chunk_hash, chunk.size = chunk.size, "chunk uploaded");
        }

        chunk_hashes.push(chunk_hash);
    }

    let manifest = Manifest::new(
        file_size,
        chunker.chunk_size(),
        chunk_hashes,
        file_path.file_name().map(|n| n.to_string_lossy().into()),
        Some("application/octet-stream".to_string()),
        None,
    )?;

    let manifest_json = manifest.to_json()?;
    transport
        .upload(manifest_json.as_bytes(), "application/json")
        .await?;
    debug!(manifest.merkle_root = %manifest.merkle_root, "manifest uploaded");

    Ok(())
}

// ---------------------------------------------------------------------------
// Verify
// ---------------------------------------------------------------------------

#[instrument(name = "daemon.verify", skip_all, fields(oid = %oid))]
async fn handle_verify(
    State(_state): State<Arc<DaemonState>>,
    Path((repo_b64, oid)): Path<(String, String)>,
) -> impl IntoResponse {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let transport = match load_read_transport(&repo_path).await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };

    match transport.exists(&oid).await {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({ "oid": oid, "ok": true })),
        ),
        Ok(false) => error_response(StatusCode::NOT_FOUND, &format!("object {} not found", oid)),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Lock handlers (unchanged from v0.3.x)
// ---------------------------------------------------------------------------

#[instrument(name = "daemon.locks.create", skip_all, fields(repo_b64 = %repo_b64))]
async fn handle_create_lock(
    State(_state): State<Arc<DaemonState>>,
    Path(repo_b64): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };

    #[derive(Deserialize)]
    struct CreateReq {
        path: String,
    }

    let req: CreateReq = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid request: {}", e))
        }
    };

    match client.create_lock(&slug, &req.path).await {
        Ok(lock) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "lock": lock })),
        ),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already locked") {
                error_response(StatusCode::CONFLICT, &msg)
            } else {
                internal_error(&msg)
            }
        }
    }
}

#[instrument(name = "daemon.locks.list", skip_all, fields(repo_b64 = %repo_b64))]
async fn handle_list_locks(
    State(_state): State<Arc<DaemonState>>,
    Path(repo_b64): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };

    match client
        .list_locks(
            &slug,
            params.path.as_deref(),
            params.cursor.as_deref(),
            params.limit,
        )
        .await
    {
        Ok((locks, next_cursor)) => {
            let mut resp = serde_json::json!({ "locks": locks });
            if let Some(cursor) = next_cursor {
                resp["next_cursor"] = serde_json::Value::String(cursor);
            }
            (StatusCode::OK, Json(resp))
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

#[instrument(name = "daemon.locks.verify", skip_all, fields(repo_b64 = %repo_b64))]
async fn handle_verify_locks(
    State(_state): State<Arc<DaemonState>>,
    Path(repo_b64): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };

    #[derive(Deserialize, Default)]
    struct VerifyReq {
        cursor: Option<String>,
        limit: Option<u32>,
    }

    let req: VerifyReq = serde_json::from_slice(&body).unwrap_or_default();

    match client
        .verify_locks(&slug, req.cursor.as_deref(), req.limit)
        .await
    {
        Ok((ours, theirs, next_cursor)) => {
            let mut resp = serde_json::json!({ "ours": ours, "theirs": theirs });
            if let Some(cursor) = next_cursor {
                resp["next_cursor"] = serde_json::Value::String(cursor);
            }
            (StatusCode::OK, Json(resp))
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

#[instrument(name = "daemon.locks.unlock", skip_all, fields(repo_b64 = %repo_b64))]
async fn handle_unlock(
    State(_state): State<Arc<DaemonState>>,
    Path((repo_b64, lock_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };

    #[derive(Deserialize, Default)]
    struct UnlockReq {
        #[serde(default)]
        force: bool,
    }

    let req: UnlockReq = serde_json::from_slice(&body).unwrap_or_default();

    match client.unlock(&slug, &lock_id, req.force).await {
        Ok(lock) => (StatusCode::OK, Json(serde_json::json!({ "lock": lock }))),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                error_response(StatusCode::NOT_FOUND, &msg)
            } else if msg.contains("forbidden") || msg.contains("owner") {
                error_response(StatusCode::FORBIDDEN, &msg)
            } else {
                internal_error(&msg)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ListParams {
    path: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}
