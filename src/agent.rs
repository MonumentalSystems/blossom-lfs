//! Git LFS custom transfer agent.
//!
//! Implements the Git LFS
//! [custom transfer protocol](https://github.com/git-lfs/git-lfs/blob/main/docs/custom-transfers.md),
//! reading JSON requests from stdin and writing responses to stdout. Each
//! upload/download is spawned as an async task so multiple transfers can
//! proceed concurrently.

use crate::{
    chunking::{ChunkAssembler, Chunker, Manifest},
    config::{Config, Transport as TransportMode},
    error::{BlossomLfsError, Result},
    protocol::{InitResponse, ProgressResponse, TransferResponse},
    transport::Transport,
};
use anyhow::Context as _;
use blossom_rs::auth::Signer;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, instrument, warn, Instrument, Span};

const TEMP_DIR: &str = ".blossom-lfs-tmp";

/// The main transfer agent that handles Git LFS requests.
///
/// Created once per process and reused across all transfers. Wraps a
/// [`Transport`] for server communication and a [`Chunker`] for splitting
/// large files. The transport is selected based on the `transport` config
/// option (HTTP by default, iroh QUIC when enabled).
pub struct Agent {
    config: Config,
    transport: Arc<Transport>,
    sender: tokio::sync::mpsc::Sender<String>,
    tasks: tokio::task::JoinSet<()>,
    chunker: Chunker,
    temp_dir: PathBuf,
}

impl Agent {
    /// Create a new agent from the given configuration.
    ///
    /// Initialises the appropriate transport (HTTP or iroh) and the
    /// [`Chunker`]. Responses are sent through `sender`.
    pub async fn new(config: Config, sender: tokio::sync::mpsc::Sender<String>) -> Result<Self> {
        let signer = Signer::from_secret_hex(&config.secret_key_hex)
            .map_err(|e| BlossomLfsError::Config(format!("Failed to create signer: {}", e)))?;

        let transport = match config.transport {
            TransportMode::Http => Transport::http(
                config.server_url.clone(),
                signer,
                std::time::Duration::from_secs(300),
            ),
            TransportMode::Iroh => {
                #[cfg(feature = "iroh")]
                {
                    create_iroh_transport(&config, signer).await?
                }
                #[cfg(not(feature = "iroh"))]
                {
                    return Err(BlossomLfsError::Config(
                        "iroh transport requested but the 'iroh' feature is not enabled. \
                         Rebuild with: cargo build --features iroh"
                            .to_string(),
                    ));
                }
            }
        };
        let transport = Arc::new(transport);

        let chunker = Chunker::new(config.chunk_size)?;
        let temp_dir = PathBuf::from(TEMP_DIR);

        Ok(Self {
            config,
            transport,
            sender,
            tasks: tokio::task::JoinSet::new(),
            chunker,
            temp_dir,
        })
    }

    /// Dispatch a single JSON-line request from Git LFS.
    ///
    /// Supported events: `init`, `upload`, `download`, `terminate`.
    #[instrument(name = "lfs.agent.process", skip_all)]
    pub async fn process(&mut self, request: &str) -> Result<()> {
        debug!(raw_request = %request, "processing LFS request");
        let request: crate::protocol::Request =
            serde_json::from_str(request).context("invalid request")?;

        match request {
            crate::protocol::Request::Init => self.init().await,
            crate::protocol::Request::Upload { oid, path } => self.upload(oid, path).await,
            crate::protocol::Request::Download { oid } => self.download(oid).await,
            crate::protocol::Request::Terminate => self.terminate().await,
        };

        Ok(())
    }

    async fn init(&mut self) {
        send_response(&self.sender, InitResponse::new().json()).await;
    }

