//! Transport layer wrapping blossom-rs [`MultiTransportClient`].
//!
//! When both HTTP and iroh endpoints are configured, the daemon uses iroh for
//! uploads (direct P2P) and HTTP for downloads (CDN caching), with automatic
//! fallback. Set `transport = http` or `transport = iroh` to force one mode.
//!
//! ## Configuration
//!
//! In `.lfsdalconfig`:
//!
//! ```ini
//! server = https://blossom.example.com       # HTTP (required)
//! iroh-endpoint = <iroh-endpoint-id>          # iroh QUIC (optional)
//! # transport = http                          # force HTTP for all ops (optional)
//! ```

use crate::error::{BlossomLfsError, Result};
use blossom_rs::protocol::BlobDescriptor;
use blossom_rs::BlobClient;

pub struct Transport {
    client: blossom_rs::MultiTransportClient,
}

impl Transport {
    /// Create an HTTP-only transport.
    pub fn http_only(
        server_url: String,
        signer: impl blossom_rs::auth::BlossomSigner + 'static,
        timeout: std::time::Duration,
    ) -> Self {
        let http = blossom_rs::BlossomClient::with_timeout(vec![server_url], signer, timeout);
        Self {
            client: blossom_rs::MultiTransportClient::http_only(http),
        }
    }

    /// Create a dual-transport client (iroh for uploads, HTTP for downloads).
    #[cfg(feature = "iroh")]
    pub fn multi(
        server_url: String,
        signer: impl blossom_rs::auth::BlossomSigner + 'static,
        timeout: std::time::Duration,
        iroh_client: blossom_rs::transport::IrohBlossomClient,
        iroh_peer: iroh::EndpointAddr,
    ) -> Self {
        let http = blossom_rs::BlossomClient::with_timeout(vec![server_url], signer, timeout);
        Self {
            client: blossom_rs::MultiTransportClient::new(http, iroh_client, iroh_peer),
        }
    }

    /// Force all operations through HTTP.
    pub fn force_http(mut self) -> Self {
        self.client = self.client.force_http();
        self
    }

    /// Force all operations through iroh.
    #[cfg(feature = "iroh")]
    pub fn force_iroh(mut self) -> Self {
        self.client = self.client.iroh_only();
        self
    }

    pub async fn upload(&self, data: &[u8], content_type: &str) -> Result<BlobDescriptor> {
        self.client
            .upload(&(), data, content_type)
            .await
            .map_err(BlossomLfsError::Blossom)
    }

    pub async fn download(&self, sha256: &str) -> Result<Vec<u8>> {
        self.client
            .download(&(), sha256)
            .await
            .map_err(BlossomLfsError::Blossom)
    }

    pub async fn exists(&self, sha256: &str) -> Result<bool> {
        self.client
            .exists(&(), sha256)
            .await
            .map_err(BlossomLfsError::Blossom)
    }

    pub async fn upload_file(
        &self,
        path: &std::path::Path,
        content_type: &str,
    ) -> Result<BlobDescriptor> {
        self.client
            .upload_file(&(), path, content_type)
            .await
            .map_err(BlossomLfsError::Blossom)
    }

    pub async fn upload_lfs(
        &self,
        data: &[u8],
        content_type: &str,
        path: &str,
        repo: &str,
        base_sha256: Option<&str>,
        is_manifest: bool,
    ) -> Result<BlobDescriptor> {
        self.client
            .http()
            .upload_lfs(data, content_type, path, repo, base_sha256, is_manifest)
            .await
            .map_err(BlossomLfsError::Blossom)
    }
}
