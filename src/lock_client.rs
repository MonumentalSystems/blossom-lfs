use anyhow::{Context as _, Result};
use blossom_rs::auth::{auth_header_value, build_blossom_auth, Signer};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LfsLock {
    pub id: String,
    pub path: String,
    pub locked_at: String,
    pub owner: LfsOwner,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LfsOwner {
    pub name: String,
}

#[derive(Debug, Serialize)]
struct CreateLockRequest {
    path: String,
}

#[derive(Debug, Deserialize)]
struct LockResponse {
    lock: LfsLock,
}

#[derive(Debug, Serialize)]
struct UnlockRequest {
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct LockListResponse {
    locks: Vec<LfsLock>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct VerifyRequest {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct VerifyResponse {
    ours: Vec<LfsLock>,
    theirs: Vec<LfsLock>,
    #[serde(default)]
    next_cursor: Option<String>,
}

pub struct LockClient {
    http: reqwest::Client,
    server_url: String,
    secret_key_hex: String,
}

impl LockClient {
    pub fn new(server_url: String, secret_key_hex: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            server_url,
            secret_key_hex,
        }
    }

    fn auth_header(&self, action: &str) -> Result<String> {
        let signer = Signer::from_secret_hex(&self.secret_key_hex)
            .map_err(|e| anyhow::anyhow!("invalid secret key: {}", e))?;
        let event = build_blossom_auth(&signer, action, None, None, "");
        Ok(auth_header_value(&event))
    }

    pub async fn create_lock(&self, repo_slug: &str, path: &str) -> Result<LfsLock> {
        let url = format!("{}/lfs/{}/locks", self.server_url, repo_slug);
        let auth = self.auth_header("lock")?;

        let resp = self
            .http
            .post(&url)
            .header("Authorization", auth)
            .header("Accept", "application/vnd.git-lfs+json")
            .json(&CreateLockRequest {
                path: path.to_string(),
            })
            .send()
            .await
            .context("lock create request failed")?;

        let status = resp.status();
        let body = resp.text().await.context("lock create response body")?;

        if status == reqwest::StatusCode::CREATED {
            let lock_resp: LockResponse =
                serde_json::from_str(&body).context("parse lock create response")?;
            Ok(lock_resp.lock)
        } else if status == reqwest::StatusCode::CONFLICT {
            let lock_resp: LockResponse =
                serde_json::from_str(&body).context("parse lock conflict response")?;
            anyhow::bail!("path already locked by {}", lock_resp.lock.owner.name)
        } else {
            anyhow::bail!("lock create failed: {} - {}", status, body)
        }
    }

    pub async fn unlock(&self, repo_slug: &str, lock_id: &str, force: bool) -> Result<LfsLock> {
        let url = format!(
            "{}/lfs/{}/locks/{}/unlock",
            self.server_url, repo_slug, lock_id
        );
        let auth = self.auth_header("lock")?;

        let resp = self
            .http
            .post(&url)
            .header("Authorization", auth)
            .header("Accept", "application/vnd.git-lfs+json")
            .json(&UnlockRequest { force })
            .send()
            .await
            .context("unlock request failed")?;

        let status = resp.status();
        let body = resp.text().await.context("unlock response body")?;

        if status == reqwest::StatusCode::OK {
            let lock_resp: LockResponse =
                serde_json::from_str(&body).context("parse unlock response")?;
            Ok(lock_resp.lock)
        } else {
            anyhow::bail!("unlock failed: {} - {}", status, body)
        }
    }

    pub async fn list_locks(
        &self,
        repo_slug: &str,
        path: Option<&str>,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<LfsLock>, Option<String>)> {
        let mut url = format!("{}/lfs/{}/locks", self.server_url, repo_slug);
        let mut params = Vec::new();
        if let Some(p) = path {
            params.push(format!("path={}", urlencoding::encode(p)));
        }
        if let Some(c) = cursor {
            params.push(format!("cursor={}", urlencoding::encode(c)));
        }
        if let Some(l) = limit {
            params.push(format!("limit={}", l));
        }
        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }

        let auth = self.auth_header("lock")?;

        let resp = self
            .http
            .get(&url)
            .header("Authorization", auth)
            .header("Accept", "application/vnd.git-lfs+json")
            .send()
            .await
            .context("lock list request failed")?;

        let status = resp.status();
        let body = resp.text().await.context("lock list response body")?;

        if status == reqwest::StatusCode::OK {
            let list_resp: LockListResponse =
                serde_json::from_str(&body).context("parse lock list response")?;
            Ok((list_resp.locks, list_resp.next_cursor))
        } else {
            anyhow::bail!("lock list failed: {} - {}", status, body)
        }
    }

    pub async fn verify_locks(
        &self,
        repo_slug: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<LfsLock>, Vec<LfsLock>, Option<String>)> {
        let url = format!("{}/lfs/{}/locks/verify", self.server_url, repo_slug);
        let auth = self.auth_header("lock")?;

        let resp = self
            .http
            .post(&url)
            .header("Authorization", auth)
            .header("Accept", "application/vnd.git-lfs+json")
            .json(&VerifyRequest {
                cursor: cursor.map(String::from),
                limit,
            })
            .send()
            .await
            .context("lock verify request failed")?;

        let status = resp.status();
        let body = resp.text().await.context("lock verify response body")?;

        if status == reqwest::StatusCode::OK {
            let verify_resp: VerifyResponse =
                serde_json::from_str(&body).context("parse verify response")?;
            Ok((
                verify_resp.ours,
                verify_resp.theirs,
                verify_resp.next_cursor,
            ))
        } else if status == reqwest::StatusCode::NOT_FOUND {
            Ok((vec![], vec![], None))
        } else {
            anyhow::bail!("lock verify failed: {} - {}", status, body)
        }
    }
}
