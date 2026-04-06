//! File chunking, Merkle-tree integrity, and manifest serialization.
//!
//! This module handles splitting large files into fixed-size chunks,
//! computing a Merkle tree over the chunk hashes for integrity verification,
//! and producing a JSON manifest that records everything needed to reassemble
//! the file on download.

pub mod chunker;
pub mod manifest;
pub mod merkle;

pub use chunker::{Chunk, ChunkAssembler, Chunker};
pub use manifest::{ChunkInfo, Manifest};
pub use merkle::{verify_merkle_root, MerkleProof, MerkleTree};
