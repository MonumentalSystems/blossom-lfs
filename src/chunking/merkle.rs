use crate::error::{BlossomLfsError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub hash: String,
    pub proof: Vec<(String, bool)>, // (sibling_hash, is_left) - is_left true if sibling is on the left
}

#[derive(Debug, Clone)]
pub struct MerkleTree {
    pub leaves: Vec<String>,
    pub tree: Vec<Vec<String>>,
}

impl MerkleTree {
    pub fn new(hashes: Vec<String>) -> Result<Self> {
        if hashes.is_empty() {
            return Err(BlossomLfsError::MerkleVerificationFailed);
        }

        let leaves = hashes.clone();
        let tree = Self::build_tree(&leaves);

        Ok(Self { leaves, tree })
    }

    fn build_tree(leaves: &[String]) -> Vec<Vec<String>> {
        if leaves.is_empty() {
            return vec![];
        }

        let mut tree = vec![leaves.to_vec()];

        while tree.last().unwrap().len() > 1 {
            let current_level = tree.last().unwrap();
            let mut next_level = Vec::new();

            for i in (0..current_level.len()).step_by(2) {
                let left = &current_level[i];
                let right = if i + 1 < current_level.len() {
                    &current_level[i + 1]
                } else {
                    left
                };

                let parent_hash = Self::hash_pair(left, right);
                next_level.push(parent_hash);
            }

            tree.push(next_level);
        }

        tree
    }

    fn hash_pair(left: &str, right: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(hex::decode(left).unwrap_or_default());
        hasher.update(hex::decode(right).unwrap_or_default());
        hex::encode(hasher.finalize())
    }

    pub fn root(&self) -> &str {
        self.tree
            .last()
            .and_then(|level| level.first())
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    pub fn proof(&self, leaf_index: usize) -> Result<MerkleProof> {
        if leaf_index >= self.leaves.len() {
            return Err(BlossomLfsError::ChunkOutOfBounds(
                leaf_index,
                self.leaves.len() - 1,
            ));
        }

        let hash = self.leaves[leaf_index].clone();
        let mut proof = Vec::new();
        let mut index = leaf_index;

        for level in &self.tree[..self.tree.len() - 1] {
            let sibling_index = if index.is_multiple_of(2) {
                // Current node is on the left, sibling is on the right
                if index + 1 < level.len() {
                    index + 1
                } else {
                    // No sibling, use self
                    index
                }
            } else {
                // Current node is on the right, sibling is on the left
                index - 1
            };

            let sibling_hash = level[sibling_index].clone();
            let is_left = !index.is_multiple_of(2); // sibling is on left if current is on right (odd index)

            proof.push((sibling_hash, is_left));
            index /= 2;
        }

        Ok(MerkleProof { hash, proof })
    }

    pub fn verify_proof(&self, proof: &MerkleProof) -> Result<bool> {
        let mut current_hash = proof.hash.clone();

        for (sibling_hash, is_left) in &proof.proof {
            if *is_left {
                // Sibling is on the left
                current_hash = Self::hash_pair(sibling_hash, &current_hash);
            } else {
                // Sibling is on the right
                current_hash = Self::hash_pair(&current_hash, sibling_hash);
            }
        }

        Ok(current_hash == self.root())
    }

    pub fn verify_chunk(&self, chunk_hash: &str, chunk_index: usize) -> Result<bool> {
        let proof = self.proof(chunk_index)?;
        if proof.hash != chunk_hash {
            return Ok(false);
        }
        self.verify_proof(&proof)
    }

    pub fn leaves(&self) -> &[String] {
        &self.leaves
    }
}

pub fn verify_merkle_root(root: &str, leaf_hash: &str, proof: &[(String, bool)]) -> bool {
    let mut current_hash = leaf_hash.to_string();

    for (sibling_hash, is_left) in proof {
        if *is_left {
            current_hash = MerkleTree::hash_pair(sibling_hash, &current_hash);
        } else {
            current_hash = MerkleTree::hash_pair(&current_hash, sibling_hash);
        }
    }

    current_hash == root
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn make_hash(s: &str) -> String {
        let hash = Sha256::digest(s.as_bytes());
        hex::encode(hash)
    }

    #[test]
    fn test_merkle_tree_single_leaf() {
        let hashes = vec![make_hash("a")];
        let tree = MerkleTree::new(hashes).unwrap();
        assert_eq!(tree.root().len(), 64);
    }

    #[test]
    fn test_merkle_tree_two_leaves() {
        let hashes = vec![make_hash("a"), make_hash("b")];
        let tree = MerkleTree::new(hashes.clone()).unwrap();
        assert_eq!(tree.tree.len(), 2);
        assert_eq!(tree.tree.last().unwrap().len(), 1);
    }

    #[test]
    fn test_merkle_proof() {
        let hashes = vec![make_hash("a"), make_hash("b"), make_hash("c")];
        let tree = MerkleTree::new(hashes.clone()).unwrap();

        let proof = tree.proof(0).unwrap();
        assert!(tree.verify_proof(&proof).unwrap());

        let proof2 = tree.proof(1).unwrap();
        assert!(tree.verify_proof(&proof2).unwrap());
    }

    #[test]
    fn test_verify_chunk() {
        let hash_a = make_hash("a");
        let hash_b = make_hash("b");
        let hashes = vec![hash_a.clone(), hash_b.clone()];
        let tree = MerkleTree::new(hashes).unwrap();

        assert!(tree.verify_chunk(&hash_a, 0).unwrap());
        assert!(tree.verify_chunk(&hash_b, 1).unwrap());
        assert!(!tree.verify_chunk(&make_hash("x"), 0).unwrap());
    }

    #[test]
    fn test_verify_merkle_root() {
        let hashes = vec![make_hash("a"), make_hash("b")];
        let tree = MerkleTree::new(hashes).unwrap();

        let proof = tree.proof(0).unwrap();
        assert!(verify_merkle_root(tree.root(), &proof.hash, &proof.proof));
    }
}
