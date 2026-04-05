use thiserror::Error;

#[derive(Error, Debug)]
pub enum BlossomLfsError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Invalid chunk size: {0}")]
    InvalidChunkSize(String),

    #[error("Merkle tree verification failed")]
    MerkleVerificationFailed,

    #[error("Chunk integrity error at index {0}")]
    ChunkIntegrityError(usize),

    #[error("Manifest not found for OID: {0}")]
    ManifestNotFound(String),

    #[error("Nostr signing error: {0}")]
    NostrSigning(String),

    #[error("Invalid private key")]
    InvalidPrivateKey,

    #[error("Upload failed: {0}")]
    UploadFailed(String),

    #[error("Download failed: {0}")]
    DownloadFailed(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("Chunk index {0} out of bounds (max: {1})")]
    ChunkOutOfBounds(usize, usize),
}

impl From<anyhow::Error> for BlossomLfsError {
    fn from(err: anyhow::Error) -> Self {
        BlossomLfsError::Config(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, BlossomLfsError>;