    async fn upload(&mut self, oid: String, path: String) {
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        let sender = self.sender.clone();
        let chunker = self.chunker.clone();

        self.tasks.spawn(async move {
            let span = tracing::info_span!("lfs.upload", blob.oid = %oid, blob.size = tracing::field::Empty, blob.chunked = tracing::field::Empty);
            let status: Result<Option<String>> = async {
                let file_path = PathBuf::from(&path);
                let metadata = tokio::fs::metadata(&file_path)
                    .await
                    .context("Failed to read file metadata")?;
                let file_size = metadata.len();
                Span::current().record("blob.size", file_size);

                let chunked = chunker.should_chunk(file_size);
                Span::current().record("blob.chunked", chunked);

                if chunked {
                    upload_chunked_file(
                        &transport, &config, &chunker, &file_path, file_size, &sender, &oid,
                    )
                    .await?;
                }

                // Upload the complete file so it's retrievable by OID,
                // but skip if it already exists on the server
                let already_exists = transport
                    .exists(&oid)
                    .await
                    .unwrap_or(false);

                if !already_exists {
                    let data = tokio::fs::read(&file_path)
                        .await
                        .context("Failed to read file")?;

                    transport
                        .upload(&data, "application/octet-stream")
                        .await?;

                    info!(blob.oid = %oid, blob.size = file_size, "blob uploaded");
                } else {
                    info!(blob.oid = %oid, "blob already exists, skipped upload");
                }

                send_progress(
                    &sender,
                    &oid,
                    file_size as usize,
                    file_size as usize,
                    file_size as usize,
                )
                .await;

                Ok(None)
            }
            .instrument(span)
            .await;

            send_response(&sender, TransferResponse::new(oid, status).json()).await;
        });
    }

    async fn download(&mut self, oid: String) {
        let transport = Arc::clone(&self.transport);
        let sender = self.sender.clone();
        let temp_dir = self.temp_dir.clone();
        let config = self.config.clone();
        let output_path = lfs_object_path(&oid);

        self.tasks.spawn(async move {
            let span = tracing::info_span!("lfs.download", blob.oid = %oid, blob.size = tracing::field::Empty, blob.chunked = tracing::field::Empty);
            let status: Result<Option<String>> = async {
                send_progress(&sender, &oid, 0, 0, 0).await;

                let blob_data = transport
                    .download(&oid)
                    .await?;

                tokio::fs::create_dir_all(output_path.parent().unwrap())
                    .await
                    .context("Failed to create output directory")?;

                // Try to parse as manifest; if it fails, treat as raw blob
                let manifest_result = std::str::from_utf8(&blob_data)
                    .ok()
                    .and_then(|s| Manifest::from_json(s).ok());

                if let Some(manifest) = manifest_result {
                    if !manifest.verify()? {
                        warn!(blob.oid = %oid, "merkle tree verification failed");
                        return Err(BlossomLfsError::MerkleVerificationFailed);
                    }

                    let total_size = manifest.file_size as usize;
                    Span::current().record("blob.size", total_size as u64);
                    Span::current().record("blob.chunked", true);
                    send_progress(&sender, &oid, 0, total_size, 0).await;

                    if manifest.chunks == 1 {
                        let chunk_data = transport
                            .download(&manifest.chunk_hashes[0])
                            .await?;

                        tokio::fs::write(&output_path, &chunk_data)
                            .await
                            .context("Failed to write file")?;

                        send_progress(&sender, &oid, total_size, total_size, total_size).await;
                    } else {
                        download_chunked_file(
                            &transport,
                            &config,
                            &manifest,
                            &output_path,
                            &sender,
                            &oid,
                            &temp_dir,
                        )
                        .await
                        .context("Failed to download chunked file")?;
                    }

                    info!(blob.oid = %oid, blob.size = total_size, blob.chunks = manifest.chunks, "chunked blob downloaded");
                } else {
                    // Raw blob — write directly
                    let total_size = blob_data.len();
                    Span::current().record("blob.size", total_size as u64);
                    Span::current().record("blob.chunked", false);
                    send_progress(&sender, &oid, 0, total_size, 0).await;

                    tokio::fs::write(&output_path, &blob_data)
                        .await
                        .context("Failed to write file")?;

                    send_progress(&sender, &oid, total_size, total_size, total_size).await;
                    info!(blob.oid = %oid, blob.size = total_size, "blob downloaded");
                }

                Ok(Some(output_path.to_string_lossy().into()))
            }
            .instrument(span)
            .await;

            send_response(&sender, TransferResponse::new(oid, status).json()).await;
        });
    }

