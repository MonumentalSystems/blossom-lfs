//! Configuration loading for the blossom-lfs daemon.
//!
//! Configuration is merged from multiple sources (first non-None wins per field):
//!
//! 1. `.lfsdalconfig` in the repository root (INI format, safe to commit without key).
//! 2. `.git/config` (INI format, local to repo, not tracked).
//! 3. Environment variables (`BLOSSOM_SERVER_URL`, `NOSTR_PRIVATE_KEY`, etc.).
//!
//! This allows `.lfsdalconfig` to contain the server URL (committable) while
//! the private key comes from `.git/config` or environment variables.
//!
//! Private keys may be provided as either a 64-character hex string or a
//! Bech32-encoded `nsec1…` value.

use anyhow::{Context as _, Result};
use std::path::PathBuf;

/// Default chunk size: 16 MiB.
const DEFAULT_CHUNK_SIZE: usize = 16 * 1024 * 1024;
const DEFAULT_CONCURRENT_UPLOADS: usize = 8;
const DEFAULT_CONCURRENT_DOWNLOADS: usize = 8;
const DEFAULT_DAEMON_PORT: u16 = 31921;

/// Runtime configuration for the LFS agent.
#[derive(Debug, Clone)]
pub struct Config {
    pub server_url: Option<String>,
    pub iroh_endpoint: Option<String>,
    pub secret_key_hex: String,
    pub chunk_size: usize,
    pub max_concurrent_uploads: usize,
    pub max_concurrent_downloads: usize,
    pub force_transport: Option<ForceTransport>,
    pub daemon_port: u16,
}

/// Force a specific transport for all operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceTransport {
    Http,
    Iroh,
}

/// Intermediate config fields that can be merged from multiple sources.
/// Each field is Option — first source to set a field wins.
#[derive(Default)]
struct ConfigFields {
    server_url: Option<String>,
    iroh_endpoint: Option<String>,
    private_key_str: Option<String>,
    chunk_size: Option<usize>,
    max_concurrent_uploads: Option<usize>,
    max_concurrent_downloads: Option<usize>,
    force_transport: Option<ForceTransport>,
    daemon_port: Option<u16>,
}

impl ConfigFields {
    /// Merge key-value pairs from a config file (INI format).
    /// Only fills fields that are still None.
    fn merge_from_content(&mut self, content: &str) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() || line.starts_with('[') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');

