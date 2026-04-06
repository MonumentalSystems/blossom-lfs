//! File splitting and reassembly.
//!
//! [`Chunker`] reads a file sequentially and produces a list of [`Chunk`]
//! descriptors, each containing the byte offset, size, and SHA-256 hash of
//! that segment. [`ChunkAssembler`] performs the inverse: it writes individual
//! chunk files to a temporary directory and concatenates them back into the
//! original file.

use crate::error::{BlossomLfsError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

/// Metadata for a single chunk produced by [`Chunker::chunk_file`].
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Zero-based index of this chunk within the file.
    pub index: usize,
    /// Byte offset from the start of the file.
    pub offset: u64,
    /// Size of this chunk in bytes.
    pub size: usize,
    /// SHA-256 hex digest of the chunk data.
    pub hash: String,
}

/// Splits files into fixed-size chunks and computes per-chunk SHA-256 hashes.
#[derive(Clone)]
pub struct Chunker {
    chunk_size: usize,
}

impl Chunker {
    /// Create a new chunker with the given maximum chunk size in bytes.
    pub fn new(chunk_size: usize) -> Result<Self> {
        if chunk_size == 0 {
            return Err(BlossomLfsError::InvalidChunkSize(
                "Chunk size must be greater than 0".to_string(),
            ));
        }
        Ok(Self { chunk_size })
    }

    /// Read `path` and split it into chunks, returning the chunk list and total file size.
    pub async fn chunk_file(&self, path: &Path) -> Result<(Vec<Chunk>, u64)> {
        let mut file = File::open(path).await?;
        let metadata = file.metadata().await?;
        let file_size = metadata.len();

        let mut chunks = Vec::new();
        let mut offset = 0u64;
        let mut index = 0;

        let mut remaining = file_size;
        loop {
            if remaining == 0 {
                break;
            }

            let to_read = std::cmp::min(self.chunk_size as u64, remaining) as usize;
            let mut buffer = vec![0u8; to_read];
            file.read_exact(&mut buffer).await?;

            let hash = self.hash_chunk(&buffer);

            chunks.push(Chunk {
                index,
                offset,
                size: to_read,
                hash,
            });

            offset += to_read as u64;
            remaining -= to_read as u64;
            index += 1;
        }

        Ok((chunks, file_size))
    }

    /// Read a single chunk from `path` at the given byte offset and size.
    pub async fn read_chunk(&self, path: &Path, offset: u64, size: usize) -> Result<Vec<u8>> {
        let mut file = File::open(path).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;

        let mut buffer = vec![0u8; size];
        file.read_exact(&mut buffer).await?;

        Ok(buffer)
    }

    pub fn hash_chunk(&self, data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    /// Returns `true` if the file is larger than the configured chunk size.
    pub fn should_chunk(&self, file_size: u64) -> bool {
        file_size > self.chunk_size as u64
    }
}

/// Writes downloaded chunks to a temporary directory and reassembles them
/// into the original file.
pub struct ChunkAssembler {
    output_dir: PathBuf,
}

impl ChunkAssembler {
    pub fn new(output_dir: PathBuf) -> Self {
        Self { output_dir }
    }

    pub async fn write_chunk(
        &self,
        file_id: &str,
        chunk_index: usize,
        data: &[u8],
    ) -> Result<PathBuf> {
        tokio::fs::create_dir_all(&self.output_dir).await?;

        let chunk_path = self
            .output_dir
            .join(file_id)
            .join(format!("chunk_{:06}", chunk_index));

        if let Some(parent) = chunk_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut file = File::create(&chunk_path).await?;
        file.write_all(data).await?;
        file.flush().await?;

        Ok(chunk_path)
    }

    pub async fn assemble(
        &self,
        file_id: &str,
        output_path: &Path,
        num_chunks: usize,
    ) -> Result<()> {
        tokio::fs::create_dir_all(output_path.parent().unwrap()).await?;

        let mut output_file = File::create(output_path).await?;

        for i in 0..num_chunks {
            let chunk_path = self
                .output_dir
                .join(file_id)
                .join(format!("chunk_{:06}", i));

            let chunk_data = tokio::fs::read(&chunk_path).await?;
            output_file.write_all(&chunk_data).await?;
        }

        Ok(())
    }

    pub async fn cleanup(&self, file_id: &str) -> Result<()> {
        let dir = self.output_dir.join(file_id);
        tokio::fs::remove_dir_all(dir)
            .await
            .map_err(BlossomLfsError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_chunk_small_file() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"test content").unwrap();
        file.flush().unwrap();

        let chunker = Chunker::new(10).unwrap();
        let (chunks, size) = chunker.chunk_file(file.path()).await.unwrap();

        assert_eq!(size, 12);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].size <= 10);
    }

    #[tokio::test]
    async fn test_chunk_hashing() {
        let chunker = Chunker::new(16).unwrap();
        let hash = chunker.hash_chunk(b"hello world");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_should_chunk() {
        let chunker = Chunker::new(1024).unwrap();
        assert!(!chunker.should_chunk(512));
        assert!(chunker.should_chunk(2048));
    }
}
