//! Configuration loading for the blossom-lfs agent.
//!
//! Configuration is resolved in priority order:
//!
//! 1. `.lfsdalconfig` in the repository root (INI format).
//! 2. `.git/config` (INI format, under `[lfs-dal]`).
//! 3. Environment variables (`BLOSSOM_SERVER_URL`, `NOSTR_PRIVATE_KEY`).
//!
//! Private keys may be provided as either a 64-character hex string or a
//! Bech32-encoded `nsec1…` value.

use anyhow::{Context as _, Result};
use std::path::PathBuf;

/// Default chunk size: 16 MiB.
const DEFAULT_CHUNK_SIZE: usize = 16 * 1024 * 1024;
const DEFAULT_CONCURRENT_UPLOADS: usize = 8;
const DEFAULT_CONCURRENT_DOWNLOADS: usize = 8;

/// Runtime configuration for the LFS agent.
#[derive(Debug, Clone)]
pub struct Config {
    /// Base URL of the Blossom server (e.g. `https://blossom.example.com`).
    pub server_url: String,
    /// Nostr private key as a 64-character hex string.
    pub secret_key_hex: String,
    /// Maximum bytes per chunk (default 16 MiB).
    pub chunk_size: usize,
    /// Maximum number of concurrent chunk uploads.
    pub max_concurrent_uploads: usize,
    /// Maximum number of concurrent chunk downloads.
    pub max_concurrent_downloads: usize,
}

impl Config {
    /// Load configuration from git config files or environment variables.
    ///
    /// Tries `.lfsdalconfig`, then `.git/config`, then falls back to
    /// environment variables.
    pub fn from_git_config() -> Result<Self> {
        let config_paths = vec![PathBuf::from(".lfsdalconfig"), PathBuf::from(".git/config")];

        for path in config_paths {
            if path.exists() {
                let config_content = std::fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read config file: {:?}", path))?;

                if let Ok(config) = Self::parse_config(&config_content) {
                    return Ok(config);
                }
            }
        }

        Self::from_env()
    }

    fn parse_config(content: &str) -> Result<Self> {
        let mut server_url = None;
        let mut private_key_str = None;
        let mut chunk_size = DEFAULT_CHUNK_SIZE;
        let mut max_concurrent_uploads = DEFAULT_CONCURRENT_UPLOADS;
        let mut max_concurrent_downloads = DEFAULT_CONCURRENT_DOWNLOADS;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() || line.starts_with('[') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');

                match key {
                    "server" => server_url = Some(value.to_string()),
                    "private-key" | "privateKey" => private_key_str = Some(value.to_string()),
                    "chunk-size" | "chunkSize" => {
                        if let Ok(v) = value.parse() {
                            chunk_size = v;
                        }
                    }
                    "max-concurrent-uploads" | "maxConcurrentUploads" => {
                        if let Ok(v) = value.parse() {
                            max_concurrent_uploads = v;
                        }
                    }
                    "max-concurrent-downloads" | "maxConcurrentDownloads" => {
                        if let Ok(v) = value.parse() {
                            max_concurrent_downloads = v;
                        }
                    }
                    _ => {}
                }
            }
        }

        let server_url =
            server_url.ok_or_else(|| anyhow::anyhow!("Missing server URL in config"))?;

        let private_key_str = private_key_str
            .or_else(|| std::env::var("NOSTR_PRIVATE_KEY").ok())
            .ok_or_else(|| anyhow::anyhow!("Missing private key"))?;

        let secret_key_hex = normalize_to_hex(&private_key_str)?;

        Ok(Config {
            server_url,
            secret_key_hex,
            chunk_size,
            max_concurrent_uploads,
            max_concurrent_downloads,
        })
    }

    fn from_env() -> Result<Self> {
        let server_url = std::env::var("BLOSSOM_SERVER_URL")
            .or_else(|_| anyhow::bail!("Missing BLOSSOM_SERVER_URL environment variable"))?;

        let private_key_str = std::env::var("NOSTR_PRIVATE_KEY")
            .or_else(|_| anyhow::bail!("Missing NOSTR_PRIVATE_KEY environment variable"))?;

        let secret_key_hex = normalize_to_hex(&private_key_str)?;

        Ok(Config {
            server_url,
            secret_key_hex,
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_concurrent_uploads: DEFAULT_CONCURRENT_UPLOADS,
            max_concurrent_downloads: DEFAULT_CONCURRENT_DOWNLOADS,
        })
    }
}

/// Convert nsec or hex private key string to hex.
fn normalize_to_hex(key: &str) -> Result<String> {
    let key = key.trim();

    if key.starts_with("nsec1") {
        let secret_key = nostr::SecretKey::parse(key)
            .map_err(|e| anyhow::anyhow!("Failed to parse nsec: {}", e))?;
        Ok(hex::encode(secret_key.secret_bytes()))
    } else {
        // Validate it's valid hex and correct length
        let bytes = hex::decode(key)
            .map_err(|e| anyhow::anyhow!("Failed to decode hex: {}", e))?;
        if bytes.len() != 32 {
            anyhow::bail!("Invalid secret key length: expected 32 bytes");
        }
        Ok(key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_chunk_size() {
        assert_eq!(DEFAULT_CHUNK_SIZE, 16 * 1024 * 1024);
    }
}