                match key {
                    "server" if self.server_url.is_none() => {
                        self.server_url = Some(value.to_string());
                    }
                    "iroh-endpoint" | "irohEndpoint" if self.iroh_endpoint.is_none() => {
                        self.iroh_endpoint = Some(value.to_string());
                    }
                    "private-key" | "privateKey" if self.private_key_str.is_none() => {
                        self.private_key_str = Some(value.to_string());
                    }
                    "chunk-size" | "chunkSize" if self.chunk_size.is_none() => {
                        if let Ok(v) = value.parse() {
                            self.chunk_size = Some(v);
                        }
                    }
                    "max-concurrent-uploads" | "maxConcurrentUploads"
                        if self.max_concurrent_uploads.is_none() =>
                    {
                        if let Ok(v) = value.parse() {
                            self.max_concurrent_uploads = Some(v);
                        }
                    }
                    "max-concurrent-downloads" | "maxConcurrentDownloads"
                        if self.max_concurrent_downloads.is_none() =>
                    {
                        if let Ok(v) = value.parse() {
                            self.max_concurrent_downloads = Some(v);
                        }
                    }
                    "transport" if self.force_transport.is_none() => {
                        match value.trim().to_lowercase().as_str() {
                            "iroh" | "quic" => self.force_transport = Some(ForceTransport::Iroh),
                            "http" | "https" => self.force_transport = Some(ForceTransport::Http),
                            _ => {}
                        }
                    }
                    "daemon-port" | "daemonPort" if self.daemon_port.is_none() => {
                        if let Ok(v) = value.parse() {
                            self.daemon_port = Some(v);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Fill remaining None fields from environment variables.
    fn merge_from_env(&mut self) {
        if self.server_url.is_none() {
            self.server_url = std::env::var("BLOSSOM_SERVER_URL").ok();
        }
        if self.private_key_str.is_none() {
            self.private_key_str = std::env::var("NOSTR_PRIVATE_KEY").ok();
        }
        if self.iroh_endpoint.is_none() {
            self.iroh_endpoint = std::env::var("BLOSSOM_IROH_ENDPOINT").ok();
        }
        if self.force_transport.is_none() {
            self.force_transport = std::env::var("BLOSSOM_TRANSPORT").ok().and_then(|v| {
                match v.trim().to_lowercase().as_str() {
                    "iroh" | "quic" => Some(ForceTransport::Iroh),
                    "http" | "https" => Some(ForceTransport::Http),
                    _ => None,
                }
            });
        }
        if self.daemon_port.is_none() {
            self.daemon_port = std::env::var("BLOSSOM_DAEMON_PORT")
                .ok()
                .and_then(|v| v.parse().ok());
        }
    }

    /// Convert to a final Config, failing if required fields are missing.
    fn into_config(self) -> Result<Config> {
        let is_iroh_only =
            self.force_transport == Some(ForceTransport::Iroh) && self.iroh_endpoint.is_some();
        let server_url = if is_iroh_only {
            self.server_url
        } else {
            Some(
                self.server_url
                    .ok_or_else(|| anyhow::anyhow!("Missing server URL in config"))?,
            )
        };

        let private_key_str = self.private_key_str.ok_or_else(|| {
            anyhow::anyhow!(
                "Missing private key — set in .lfsdalconfig, .git/config, or NOSTR_PRIVATE_KEY env"
            )
        })?;

        let secret_key_hex = normalize_to_hex(&private_key_str)?;

        Ok(Config {
            server_url,
            iroh_endpoint: self.iroh_endpoint,
            secret_key_hex,
            chunk_size: self.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE),
            max_concurrent_uploads: self
                .max_concurrent_uploads
                .unwrap_or(DEFAULT_CONCURRENT_UPLOADS),
            max_concurrent_downloads: self
                .max_concurrent_downloads
                .unwrap_or(DEFAULT_CONCURRENT_DOWNLOADS),
            force_transport: self.force_transport,
            daemon_port: self.daemon_port.unwrap_or(DEFAULT_DAEMON_PORT),
        })
    }
}

impl Config {
    /// Load configuration from a specific repo directory path.
    ///
    /// Merges config from multiple sources (fields filled in priority order):
    /// 1. `.lfsdalconfig` in the repo root
    /// 2. `.git/config` in the repo
    /// 3. Environment variables
    pub fn from_repo_path(repo_path: &std::path::Path) -> Result<Self> {
        let mut fields = ConfigFields::default();

        let lfsdalconfig = repo_path.join(".lfsdalconfig");
        if lfsdalconfig.exists() {
            let content = std::fs::read_to_string(&lfsdalconfig)
                .with_context(|| format!("Failed to read config: {:?}", lfsdalconfig))?;
            fields.merge_from_content(&content);
        }

        let git_config = repo_path.join(".git/config");
        if git_config.exists() {
            let content = std::fs::read_to_string(&git_config)
                .with_context(|| format!("Failed to read config: {:?}", git_config))?;
            fields.merge_from_content(&content);
        }

        fields.merge_from_env();
        fields.into_config()
    }

    /// Load configuration from git config files or environment variables.
    ///
    /// Same merge logic as `from_repo_path` but uses current directory.
    pub fn from_git_config() -> Result<Self> {
        let mut fields = ConfigFields::default();

        let lfsdalconfig = PathBuf::from(".lfsdalconfig");
        if lfsdalconfig.exists() {
            if let Ok(content) = std::fs::read_to_string(&lfsdalconfig) {
                fields.merge_from_content(&content);
            }
        }

        let git_config = PathBuf::from(".git/config");
        if git_config.exists() {
            if let Ok(content) = std::fs::read_to_string(&git_config) {
                fields.merge_from_content(&content);
            }
        }

        fields.merge_from_env();
        fields.into_config()
    }
}

fn normalize_to_hex(key: &str) -> Result<String> {
    let key = key.trim();

    if key.starts_with("nsec1") {
        let secret_key = nostr::SecretKey::parse(key)
            .map_err(|e| anyhow::anyhow!("Failed to parse nsec: {}", e))?;
        Ok(hex::encode(secret_key.secret_bytes()))
    } else {
        let bytes = hex::decode(key).map_err(|e| anyhow::anyhow!("Failed to decode hex: {}", e))?;
        if bytes.len() != 32 {
            anyhow::bail!("Invalid secret key length: expected 32 bytes");
        }
        Ok(key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse a single config string.
    fn parse(content: &str) -> Result<Config> {
        let mut fields = ConfigFields::default();
        fields.merge_from_content(content);
        fields.into_config()
    }

    #[test]
    fn test_default_chunk_size() {
        assert_eq!(DEFAULT_CHUNK_SIZE, 16 * 1024 * 1024);
    }

    #[test]
    fn test_parse_config_basic() {
        let config = parse(
            "server=https://blossom.example.com\n\
             private-key=0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        assert_eq!(
            config.server_url,
            Some("https://blossom.example.com".to_string())
        );
        assert!(config.iroh_endpoint.is_none());
        assert!(config.force_transport.is_none());
        assert_eq!(config.chunk_size, DEFAULT_CHUNK_SIZE);
    }

    #[test]
    fn test_parse_config_custom_values() {
        let config = parse(
            "server=https://blossom.example.com\n\
             private-key=0000000000000000000000000000000000000000000000000000000000000001\n\
             chunk-size=4096\n\
             max-concurrent-uploads=4\n\
             max-concurrent-downloads=2\n\
             daemon-port=9999",
        )
        .unwrap();
        assert_eq!(config.chunk_size, 4096);
        assert_eq!(config.max_concurrent_uploads, 4);
        assert_eq!(config.max_concurrent_downloads, 2);
        assert_eq!(config.daemon_port, 9999);
    }

    #[test]
    fn test_parse_config_with_sections_and_comments() {
        let config = parse(
            "# this is a comment\n\
             [lfs-dal]\n\
             \n\
             server=https://blossom.example.com\n\
             private-key=0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        assert_eq!(
            config.server_url,
            Some("https://blossom.example.com".to_string())
        );
    }

    #[test]
    fn test_parse_config_quoted_values() {
        let config = parse(
            "server=\"https://blossom.example.com\"\n\
             private-key=0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        assert_eq!(
            config.server_url,
            Some("https://blossom.example.com".to_string())
        );
    }

    #[test]
    fn test_parse_config_camel_case() {
        let config = parse(
            "server=https://blossom.example.com\n\
             privateKey=0000000000000000000000000000000000000000000000000000000000000001\n\
             chunkSize=4096\n\
             daemonPort=8080",
        )
        .unwrap();
        assert_eq!(config.chunk_size, 4096);
        assert_eq!(config.daemon_port, 8080);
    }

    #[test]
    fn test_parse_config_missing_server() {
        let result =
            parse("private-key=0000000000000000000000000000000000000000000000000000000000000001");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_config_missing_key() {
        let result = parse("server=https://blossom.example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_from_multiple_sources() {
        let mut fields = ConfigFields::default();
        // First source: server only
        fields.merge_from_content("server=https://blossom.example.com");
        // Second source: key only
        fields.merge_from_content(
            "private-key=0000000000000000000000000000000000000000000000000000000000000001",
        );
        let config = fields.into_config().unwrap();
        assert_eq!(
            config.server_url,
            Some("https://blossom.example.com".to_string())
        );
    }

    #[test]
    fn test_first_source_wins() {
        let mut fields = ConfigFields::default();
        fields.merge_from_content(
            "server=https://first.com\nprivate-key=0000000000000000000000000000000000000000000000000000000000000001",
        );
        fields.merge_from_content(
            "server=https://second.com\nprivate-key=0000000000000000000000000000000000000000000000000000000000000002",
        );
        let config = fields.into_config().unwrap();
        assert_eq!(config.server_url, Some("https://first.com".to_string()));
    }

    #[test]
    fn test_from_repo_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".lfsdalconfig"),
            "server=https://example.com\nprivate-key=0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();

        let config = Config::from_repo_path(dir.path()).unwrap();
        assert_eq!(config.server_url, Some("https://example.com".to_string()));
    }

    #[test]
    fn test_from_repo_path_split_config() {
        let dir = tempfile::tempdir().unwrap();
        // .lfsdalconfig has server only
        std::fs::write(
            dir.path().join(".lfsdalconfig"),
            "server=https://example.com",
        )
        .unwrap();
        // .git/config has the key
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[lfs-dal]\nprivate-key=0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();

        let config = Config::from_repo_path(dir.path()).unwrap();
        assert_eq!(config.server_url, Some("https://example.com".to_string()));
        assert_eq!(
            config.secret_key_hex,
            "0000000000000000000000000000000000000000000000000000000000000001"
        );
    }

    #[test]
    fn test_normalize_hex_key() {
        let hex = "0000000000000000000000000000000000000000000000000000000000000001";
        assert_eq!(normalize_to_hex(hex).unwrap(), hex);
    }

    #[test]
    fn test_normalize_invalid_hex() {
        assert!(normalize_to_hex("not-hex").is_err());
    }

    #[test]
    fn test_normalize_short_hex() {
        assert!(normalize_to_hex("abcd").is_err());
    }

    #[test]
    fn test_iroh_only_no_server() {
        let config = parse(
            "iroh-endpoint=abc123\n\
             transport=iroh\n\
             private-key=0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        assert_eq!(config.force_transport, Some(ForceTransport::Iroh));
        assert!(config.server_url.is_none());
    }
}
