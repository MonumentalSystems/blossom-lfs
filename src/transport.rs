//! Pluggable transport layer for blob operations.
//!
//! [`Transport`] wraps the blossom-rs [`BlobClient`] trait, dispatching to
//! either the HTTP [`BlossomClient`] or the iroh QUIC
//! [`IrohBlossomClient`] depending on the configured transport mode.
//!
//! Each variant stores both the client and its address (the `BlobClient`
//! associated type): `()` for HTTP, `EndpointAddr` for iroh.
//!
//! ## Configuration
//!
//! ```ini
//! [lfs-dal]
//!     server = https://blossom.example.com   # HTTP (default)
//!     # — or —
//!     server = <iroh-endpoint-id>            # iroh QUIC
//!     transport = iroh
//! ```

use crate::error::{BlossomLfsError, Result};
use blossom_rs::{BlobClient, protocol::BlobDescriptor};

/// Transport wrapping either HTTP or iroh QUIC blob operations.
///
/// Uses the [`BlobClient`] trait from blossom-rs for a unified interface.
/// HTTP uses `Address = ()` (server list is internal to the client),
/// iroh uses `Address = EndpointAddr` (stored here alongside the client).
pub enum Transport {
    /// Standard HTTPS via [`blossom_rs::BlossomClient`].
    Http(blossom_rs::BlossomClient),

    /// iroh QUIC via [`blossom_rs::transport::IrohBlossomClient`].
    #[cfg(feature = "iroh")]
    Iroh {
        client: blossom_rs::transport::IrohBlossomClient,
        peer: iroh::EndpointAddr,
    },
}

impl Transport {
    /// Create an HTTP transport.
    pub fn http(
        server_url: String,
        signer: impl blossom_rs::auth::BlossomSigner + 'static,
        timeout: std::time::Duration,
    ) -> Self {
        Self::Http(blossom_rs::BlossomClient::with_timeout(
            vec![server_url],
            signer,
            timeout,
        ))
    }

    /// Create an iroh QUIC transport.
    ///
    /// `endpoint_id` is the remote peer's base32-encoded iroh endpoint ID.
    #[cfg(feature = "iroh")]
    pub fn iroh(
        endpoint: iroh::endpoint::Endpoint,
        signer: impl blossom_rs::auth::BlossomSigner + 'static,
        endpoint_id: &str,
    ) -> std::result::Result<Self, String> {
        let eid: iroh::EndpointId = endpoint_id
            .parse()
            .map_err(|e| format!("invalid iroh endpoint ID '{}': {}", endpoint_id, e))?;

        Ok(Self::Iroh {
            client: blossom_rs::transport::IrohBlossomClient::new(endpoint, signer),
            peer: iroh::EndpointAddr::from(eid),
        })
    }

    /// Upload a blob via the [`BlobClient`] trait.
    pub async fn upload(&self, data: &[u8], content_type: &str) -> Result<BlobDescriptor> {
        match self {
            Self::Http(c) => BlobClient::upload(c, &(), data, content_type).await,
            #[cfg(feature = "iroh")]
            Self::Iroh { client, peer } => BlobClient::upload(client, peer, data, content_type).await,
        }
        .map_err(BlossomLfsError::Blossom)
    }

    /// Download a blob by SHA-256 hex hash via the [`BlobClient`] trait.
    pub async fn download(&self, sha256: &str) -> Result<Vec<u8>> {
        match self {
            Self::Http(c) => BlobClient::download(c, &(), sha256).await,
            #[cfg(feature = "iroh")]
            Self::Iroh { client, peer } => BlobClient::download(client, peer, sha256).await,
        }
        .map_err(BlossomLfsError::Blossom)
    }

    /// Check whether a blob exists via the [`BlobClient`] trait.
    pub async fn exists(&self, sha256: &str) -> Result<bool> {
        match self {
            Self::Http(c) => BlobClient::exists(c, &(), sha256).await,
            #[cfg(feature = "iroh")]
            Self::Iroh { client, peer } => BlobClient::exists(client, peer, sha256).await,
        }
        .map_err(BlossomLfsError::Blossom)
    }
}
