use crate::{
    chunking::{ChunkAssembler, Chunker, Manifest},
    config::Config,
    error::{BlossomLfsError, Result},
    protocol::{InitResponse, ProgressResponse, TransferResponse},
};
use anyhow::Context as _;
use blossom_rs::{auth::Signer, BlossomClient};
use log::debug;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TEMP_DIR: &str = ".blossom-lfs-tmp";

pub struct Agent {
    config: Config,
    client: Arc<BlossomClient>,
    sender: tokio::sync::mpsc::Sender<String>,
    tasks: tokio::task::JoinSet<()>,
    chunker: Chunker,
    temp_dir: PathBuf,
}

impl Agent {
    pub fn new(config: Config, sender: tokio::sync::mpsc::Sender<String>) -> Result<Self> {
        let signer = Signer::from_secret_hex(&config.secret_key_hex)
            .map_err(|e| BlossomLfsError::Config(format!("Failed to create signer: {}", e)))?;
        let client = Arc::new(BlossomClient::with_timeout(
            vec![config.server_url.clone()],
            signer,
            std::time::Duration::from_secs(300),
        ));
        let chunker = Chunker::new(config.chunk_size)?;
        let temp_dir = PathBuf::from(TEMP_DIR);

        Ok(Self {
            config,
            client,
            sender,
            tasks: tokio::task::JoinSet::new(),
            chunker,
            temp_dir,
        })
    }

    pub async fn process(&mut self, request: &str) -> Result<()> {
        debug!("request: {}", request);
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
        let client = Arc::clone(&self.client);
        let sender = self.sender.clone();
        let chunker = self.chunker.clone();

        self.tasks.spawn(async move {
            let status: Result<Option<String>> = async {
                let file_path = PathBuf::from(&path);
                let metadata = tokio::fs::metadata(&file_path)
                    .await
                    .context("Failed to read file metadata")?;
                let file_size = metadata.len();

                if chunker.should_chunk(file_size) {
                    upload_chunked_file(
                        &client, &config, &chunker, &file_path, file_size, &sender, &oid,
                    )
                    .await?;
                }

                // Upload the complete file so it's retrievable by OID,
                // but skip if it already exists on the server
                let already_exists = client
                    .exists(&oid)
                    .await
                    .unwrap_or(false);

                if !already_exists {
                    let data = tokio::fs::read(&file_path)
                        .await
                        .context("Failed to read file")?;

                    client
                        .upload(&data, "application/octet-stream")
                        .await
                        .map_err(BlossomLfsError::Blossom)?;
                } else {
                    debug!("blob {} already exists on server, skipping upload", oid);
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
            .await;

            send_response(&sender, TransferResponse::new(oid, status).json()).await;
        });
    }

    async fn download(&mut self, oid: String) {
        let client = Arc::clone(&self.client);
        let sender = self.sender.clone();
        let temp_dir = self.temp_dir.clone();
        let config = self.config.clone();
        let output_path = lfs_object_path(&oid);

        self.tasks.spawn(async move {
            let status: Result<Option<String>> = async {
                send_progress(&sender, &oid, 0, 0, 0).await;

                let blob_data: Vec<u8> = client
                    .download(&oid)
                    .await
                    .map_err(BlossomLfsError::Blossom)?;

                tokio::fs::create_dir_all(output_path.parent().unwrap())
                    .await
                    .context("Failed to create output directory")?;

                // Try to parse as manifest; if it fails, treat as raw blob
                let manifest_result = std::str::from_utf8(&blob_data)
                    .ok()
                    .and_then(|s| Manifest::from_json(s).ok());

                if let Some(manifest) = manifest_result {
                    if !manifest.verify()? {
                        return Err(BlossomLfsError::MerkleVerificationFailed);
                    }

                    let total_size = manifest.file_size as usize;
                    send_progress(&sender, &oid, 0, total_size, 0).await;

                    if manifest.chunks == 1 {
                        let chunk_data: Vec<u8> = client
                            .download(&manifest.chunk_hashes[0])
                            .await
                            .map_err(BlossomLfsError::Blossom)?;

                        tokio::fs::write(&output_path, &chunk_data)
                            .await
                            .context("Failed to write file")?;

                        send_progress(&sender, &oid, total_size, total_size, total_size).await;
                    } else {
                        download_chunked_file(
                            &client,
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
                } else {
                    // Raw blob — write directly
                    let total_size = blob_data.len();
                    send_progress(&sender, &oid, 0, total_size, 0).await;

                    tokio::fs::write(&output_path, &blob_data)
                        .await
                        .context("Failed to write file")?;

                    send_progress(&sender, &oid, total_size, total_size, total_size).await;
                }

                Ok(Some(output_path.to_string_lossy().into()))
            }
            .await;

            send_response(&sender, TransferResponse::new(oid, status).json()).await;
        });
    }

    async fn terminate(&mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

async fn upload_chunked_file(
    client: &BlossomClient,
    _config: &Config,
    chunker: &Chunker,
    file_path: &Path,
    file_size: u64,
    sender: &tokio::sync::mpsc::Sender<String>,
    oid: &str,
) -> Result<Vec<String>> {
    let (chunks, _) = chunker.chunk_file(file_path).await?;

    let mut bytes_so_far = 0usize;
    let mut chunk_hashes = Vec::new();

    for chunk in &chunks {
        let chunk_data = chunker
            .read_chunk(file_path, chunk.offset, chunk.size)
            .await?;

        let chunk_hash = hash_data(&chunk_data);

        // Skip upload if this chunk already exists on the server
        let already_exists = client
            .exists(&chunk_hash)
            .await
            .unwrap_or(false);

        if !already_exists {
            client
                .upload(&chunk_data, "application/octet-stream")
                .await
                .map_err(BlossomLfsError::Blossom)?;
        } else {
            debug!("chunk {} already exists on server, skipping", chunk_hash);
        }

        chunk_hashes.push(chunk_hash);
        bytes_so_far += chunk.size;

        send_progress(sender, oid, bytes_so_far, file_size as usize, chunk.size).await;
    }

    Ok(chunk_hashes)
}

async fn download_chunked_file(
    client: &BlossomClient,
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
        let chunk_data: Vec<u8> = client
            .download(&chunk_info.hash)
            .await
            .map_err(BlossomLfsError::Blossom)?;

        assembler
            .write_chunk(oid, chunk_info.index, &chunk_data)
            .await?;

        bytes_so_far += chunk_info.size;
        send_progress(sender, oid, bytes_so_far, total_size, chunk_info.size).await;
    }

    assembler
        .assemble(oid, output_path, manifest.chunks)
        .await?;

    assembler.cleanup(oid).await?;

    Ok(())
}

fn hash_data(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

async fn send_response(sender: &tokio::sync::mpsc::Sender<String>, msg: String) {
    debug!("response: {}", &msg);
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
