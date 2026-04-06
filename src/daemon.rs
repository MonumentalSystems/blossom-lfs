use anyhow::{Context as _, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use base64::Engine;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, instrument, warn};

use crate::config::Config;
use crate::lock_client::LockClient;

#[derive(Clone)]
struct DaemonState {}

pub async fn run_daemon(port: u16) -> Result<()> {
    let state = DaemonState {};

    let app = Router::new()
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

fn error_json(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "message": msg })),
    )
}

async fn load_client(repo_path: &std::path::Path) -> Result<(Config, String, LockClient)> {
    let config = Config::from_repo_path(repo_path)?;

    let repo_slug = derive_repo_slug(repo_path).await?;

    let client = LockClient::new(config.server_url.clone(), config.secret_key_hex.clone());

    Ok((config, repo_slug, client))
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

#[instrument(name = "daemon.locks.create", skip_all, fields(repo_b64 = %repo_b64))]
async fn handle_create_lock(
    State(_state): State<Arc<DaemonState>>,
    Path(repo_b64): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let repo_path = match decode_repo_path(&repo_b64) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "message": e.to_string() })),
            );
        }
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return error_json(&e.to_string()),
    };

    #[derive(Deserialize)]
    struct CreateReq {
        path: String,
    }

    let req: CreateReq = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "message": format!("invalid request: {}", e) })),
            );
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
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({ "message": msg })),
                )
            } else {
                error!(error.message = %msg, "lock create failed");
                error_json(&msg)
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
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "message": e.to_string() })),
            );
        }
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return error_json(&e.to_string()),
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
        Err(e) => {
            error!(error.message = %e, "lock list failed");
            error_json(&e.to_string())
        }
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
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "message": e.to_string() })),
            );
        }
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return error_json(&e.to_string()),
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
        Err(e) => {
            error!(error.message = %e, "lock verify failed");
            error_json(&e.to_string())
        }
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
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "message": e.to_string() })),
            );
        }
    };

    let (_, slug, client) = match load_client(&repo_path).await {
        Ok(v) => v,
        Err(e) => return error_json(&e.to_string()),
    };

    #[derive(Deserialize, Default)]
    struct UnlockReq {
        #[serde(default)]
        force: bool,
    }

    let req: UnlockReq = serde_json::from_slice(&body).unwrap_or_default();

    match client.unlock(&slug, &lock_id, req.force).await {
        Ok(lock) => (
            StatusCode::OK,
            Json(serde_json::json!({ "lock": lock })),
        ),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "message": msg })),
                )
            } else if msg.contains("forbidden") || msg.contains("owner") {
                (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "message": msg })),
                )
            } else {
                error!(error.message = %msg, "unlock failed");
                error_json(&msg)
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
