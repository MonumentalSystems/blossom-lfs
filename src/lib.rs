//! # blossom-lfs
//!
//! A Git LFS custom transfer agent that stores large files on
//! [Blossom](https://github.com/hzrd149/blossom) blob servers with automatic
//! chunking, Merkle-tree integrity verification, and Nostr (BIP-340)
//! authentication.
//!
//! ## Overview
//!
//! `blossom-lfs` acts as a bridge between Git LFS and Blossom blob storage.
//! When Git LFS needs to upload or download a large file, it delegates to this
//! agent, which handles:
//!
//! - **Chunked transfers** — files larger than the configured chunk size
//!   (default 16 MB) are split into chunks, each uploaded independently.
//! - **Merkle-tree verification** — a manifest containing chunk hashes and a
//!   Merkle root is uploaded alongside the chunks, enabling integrity checks on
//!   download.
//! - **Deduplication** — before uploading, the agent checks whether a blob
//!   already exists on the server (via HEAD request) and skips redundant uploads.
//! - **Nostr authentication** — all server requests are signed with BIP-340
//!   Schnorr signatures using kind-24242 events, handled by the
//!   [`blossom-rs`](https://crates.io/crates/blossom-rs) client.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────┐  stdin/stdout  ┌───────┐  HTTP  ┌────────────────┐
//! │  Git LFS │ ◄────────────► │ Agent │ ◄────► │ Blossom Server │
//! └──────────┘   JSON lines   └───────┘        └────────────────┘
//!                                 │
//!                          ┌──────┴──────┐
//!                          │  Chunking   │
//!                          │  + Merkle   │
//!                          └─────────────┘
//! ```
//!
//! ## Modules
//!
//! - [`agent`] — Git LFS transfer protocol handler (init, upload, download,
//!   terminate).
//! - [`chunking`] — File splitting, Merkle-tree construction, and manifest
//!   serialization.
//! - [`config`] — Configuration loading from `.lfsdalconfig`, `.git/config`, or
//!   environment variables.
//! - [`error`] — Typed error definitions.
//! - [`protocol`] — Git LFS custom transfer protocol message types.
//! - [`transport`] — Pluggable transport layer (HTTP and optional iroh QUIC).

pub mod agent;
pub mod chunking;
pub mod config;
pub mod daemon;
pub mod error;
pub mod lock_client;
pub mod protocol;
pub mod transport;

pub use agent::Agent;
pub use config::Config;
pub use daemon::run_daemon;
pub use error::{BlossomLfsError, Result};