    async fn terminate(&mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

/// Create an iroh transport from the config.
///
/// `server_url` is parsed as an iroh endpoint ID (base32-encoded).
#[cfg(feature = "iroh")]
async fn create_iroh_transport(config: &Config, signer: Signer) -> Result<Transport> {
    let endpoint: iroh::endpoint::Endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
        .await
        .map_err(|e| BlossomLfsError::Config(format!("failed to create iroh endpoint: {}", e)))?;

    info!(iroh.endpoint_id = %config.server_url, "connecting via iroh QUIC");

    Transport::iroh(endpoint, signer, &config.server_url).map_err(BlossomLfsError::Config)
}

#[instrument(name = "lfs.upload.chunked", skip_all, fields(blob.oid = %oid, blob.size = file_size, blob.chunks = tracing::field::Empty, chunks.skipped = tracing::field::Empty))]
async fn upload_chunked_file(
    transport: &Transport,
    _config: &Config,
    chunker: &Chunker,
    file_path: &Path,
    file_size: u64,
    sender: &tokio::sync::mpsc::Sender<String>,
    oid: &str,
) -> Result<Vec<String>> {
    let (chunks, _) = chunker.chunk_file(file_path).await?;
    Span::current().record("blob.chunks", chunks.len());

    let mut bytes_so_far = 0usize;
    let mut chunk_hashes = Vec::new();
    let mut skipped = 0u32;

    for chunk in &chunks {
        let chunk_data = chunker
            .read_chunk(file_path, chunk.offset, chunk.size)
            .await?;

        let chunk_hash = hash_data(&chunk_data);

        // Skip upload if this chunk already exists on the server
        let already_exists = transport.exists(&chunk_hash).await.unwrap_or(false);

        if !already_exists {
            transport
                .upload(&chunk_data, "application/octet-stream")
                .await?;
            debug!(chunk.sha256 = %chunk_hash, chunk.size = chunk.size, chunk.index = chunk.index, "chunk uploaded");
        } else {
            skipped += 1;
            debug!(chunk.sha256 = %chunk_hash, chunk.index = chunk.index, "chunk already exists, skipped");
        }

        chunk_hashes.push(chunk_hash);
        bytes_so_far += chunk.size;

        send_progress(sender, oid, bytes_so_far, file_size as usize, chunk.size).await;
    }

    Span::current().record("chunks.skipped", skipped);
    info!(
        blob.chunks = chunks.len(),
        chunks.skipped = skipped,
        "chunked upload complete"
    );

    Ok(chunk_hashes)
}

#[instrument(name = "lfs.download.chunked", skip_all, fields(blob.oid = %oid, blob.size = manifest.file_size, blob.chunks = manifest.chunks))]
async fn download_chunked_file(
    transport: &Transport,
    _config: &Config,
    manifest: &Manifest,
    output_path: &Path,
    sender: &tokio::sync::mpsc::Sender<String>,
    oid: &str,
    temp_dir: &Path,
) -> Result<()> {
    let assembler = ChunkAssembler::new(temp_dir.to_path_buf());

    let mut bytes_so_far = 0usize;
    let total_size = manifest.file_size as usize;

    for chunk_info in manifest.all_chunk_info()? {
        let chunk_data = transport.download(&chunk_info.hash).await?;

        assembler
            .write_chunk(oid, chunk_info.index, &chunk_data)
            .await?;

        debug!(chunk.sha256 = %chunk_info.hash, chunk.size = chunk_info.size, chunk.index = chunk_info.index, "chunk downloaded");

        bytes_so_far += chunk_info.size;
        send_progress(sender, oid, bytes_so_far, total_size, chunk_info.size).await;
    }

    assembler
        .assemble(oid, output_path, manifest.chunks)
        .await?;

    assembler.cleanup(oid).await?;

    info!(blob.chunks = manifest.chunks, "chunked download assembled");

    Ok(())
}

fn hash_data(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

async fn send_response(sender: &tokio::sync::mpsc::Sender<String>, msg: String) {
    debug!(lfs.response = %msg, "sending LFS response");
    let _ = sender.send(msg).await;
}

async fn send_progress(
    sender: &tokio::sync::mpsc::Sender<String>,
    oid: &str,
    bytes_so_far: usize,
    total_bytes: usize,
    bytes_since_last: usize,
) {
    send_response(
        sender,
        ProgressResponse::new(oid.to_string(), bytes_so_far, total_bytes, bytes_since_last).json(),
    )
    .await;
}

fn lfs_object_path(oid: &str) -> PathBuf {
    PathBuf::from(".git/lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(oid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_hash_data() {
        let hash = hash_data(b"test");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_lfs_object_path() {
        let path = lfs_object_path("abc123");
        assert!(path.to_str().unwrap().contains(".git/lfs/objects"));
    }
}
