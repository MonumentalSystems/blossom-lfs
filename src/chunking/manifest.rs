use crate::{
    chunking::merkle::MerkleTree,
    error::{BlossomLfsError, Result},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

const MANIFEST_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkInfo {
    pub index: usize,
    pub hash: String,
    pub offset: u64,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub file_size: u64,
    pub chunk_size: usize,
    pub chunks: usize,
    pub merkle_root: String,
    pub chunk_hashes: Vec<String>,
    pub original_filename: Option<String>,
    pub content_type: Option<String>,
    pub created_at: u64,
    pub blossom_server: Option<String>,
}

impl Manifest {
    pub fn new(
        file_size: u64,
        chunk_size: usize,
        chunk_hashes: Vec<String>,
        original_filename: Option<String>,
        content_type: Option<String>,
        blossom_server: Option<String>,
    ) -> Result<Self> {
        let merkle_tree = MerkleTree::new(chunk_hashes.clone())?;
        let merkle_root = merkle_tree.root().to_string();

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BlossomLfsError::Config(e.to_string()))?
            .as_secs();

        Ok(Self {
            version: MANIFEST_VERSION.to_string(),
            file_size,
            chunk_size,
            chunks: chunk_hashes.len(),
            merkle_root,
            chunk_hashes,
            original_filename,
            content_type,
            created_at,
            blossom_server,
        })
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(BlossomLfsError::Serialization)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(BlossomLfsError::Serialization)
    }

    pub fn hash(&self) -> Result<String> {
        let json = self.to_json()?;
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        Ok(hex::encode(hasher.finalize()))
    }

    pub fn verify(&self) -> Result<bool> {
        let merkle_tree = MerkleTree::new(self.chunk_hashes.clone())?;
        Ok(merkle_tree.root() == self.merkle_root)
    }

    pub fn chunk_info(&self, index: usize) -> Result<ChunkInfo> {
        if index >= self.chunks {
            return Err(BlossomLfsError::ChunkOutOfBounds(index, self.chunks - 1));
        }

        let offset = (index * self.chunk_size) as u64;
        let size = if index == self.chunks - 1 {
            (self.file_size - offset) as usize
        } else {
            self.chunk_size
        };

        Ok(ChunkInfo {
            index,
            hash: self.chunk_hashes[index].clone(),
            offset,
            size,
        })
    }

    pub fn all_chunk_info(&self) -> Result<Vec<ChunkInfo>> {
        (0..self.chunks).map(|i| self.chunk_info(i)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_creation() {
        let hashes = vec!["a".repeat(64), "b".repeat(64)];
        let manifest = Manifest::new(
            1024,
            512,
            hashes.clone(),
            Some("test.bin".to_string()),
            Some("application/octet-stream".to_string()),
            Some("https://cdn.example.com".to_string()),
        )
        .unwrap();

        assert_eq!(manifest.version, "1.0");
        assert_eq!(manifest.file_size, 1024);
        assert_eq!(manifest.chunks, 2);
        assert!(manifest.verify().unwrap());
    }

    #[test]
    fn test_manifest_serialization() {
        let hashes = vec!["a".repeat(64)];
        let manifest = Manifest::new(512, 512, hashes, None, None, None).unwrap();

        let json = manifest.to_json().unwrap();
        let decoded = Manifest::from_json(&json).unwrap();

        assert_eq!(decoded.merkle_root, manifest.merkle_root);
    }

    #[test]
    fn test_chunk_info() {
        let hashes = vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)];
        let manifest = Manifest::new(1024, 512, hashes, None, None, None).unwrap();

        let info = manifest.chunk_info(0).unwrap();
        assert_eq!(info.offset, 0);
        assert_eq!(info.size, 512);

        let last_info = manifest.chunk_info(2).unwrap();
        assert_eq!(last_info.size, 0);
        assert_eq!(last_info.offset, 1024);
    }
}
