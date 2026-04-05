use blossom_lfs::chunking::Manifest;

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
    assert_eq!(manifest.chunk_size, 512);
    assert_eq!(manifest.chunks, 2);
    assert_eq!(manifest.chunk_hashes, hashes);
    assert!(manifest.verify().unwrap(), "Manifest should verify");
}

#[test]
fn test_manifest_serialization() {
    let hashes = vec!["a".repeat(64), "b".repeat(64)];
    let manifest = Manifest::new(
        2048,
        1024,
        hashes,
        Some("data.tar.gz".to_string()),
        None,
        None,
    )
    .unwrap();

    let json = manifest.to_json().unwrap();
    let decoded = Manifest::from_json(&json).unwrap();

    assert_eq!(decoded.merkle_root, manifest.merkle_root);
    assert_eq!(decoded.file_size, manifest.file_size);
    assert_eq!(decoded.chunks, manifest.chunks);
}

#[test]
fn test_manifest_hash() {
    let manifest1 = Manifest::new(512, 512, vec!["a".repeat(64)], None, None, None).unwrap();

    let manifest2 = Manifest::new(512, 512, vec!["b".repeat(64)], None, None, None).unwrap();

    let hash1 = manifest1.hash().unwrap();
    let hash2 = manifest2.hash().unwrap();

    assert_ne!(
        hash1, hash2,
        "Different manifests should have different hashes"
    );
    assert_eq!(hash1.len(), 64, "Hash should be 64 hex chars");
}

#[test]
fn test_manifest_chunk_info() {
    let hashes = vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)];
    let manifest = Manifest::new(1024, 512, hashes, None, None, None).unwrap();

    let info0 = manifest.chunk_info(0).unwrap();
    assert_eq!(info0.index, 0);
    assert_eq!(info0.offset, 0);
    assert_eq!(info0.size, 512);
    assert_eq!(info0.hash, "a".repeat(64));

    let info2 = manifest.chunk_info(2).unwrap();
    assert_eq!(info2.index, 2);
    assert_eq!(info2.offset, 1024); // Last chunk has 0 size
    assert_eq!(info2.size, 0);
}

#[test]
fn test_manifest_out_of_bounds() {
    let manifest = Manifest::new(512, 512, vec!["a".repeat(64)], None, None, None).unwrap();

    let result = manifest.chunk_info(1);
    assert!(result.is_err(), "Should error for out-of-bounds chunk");
}

#[test]
fn test_manifest_verification() {
    let manifest = Manifest::new(
        2048,
        1024,
        vec!["a".repeat(64), "b".repeat(64)],
        None,
        None,
        None,
    )
    .unwrap();

    assert!(manifest.verify().unwrap(), "Valid manifest should verify");
}

#[test]
fn test_manifest_all_chunks() {
    let hashes = vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)];
    let manifest = Manifest::new(3072, 1024, hashes.clone(), None, None, None).unwrap();

    let all_info = manifest.all_chunk_info().unwrap();
    assert_eq!(all_info.len(), 3);

    for (i, info) in all_info.iter().enumerate() {
        assert_eq!(info.index, i);
        assert_eq!(info.hash, hashes[i]);
    }
}
