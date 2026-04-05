use blossom_lfs::chunking::{verify_merkle_root, MerkleTree};
use sha2::{Digest, Sha256};

fn make_hash(s: &str) -> String {
    let hash = Sha256::digest(s.as_bytes());
    hex::encode(hash)
}

#[test]
fn test_merkle_tree_single_leaf() {
    let hashes = vec![make_hash("a")];
    let tree = MerkleTree::new(hashes).unwrap();

    assert_eq!(tree.root().len(), 64, "Root hash should be 64 hex chars");
    assert_eq!(tree.leaves().len(), 1, "Should have 1 leaf");
}

#[test]
fn test_merkle_tree_two_leaves() {
    let hashes = vec![make_hash("a"), make_hash("b")];
    let tree = MerkleTree::new(hashes).unwrap();

    assert_eq!(tree.tree.len(), 2, "Should have 2 levels");
    assert_eq!(
        tree.tree.last().unwrap().len(),
        1,
        "Root should be single hash"
    );
}

#[test]
fn test_merkle_tree_four_leaves() {
    let hashes = vec![
        make_hash("a"),
        make_hash("b"),
        make_hash("c"),
        make_hash("d"),
    ];
    let tree = MerkleTree::new(hashes).unwrap();

    assert_eq!(tree.tree.len(), 3, "Should have 3 levels");
    assert_eq!(tree.leaves().len(), 4);
}

#[test]
fn test_merkle_proof() {
    let hashes = vec![make_hash("a"), make_hash("b"), make_hash("c")];
    let tree = MerkleTree::new(hashes.clone()).unwrap();

    let proof = tree.proof(0).unwrap();
    assert!(tree.verify_proof(&proof).unwrap(), "Proof should be valid");

    let proof2 = tree.proof(1).unwrap();
    assert!(
        tree.verify_proof(&proof2).unwrap(),
        "Second proof should be valid"
    );
}

#[test]
fn test_verify_chunk() {
    let hash_a = make_hash("a");
    let hash_b = make_hash("b");
    let hashes = vec![hash_a.clone(), hash_b.clone()];
    let tree = MerkleTree::new(hashes).unwrap();

    assert!(
        tree.verify_chunk(&hash_a, 0).unwrap(),
        "Chunk 0 should verify"
    );
    assert!(
        tree.verify_chunk(&hash_b, 1).unwrap(),
        "Chunk 1 should verify"
    );
    assert!(
        !tree.verify_chunk(&make_hash("x"), 0).unwrap(),
        "Wrong hash should fail"
    );
}

#[test]
fn test_verify_merkle_root_function() {
    let hashes = vec![make_hash("a"), make_hash("b")];
    let tree = MerkleTree::new(hashes).unwrap();

    let proof = tree.proof(0).unwrap();
    let root = tree.root();

    assert!(
        verify_merkle_root(root, &proof.hash, &proof.proof),
        "Should verify"
    );
}

#[test]
fn test_merkle_tree_out_of_bounds() {
    let hashes = vec![make_hash("a")];
    let tree = MerkleTree::new(hashes).unwrap();

    let result = tree.proof(1);
    assert!(result.is_err(), "Should error for out-of-bounds index");
}

#[test]
fn test_merkle_consistency() {
    // Same leaves should produce same root
    let hashes1 = vec![make_hash("a"), make_hash("b")];
    let hashes2 = vec![make_hash("a"), make_hash("b")];

    let tree1 = MerkleTree::new(hashes1).unwrap();
    let tree2 = MerkleTree::new(hashes2).unwrap();

    assert_eq!(
        tree1.root(),
        tree2.root(),
        "Same leaves should give same root"
    );
}
