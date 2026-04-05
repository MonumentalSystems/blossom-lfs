use anyhow::{Context as _, Result};
use std::path::PathBuf;

const DEFAULT_CHUNK_SIZE: usize = 16 * 1024 * 1024;
const DEFAULT_CONCURRENT_UPLOADS: usize = 8;
const DEFAULT_CONCURRENT_DOWNLOADS: usize = 8;
const DEFAULT_AUTH_EXPIRATION: u64 = 3600;

#[derive(Debug, Clone)]
pub struct Config {
    pub server_url: String,
    pub secret_key: [u8; 32],
    pub chunk_size: usize,
    pub max_concurrent_uploads: usize,
    pub max_concurrent_downloads: usize,
    pub auth_expiration: u64,
}

impl Config {
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

        // Try environment variables if no config file found
        Self::from_env()
    }

    fn parse_config(content: &str) -> Result<Self> {
        let mut server_url = None;
        let mut private_key_str = None;
        let mut chunk_size = DEFAULT_CHUNK_SIZE;
        let mut max_concurrent_uploads = DEFAULT_CONCURRENT_UPLOADS;
        let mut max_concurrent_downloads = DEFAULT_CONCURRENT_DOWNLOADS;
        let mut auth_expiration = DEFAULT_AUTH_EXPIRATION;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }

            if line.starts_with('[') {
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
                    "auth-expiration" | "authExpiration" => {
                        if let Ok(v) = value.parse() {
                            auth_expiration = v;
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
            .ok_or_else(|| {
                anyhow::anyhow!("Missing private key (set in config or NOSTR_PRIVATE_KEY env var)")
            })?;

        let secret_key =
            parse_secret_key(&private_key_str).context("Failed to parse Nostr private key")?;

        Ok(Config {
            server_url,
            secret_key,
            chunk_size,
            max_concurrent_uploads,
            max_concurrent_downloads,
            auth_expiration,
        })
    }

    fn from_env() -> Result<Self> {
        let server_url = std::env::var("BLOSSOM_SERVER_URL")
            .or_else(|_| anyhow::bail!("Missing BLOSSOM_SERVER_URL environment variable"))?;

        let private_key_str = std::env::var("NOSTR_PRIVATE_KEY")
            .or_else(|_| anyhow::bail!("Missing NOSTR_PRIVATE_KEY environment variable"))?;

        let secret_key =
            parse_secret_key(&private_key_str).context("Failed to parse Nostr private key")?;

        Ok(Config {
            server_url,
            secret_key,
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_concurrent_uploads: DEFAULT_CONCURRENT_UPLOADS,
            max_concurrent_downloads: DEFAULT_CONCURRENT_DOWNLOADS,
            auth_expiration: DEFAULT_AUTH_EXPIRATION,
        })
    }
}

fn parse_secret_key(key: &str) -> Result<[u8; 32]> {
    let key = key.trim();

    // Handle nsec format (bech32)
    if key.starts_with("nsec1") {
        // For now, just strip the prefix and decode as lowercase hex
        // In production, should use proper bech32 decoding
        let hex_part = key
            .strip_prefix("nsec1")
            .ok_or_else(|| anyhow::anyhow!("Invalid nsec format"))?;
        hex::decode(hex_part)
            .map_err(|e| anyhow::anyhow!("Failed to decode hex: {}", e))?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid secret key length"))
    } else {
        // Treat as hex
        hex::decode(key)
            .map_err(|e| anyhow::anyhow!("Failed to decode hex: {}", e))?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid secret key length: expected 32 bytes"))
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
