use crate::{
    blossom::{auth::AuthToken, types::BlobDescriptor},
    error::{BlossomLfsError, Result},
};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE},
    Client, StatusCode,
};

#[derive(Clone)]
pub struct BlossomClient {
    client: Client,
    server_url: String,
}

impl BlossomClient {
    pub fn new(server_url: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| BlossomLfsError::Http(e.to_string()))?;

        Ok(Self { client, server_url })
    }

    pub async fn check_upload_requirements(
        &self,
        sha256: &str,
        size: u64,
        content_type: Option<&str>,
        auth_token: Option<&AuthToken>,
    ) -> Result<()> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-SHA-256",
            HeaderValue::from_str(sha256).map_err(|e| BlossomLfsError::Http(e.to_string()))?,
        );
        headers.insert(
            "X-Content-Length",
            HeaderValue::from_str(&size.to_string())
                .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
        );

        if let Some(ct) = content_type {
            headers.insert(
                "X-Content-Type",
                HeaderValue::from_str(ct).map_err(|e| BlossomLfsError::Http(e.to_string()))?,
            );
        }

        if let Some(token) = auth_token {
            let auth_header = token.to_authorization_header()?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_header)
                    .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
            );
        }

        let response = self
            .client
            .head(format!("{}/upload", self.server_url))
            .headers(headers)
            .send()
            .await
            .map_err(|e| BlossomLfsError::Http(e.to_string()))?;

        match response.status() {
            StatusCode::OK => Ok(()),
            status => {
                let reason = response
                    .headers()
                    .get("X-Reason")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Unknown error");
                Err(BlossomLfsError::ServerError(format!(
                    "{}: {}",
                    status, reason
                )))
            }
        }
    }

    pub async fn upload_blob(
        &self,
        data: Vec<u8>,
        sha256: &str,
        content_type: Option<&str>,
        auth_token: Option<&AuthToken>,
    ) -> Result<BlobDescriptor> {
        let mut headers = HeaderMap::new();

        if let Some(ct) = content_type {
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_str(ct).map_err(|e| BlossomLfsError::Http(e.to_string()))?,
            );
        }

        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&data.len().to_string())
                .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
        );

        headers.insert(
            "X-SHA-256",
            HeaderValue::from_str(sha256).map_err(|e| BlossomLfsError::Http(e.to_string()))?,
        );

        if let Some(token) = auth_token {
            let auth_header = token.to_authorization_header()?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_header)
                    .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
            );
        }

        let response = self
            .client
            .put(format!("{}/upload", self.server_url))
            .headers(headers)
            .body(data)
            .send()
            .await
            .map_err(|e| BlossomLfsError::Http(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let descriptor: BlobDescriptor = response
                    .json()
                    .await
                    .map_err(|e| BlossomLfsError::Http(e.to_string()))?;
                Ok(descriptor)
            }
            status => {
                let reason = response
                    .headers()
                    .get("X-Reason")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Unknown error");
                Err(BlossomLfsError::ServerError(format!(
                    "{}: {}",
                    status, reason
                )))
            }
        }
    }

    pub async fn has_blob(&self, sha256: &str, auth_token: Option<&AuthToken>) -> Result<bool> {
        let mut headers = HeaderMap::new();

        if let Some(token) = auth_token {
            let auth_header = token.to_authorization_header()?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_header)
                    .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
            );
        }

        let response = self
            .client
            .head(format!("{}/{}", self.server_url, sha256))
            .headers(headers)
            .send()
            .await
            .map_err(|e| BlossomLfsError::Http(e.to_string()))?;

        Ok(response.status() == StatusCode::OK)
    }

    pub async fn download_blob(
        &self,
        sha256: &str,
        auth_token: Option<&AuthToken>,
    ) -> Result<Vec<u8>> {
        let mut headers = HeaderMap::new();

        if let Some(token) = auth_token {
            let auth_header = token.to_authorization_header()?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_header)
                    .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
            );
        }

        let response = self
            .client
            .get(format!("{}/{}", self.server_url, sha256))
            .headers(headers)
            .send()
            .await
            .map_err(|e| BlossomLfsError::Http(e.to_string()))?;

        match response.status() {
            StatusCode::OK => response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| BlossomLfsError::Http(e.to_string())),
            StatusCode::NOT_FOUND => Err(BlossomLfsError::ManifestNotFound(sha256.to_string())),
            status => {
                let reason = response
                    .headers()
                    .get("X-Reason")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Unknown error");
                Err(BlossomLfsError::ServerError(format!(
                    "{}: {}",
                    status, reason
                )))
            }
        }
    }

    pub async fn download_blob_range(
        &self,
        sha256: &str,
        start: u64,
        end: u64,
        auth_token: Option<&AuthToken>,
    ) -> Result<Vec<u8>> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Range",
            HeaderValue::from_str(&format!("bytes={}-{}", start, end))
                .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
        );

        if let Some(token) = auth_token {
            let auth_header = token.to_authorization_header()?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_header)
                    .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
            );
        }

        let response = self
            .client
            .get(format!("{}/{}", self.server_url, sha256))
            .headers(headers)
            .send()
            .await
            .map_err(|e| BlossomLfsError::Http(e.to_string()))?;

        match response.status() {
            StatusCode::PARTIAL_CONTENT => response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| BlossomLfsError::Http(e.to_string())),
            StatusCode::OK => response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| BlossomLfsError::Http(e.to_string())),
            status => {
                let reason = response
                    .headers()
                    .get("X-Reason")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Unknown error");
                Err(BlossomLfsError::ServerError(format!(
                    "{}: {}",
                    status, reason
                )))
            }
        }
    }

    pub async fn delete_blob(&self, sha256: &str, auth_token: &AuthToken) -> Result<()> {
        let mut headers = HeaderMap::new();
        let auth_header = auth_token.to_authorization_header()?;
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_header)
                .map_err(|e| BlossomLfsError::Http(e.to_string()))?,
        );

        let response = self
            .client
            .delete(format!("{}/{}", self.server_url, sha256))
            .headers(headers)
            .send()
            .await
            .map_err(|e| BlossomLfsError::Http(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            status => {
                let reason = response
                    .headers()
                    .get("X-Reason")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Unknown error");
                Err(BlossomLfsError::ServerError(format!(
                    "{}: {}",
                    status, reason
                )))
            }
        }
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = BlossomClient::new("https://cdn.example.com".to_string()).unwrap();
        assert_eq!(client.server_url(), "https://cdn.example.com");
    }
}
