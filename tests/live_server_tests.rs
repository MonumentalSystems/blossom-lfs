//! Integration tests against a live Blossom server.
//!
//! These tests are gated behind `#[ignore]` so they don't run during normal
//! `cargo test`. Run them with:
//!
//! ```sh
//! BLOSSOM_TEST_SERVER=https://blossom.gnostr.cloud \
//! BLOSSOM_TEST_NSEC=nsec1... \
//!   cargo test --test live_server_tests -- --ignored
//! ```
//!
//! ## Environment variables
//!
//! | Variable              | Required | Description                                    |
//! |-----------------------|----------|------------------------------------------------|
//! | `BLOSSOM_TEST_SERVER` | yes      | Base URL of the Blossom server                 |
//! | `BLOSSOM_TEST_NSEC`   | yes      | Nostr private key (nsec1… or 64-char hex)      |

use blossom_rs::{auth::Signer, BlossomClient};
use sha2::{Digest, Sha256};

/// Read required env var or skip (panic with clear message).
fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "Environment variable {} is required for live server tests. \
             Run with: {} =<value> cargo test --test live_server_tests -- --ignored",
            name, name
        )
    })
}

/// Create a BlossomClient from the test environment.
fn live_client() -> BlossomClient {
    let server_url = required_env("BLOSSOM_TEST_SERVER");
    let nsec = required_env("BLOSSOM_TEST_NSEC");

    // Normalise nsec → hex if needed
    let hex_key = if nsec.starts_with("nsec1") {
        let sk = nostr::SecretKey::parse(&nsec).expect("invalid BLOSSOM_TEST_NSEC");
        hex::encode(sk.secret_bytes())
    } else {
        nsec
    };

    let signer = Signer::from_secret_hex(&hex_key).expect("failed to create signer");
    BlossomClient::with_timeout(vec![server_url], signer, std::time::Duration::from_secs(30))
}

/// Generate a unique test payload so tests don't collide.
fn test_payload(tag: &str) -> Vec<u8> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("blossom-lfs live test [{}] @ {}", tag, ts).into_bytes()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn live_upload_and_download() {
    let client = live_client();
    let data = test_payload("upload_download");
    let expected_hash = format!("{:x}", Sha256::digest(&data));

    // Upload
    let descriptor = client
        .upload(&data, "application/octet-stream")
        .await
        .expect("upload failed");

    assert_eq!(
        descriptor.sha256, expected_hash,
        "server returned wrong hash"
    );
    assert_eq!(descriptor.size, data.len() as u64);

    // Download
    let downloaded: Vec<u8> = client
        .download(&expected_hash)
        .await
        .expect("download failed");

    assert_eq!(
        downloaded, data,
        "downloaded data should match uploaded data"
    );
}

#[tokio::test]
#[ignore]
async fn live_exists_check() {
    let client = live_client();
    let data = test_payload("exists_check");
    let expected_hash = format!("{:x}", Sha256::digest(&data));

    // Should not exist yet (unique payload)
    let before = client.exists(&expected_hash).await.unwrap_or(false);
    // (We don't assert false here because a previous test run may have uploaded
    // the same nanos — unlikely but possible.)

    // Upload
    client
        .upload(&data, "application/octet-stream")
        .await
        .expect("upload failed");

    // Should exist now
    let after = client
        .exists(&expected_hash)
        .await
        .expect("exists check failed");
    assert!(after, "blob should exist after upload");

    // Non-existent hash
    let missing = client
        .exists("0000000000000000000000000000000000000000000000000000000000000000")
        .await
        .unwrap_or(false);
    assert!(!missing, "random hash should not exist");

    let _ = before; // suppress unused warning
}

#[tokio::test]
#[ignore]
async fn live_dedup_skips_existing() {
    let client = live_client();
    let data = test_payload("dedup");
    let expected_hash = format!("{:x}", Sha256::digest(&data));

    // Upload once
    client
        .upload(&data, "application/octet-stream")
        .await
        .expect("first upload failed");

    // Verify exists
    assert!(
        client.exists(&expected_hash).await.unwrap(),
        "should exist after first upload"
    );

    // Upload again — should succeed (server accepts idempotent puts)
    let descriptor = client
        .upload(&data, "application/octet-stream")
        .await
        .expect("second upload failed");

    assert_eq!(descriptor.sha256, expected_hash);
}

#[tokio::test]
#[ignore]
async fn live_chunked_upload_and_manifest() {
    use blossom_lfs::chunking::{Chunker, Manifest};

    let client = live_client();

    // Create a payload that will be chunked (3 chunks of 64 bytes)
    let data: Vec<u8> = (0..192).map(|i| (i % 256) as u8).collect();

    let chunker = Chunker::new(64).unwrap();

    // Write to a temp file for chunking
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut tmp, &data).unwrap();

    let (chunks, file_size) = chunker.chunk_file(tmp.path()).await.unwrap();
    assert_eq!(file_size, 192);
    assert_eq!(chunks.len(), 3);

    // Upload each chunk
    let mut chunk_hashes = Vec::new();
    for chunk in &chunks {
        let chunk_data = chunker
            .read_chunk(tmp.path(), chunk.offset, chunk.size)
            .await
            .unwrap();

        client
            .upload(&chunk_data, "application/octet-stream")
            .await
            .expect("chunk upload failed");

        chunk_hashes.push(chunk.hash.clone());
    }

    // Build and upload manifest
    let manifest = Manifest::new(
        file_size,
        64,
        chunk_hashes.clone(),
        Some("live_test.bin".to_string()),
        Some("application/octet-stream".to_string()),
        None,
    )
    .unwrap();

    assert!(manifest.verify().unwrap(), "manifest should verify locally");

    let manifest_json = manifest.to_json().unwrap();
    let manifest_descriptor = client
        .upload(manifest_json.as_bytes(), "application/json")
        .await
        .expect("manifest upload failed");

    // Download and verify manifest
    let downloaded: Vec<u8> = client
        .download(&manifest_descriptor.sha256)
        .await
        .expect("manifest download failed");

    let parsed = Manifest::from_json(&String::from_utf8_lossy(&downloaded)).unwrap();
    assert_eq!(parsed.file_size, 192);
    assert_eq!(parsed.chunks, 3);
    assert!(
        parsed.verify().unwrap(),
        "downloaded manifest should verify"
    );

    // Download each chunk and reassemble
    let mut reassembled = Vec::new();
    for hash in &parsed.chunk_hashes {
        let chunk_data: Vec<u8> = client.download(hash).await.expect("chunk download failed");
        reassembled.extend_from_slice(&chunk_data);
    }

    assert_eq!(reassembled, data, "reassembled data should match original");
}
