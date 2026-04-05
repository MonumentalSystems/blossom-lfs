use crate::{
    blossom::{BlossomClient, AuthToken, ActionType},
    chunking::{Chunker, Manifest, ChunkAssembler},
    config::Config,
    error::{BlossomLfsError, Result},
    protocol::{InitResponse, ProgressResponse, TransferResponse},
};
use anyhow::Context as _;
use log::debug;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const TEMP_DIR: &str = ".blossom-lfs-tmp";

pub struct Agent {
    config: Config,
    client: BlossomClient,
    sender: tokio::sync::mpsc::Sender<String>,
    tasks: tokio::task::JoinSet<()>,
    chunker: Chunker,
    temp_dir: PathBuf,
}

impl Agent {
    pub fn new(config: Config, sender: tokio::sync::mpsc::Sender<String>) -> Result<Self> {
        let client = BlossomClient::new(config.server_url.clone())?;
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
        let request: crate::protocol::Request = serde_json::from_str(request)
            .context("invalid request")?;
        
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
        let client = self.client.clone();
        let sender = self.sender.clone();
        let chunker = self.chunker.clone();

        self.tasks.spawn(async move {
            let status: Result<Option<String>> = async {
                let file_path = PathBuf::from(&path);
                let metadata = tokio::fs::metadata(&file_path).await
                    .context("Failed to read file metadata")?;
                let file_size = metadata.len();
                
                let chunk_hashes = if chunker.should_chunk(file_size) {
                    Some(upload_chunked_file(
                        &client,
                        &config,
                        &chunker,
                        &file_path,
                        file_size,
                        &sender,
                        &oid,
                    ).await?)
                } else {
                    None
                };
                
                if chunk_hashes.is_some() {
                    // Chunked upload complete — chunks are on the server.
                    // Now upload the complete file so it's retrievable by OID.
                    let data = tokio::fs::read(&file_path).await
                        .context("Failed to read file")?;
                    let hash = hash_data(&data);

                    let auth_token = create_auth_token(&config, ActionType::Upload, Some(&hash))?;
                    client.upload_blob(data, &hash, None, Some(&auth_token))
                        .await?;

                    send_progress(&sender, &oid, file_size as usize, file_size as usize, file_size as usize).await;
                } else {
                    // Single blob upload: upload raw data directly under its hash (= OID)
                    let data = tokio::fs::read(&file_path).await
                        .context("Failed to read file")?;
                    let hash = hash_data(&data);

                    let auth_token = create_auth_token(&config, ActionType::Upload, Some(&hash))?;
                    client.upload_blob(data, &hash, None, Some(&auth_token))
                        .await?;

                    send_progress(&sender, &oid, file_size as usize, file_size as usize, file_size as usize).await;
                }

                Ok(None)
            }
            .await;
            
            send_response(&sender, TransferResponse::new(oid, status).json()).await;
        });
    }
    
    async fn download(&mut self, oid: String) {
        let config = self.config.clone();
        let client = self.client.clone();
        let sender = self.sender.clone();
        let temp_dir = self.temp_dir.clone();
        let output_path = lfs_object_path(&oid);
        
        self.tasks.spawn(async move {
            let status: Result<Option<String>> = async {
                let auth_token = create_auth_token(&config, ActionType::Get, Some(&oid))?;

                send_progress(&sender, &oid, 0, 0, 0).await;

                let blob_data = client.download_blob(&oid, Some(&auth_token))
                    .await
                    .map_err(|e| BlossomLfsError::from(e))?;

                tokio::fs::create_dir_all(output_path.parent().unwrap()).await
                    .context("Failed to create output directory")?;

                // Try to parse as manifest; if it fails, treat as raw blob
                let manifest_result = std::str::from_utf8(&blob_data)
                    .ok()
                    .and_then(|s| Manifest::from_json(s).ok());

                if let Some(manifest) = manifest_result {
                    if !manifest.verify().map_err(|e| BlossomLfsError::from(e))? {
                        return Err(BlossomLfsError::MerkleVerificationFailed.into());
                    }

                    let total_size = manifest.file_size as usize;
                    send_progress(&sender, &oid, 0, total_size, 0).await;

                    if manifest.chunks == 1 {
                        let chunk_data = client.download_blob(&manifest.chunk_hashes[0], Some(&auth_token))
                            .await
                            .context("Failed to download single chunk")?;

                        tokio::fs::write(&output_path, &chunk_data).await
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
                        ).await
                        .context("Failed to download chunked file")?;
                    }
                } else {
                    // Raw blob — write directly
                    let total_size = blob_data.len();
                    send_progress(&sender, &oid, 0, total_size, 0).await;

                    tokio::fs::write(&output_path, &blob_data).await
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
    config: &Config,
    chunker: &Chunker,
    file_path: &PathBuf,
    file_size: u64,
    sender: &tokio::sync::mpsc::Sender<String>,
    oid: &str,
) -> Result<Vec<String>> {
    let (chunks, _) = chunker.chunk_file(file_path).await
        .map_err(|e| BlossomLfsError::from(e))?;
    
    let auth_token = create_auth_token(config, ActionType::Upload, None)?;
    
    let mut bytes_so_far = 0usize;
    let mut chunk_hashes = Vec::new();
    
    for chunk in &chunks {
        let chunk_data = chunker.read_chunk(file_path, chunk.offset, chunk.size).await
            .map_err(|e| BlossomLfsError::from(e))?;
        
        let chunk_hash = hash_data(&chunk_data);
        
        client.upload_blob(chunk_data.clone(), &chunk_hash, None, Some(&auth_token))
            .await?;
        
        chunk_hashes.push(chunk_hash);
        bytes_so_far += chunk.size;
        
        send_progress(sender, oid, bytes_so_far, file_size as usize, chunk.size).await;
    }
    
    Ok(chunk_hashes)
}

async fn download_chunked_file(
    client: &BlossomClient,
    config: &Config,
    manifest: &Manifest,
    output_path: &PathBuf,
    sender: &tokio::sync::mpsc::Sender<String>,
    oid: &str,
    temp_dir: &PathBuf,
) -> Result<()> {
    let auth_token = create_auth_token(config, ActionType::Get, None)?;
    let assembler = ChunkAssembler::new(temp_dir.clone());
    
    let mut bytes_so_far = 0usize;
    let total_size = manifest.file_size as usize;
    
    for chunk_info in manifest.all_chunk_info().map_err(|e| BlossomLfsError::from(e))? {
        let chunk_data = client.download_blob(&chunk_info.hash, Some(&auth_token))
            .await
            .map_err(|e| BlossomLfsError::from(e))?;
        
        assembler.write_chunk(oid, chunk_info.index, &chunk_data).await
            .map_err(|e| BlossomLfsError::from(e))?;
        
        bytes_so_far += chunk_info.size;
        send_progress(sender, oid, bytes_so_far, total_size, chunk_info.size).await;
    }
    
    assembler.assemble(oid, output_path, manifest.chunks).await
        .map_err(|e| BlossomLfsError::from(e))?;
    
    assembler.cleanup(oid).await
        .map_err(|e| BlossomLfsError::from(e))?;
    
    Ok(())
}

fn create_auth_token(config: &Config, action: ActionType, blob_hash: Option<&str>) -> Result<AuthToken> {
    let hashes: Vec<&str> = blob_hash.map(|h| vec![h]).unwrap_or_default();
    
    AuthToken::new(
        &config.secret_key,
        action,
        Some(&config.server_url),
        if hashes.is_empty() { None } else { Some(hashes) },
        config.auth_expiration,
    )
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