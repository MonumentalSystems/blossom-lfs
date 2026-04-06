use blossom_rs::auth::{auth_header_value, build_blossom_auth, Signer};

fn generate_test_signer() -> Signer {
    Signer::generate()
}

#[test]
fn test_build_blossom_auth_produces_valid_event() {
    let signer = generate_test_signer();

    let event = build_blossom_auth(
        &signer,
        "upload",
        Some("abc123"),
        Some("cdn.example.com"),
        "Upload Blob",
    );

    assert_eq!(event.kind, 24242);
    assert!(!event.id.is_empty());
    assert!(!event.sig.is_empty());
    assert!(!event.pubkey.is_empty());
}

#[test]
fn test_auth_header_value_format() {
    let signer = generate_test_signer();

    let event = build_blossom_auth(&signer, "get", None, Some("test.com"), "Download Blob");
    let header = auth_header_value(&event);

    assert!(
        header.starts_with("Nostr "),
        "Header should start with 'Nostr '"
    );
}

#[test]
fn test_signer_from_secret_hex() {
    let signer = Signer::generate();
    let hex_key = signer.secret_key_hex();

    let restored = Signer::from_secret_hex(&hex_key);
    assert!(restored.is_ok(), "Should create signer from hex key");
}

#[test]
fn test_different_actions_same_pubkey() {
    let signer = generate_test_signer();

    let upload_event = build_blossom_auth(&signer, "upload", None, None, "Upload Blob");
    let get_event = build_blossom_auth(&signer, "get", None, None, "Download Blob");

    assert_eq!(upload_event.pubkey, get_event.pubkey);
}

#[test]
fn test_event_fields_are_hex() {
    let signer = generate_test_signer();

    let event = build_blossom_auth(&signer, "upload", Some("testhash"), None, "Upload Blob");

    assert!(event.id.chars().all(|c: char| c.is_ascii_hexdigit()));
    assert!(event.pubkey.chars().all(|c: char| c.is_ascii_hexdigit()));
    assert!(event.sig.chars().all(|c: char| c.is_ascii_hexdigit()));
}
