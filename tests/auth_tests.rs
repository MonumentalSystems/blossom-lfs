use blossom_lfs::blossom::{ActionType, AuthToken};
use secp256k1::{Secp256k1, SecretKey};

fn generate_test_key() -> [u8; 32] {
    let secp = Secp256k1::new();
    let mut rng = secp256k1::rand::thread_rng();
    let secret_key = SecretKey::new(&mut rng);
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&secret_key.secret_bytes());
    key_bytes
}

#[test]
fn test_auth_token_creation() {
    let secret_key = generate_test_key();

    let token = AuthToken::new(
        &secret_key,
        ActionType::Upload,
        Some("cdn.example.com"),
        Some(vec!["abc123"]),
        3600,
    )
    .unwrap();

    // Token should have event fields populated
    assert_eq!(token.event.kind, 24242);
    assert!(!token.event.id.is_empty());
    assert!(!token.event.sig.is_empty());
}

#[test]
fn test_auth_token_different_actions() {
    let secret_key = generate_test_key();

    let upload_token = AuthToken::new(&secret_key, ActionType::Upload, None, None, 3600).unwrap();
    let get_token = AuthToken::new(&secret_key, ActionType::Get, None, None, 3600).unwrap();
    let delete_token = AuthToken::new(&secret_key, ActionType::Delete, None, None, 3600).unwrap();

    // Different actions should produce different events
    // But the same secret key should have same pubkey
    assert_eq!(upload_token.event.pubkey, get_token.event.pubkey);

    // Check that 't' tag has correct action
    let upload_tags: Vec<_> = upload_token
        .event
        .tags
        .iter()
        .filter(|tag| tag[0] == "t")
        .collect();
    assert!(upload_tags.iter().any(|tag| tag[1] == "upload"));
}

#[test]
fn test_auth_token_server_scoping() {
    let secret_key = generate_test_key();

    let token_with_server = AuthToken::new(
        &secret_key,
        ActionType::Upload,
        Some("example.com"),
        None,
        3600,
    )
    .unwrap();

    let token_without_server =
        AuthToken::new(&secret_key, ActionType::Upload, None, None, 3600).unwrap();

    // Token with server should have 'server' tag
    let server_tags: Vec<_> = token_with_server
        .event
        .tags
        .iter()
        .filter(|tag| tag[0] == "server")
        .collect();
    assert!(!server_tags.is_empty());
    assert_eq!(server_tags[0][1], "example.com");

    // Token without server should not have 'server' tag
    let server_tags: Vec<_> = token_without_server
        .event
        .tags
        .iter()
        .filter(|tag| tag[0] == "server")
        .collect();
    assert!(server_tags.is_empty());
}

#[test]
fn test_auth_token_blob_hash() {
    let secret_key = generate_test_key();

    let token = AuthToken::new(
        &secret_key,
        ActionType::Upload,
        None,
        Some(vec!["abc123", "def456"]),
        3600,
    )
    .unwrap();

    // Should have 'x' tags for each blob hash
    let x_tags: Vec<_> = token
        .event
        .tags
        .iter()
        .filter(|tag| tag[0] == "x")
        .collect();
    assert_eq!(x_tags.len(), 2);

    let hash_values: Vec<&str> = x_tags.iter().map(|tag| tag[1].as_str()).collect();
    assert!(hash_values.contains(&"abc123"));
    assert!(hash_values.contains(&"def456"));
}

#[test]
fn test_auth_token_expiration() {
    let secret_key = generate_test_key();

    let token = AuthToken::new(
        &secret_key,
        ActionType::Upload,
        None,
        None,
        3600, // 1 hour
    )
    .unwrap();

    // Should have 'expiration' tag
    let exp_tags: Vec<_> = token
        .event
        .tags
        .iter()
        .filter(|tag| tag[0] == "expiration")
        .collect();
    assert!(!exp_tags.is_empty());

    let expiration: u64 = exp_tags[0][1].parse().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Expiration should be approximately 1 hour from now
    assert!(expiration > now);
    assert!(expiration < now + 3700); // Allow some variance
}

#[test]
fn test_auth_token_authorization_header() {
    let secret_key = generate_test_key();

    let token = AuthToken::new(&secret_key, ActionType::Get, Some("test.com"), None, 3600).unwrap();

    let header = token.to_authorization_header().unwrap();

    assert!(
        header.starts_with("Nostr "),
        "Header should start with 'Nostr '"
    );

    let encoded = header.strip_prefix("Nostr ").unwrap();
    // Should be valid base64url
    assert!(encoded
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
}

#[test]
fn test_auth_token_signature_validity() {
    let secret_key = generate_test_key();

    let token = AuthToken::new(
        &secret_key,
        ActionType::Upload,
        None,
        Some(vec!["testhash"]),
        3600,
    )
    .unwrap();

    // Signature should not be empty
    assert!(!token.event.sig.is_empty());
    assert!(!token.event.id.is_empty());

    // Event ID, pubkey, and sig should all be hex strings
    assert!(token.event.id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(token.event.pubkey.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(token.event.sig.chars().all(|c| c.is_ascii_hexdigit()));
}
