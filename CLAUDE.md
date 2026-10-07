# CLAUDE.md

## Project Overview

blossom-lfs is a Git LFS daemon (v0.6.1) that bridges vanilla `git lfs` with Blossom blob storage. A local HTTP server on `localhost:31921` handles all Git LFS operations: batch, upload, download, verify, and locks. No custom transfer agent configuration needed.

Requires Rust 1.94.1 or newer. Package metadata in `Cargo.toml` and the committed `Cargo.lock` define the release and dependency versions.

## Build & Test

```bash
cargo build --locked                      # default (HTTP only)
cargo build --locked --features iroh      # with iroh QUIC transport
cargo test --locked                       # HTTP tests; external tests are ignored
cargo test --locked --features iroh       # includes QUIC integration tests
cargo test --locked --test live_server_tests -- --ignored  # requires BLOSSOM_TEST_SERVER + BLOSSOM_TEST_NSEC env vars
cargo test --locked --test git_lfs_e2e_tests -- --ignored  # requires git-lfs binary installed
cargo fmt --check                # CI checks this
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --features iroh -- -D warnings
cargo doc --locked --no-deps --all-features # generate rustdocs
```

## Architecture

- **`src/daemon.rs`** — Local axum HTTP server. Handles all Git LFS wire protocol: batch, streaming download/upload, verify, and lock API. Base64url-decodes repo filesystem path from URL, reads per-repo Blossom config, forwards lock requests to Blossom BUD-19 endpoints.
- **`src/lock_client.rs`** — HTTP client for Blossom BUD-19 lock endpoints with Nostr auth signing.
- **`src/transport.rs`** — Wrapper around `MultiTransportClient` (HTTP and optional iroh QUIC). Created per-request from per-repo config.
- **`src/chunking/`** — File splitting (`Chunker`), Merkle tree (`MerkleTree`), manifest serialization (`Manifest`).
- **`src/config.rs`** — Loads from `.lfsdalconfig` → `.git/config` → env vars. Supports `transport = http | iroh`, chunk sizing, and concurrency settings. The CLI daemon port is set by `--port`.
- **`src/error.rs`** — `BlossomLfsError` enum, `Result<T>` alias.
- **`src/main.rs`** — CLI entry point (clap) with `daemon`, `setup`, `install`, `uninstall`, and `clone` subcommands. Logging options precede the subcommand.

## Daemon Routes

```
POST /lfs/<b64>/objects/batch           Git LFS batch API (basic transfer)
GET  /lfs/<b64>/objects/<oid>           Streaming download (chunked reassembly)
PUT  /lfs/<b64>/objects/<oid>           Streaming upload (chunking pipeline)
POST /lfs/<b64>/objects/<oid>/verify    Post-upload verify (HEAD check)
POST /lfs/<b64>/locks                   Create lock  (→ Blossom BUD-19)
GET  /lfs/<b64>/locks                   List locks   (→ Blossom BUD-19)
POST /lfs/<b64>/locks/verify            Verify locks (→ Blossom BUD-19)
POST /lfs/<b64>/locks/<id>/unlock       Unlock       (→ Blossom BUD-19)
```

## Key Design Decisions

- **Local HTTP, no custom agent**: Vanilla `git lfs` talks to `localhost:31921` after `setup` configures `lfs.url`, `lfs.locksurl`, and `lfs.locksverify`. Upstream requests use HTTP or optional iroh.
- **Stateless daemon**: Each request loads per-repo config via `Config::from_repo_path()`. No cached state.
- **Streaming**: Downloads use `Body::from_stream()` for chunked reassembly. Uploads stream to tempfile, then chunk and upload.
- **blossom-rs as the Blossom layer**: All HTTP client, Nostr auth, and protocol types come from `blossom-rs` (v0.6.1).
- **BUD-20 compression**: Daemon sends `["t","lfs"]` + `["path",...]` + `["repo",...]` tags in upload auth events. Server applies zstd/xdelta3 transparently.
- **Dedup via `exists()`**: Before every upload, HEAD-check the server and skip if the blob is already there.
- **MultiTransportClient wrapper**: `Transport` wraps blossom-rs `MultiTransportClient`; iroh handles uploads and HTTP handles downloads with fallback unless a transport is forced.
- **Structured tracing**: OTEL-style semantic fields (`blob.oid`, `blob.size`, `chunk.sha256`).
- **Request-bound lock auth**: Fresh kind-24242 events bind the server, decoded lock route, and HTTP method. blossom-rs handles request binding for blob and iroh operations.
- **Configuration precedence**: First value wins: `.lfsdalconfig`, `.git/config`, environment. Keep private keys out of tracked files, and put comments on their own lines.
- **Custom ports**: Start the daemon with `--port`; `setup` and `clone` read `BLOSSOM_DAEMON_PORT`. These values must match.

## Feature Flags

- `default` — HTTP transport only
- `iroh` — Adds iroh QUIC transport, enables `blossom-rs/iroh-transport`, `blossom-rs/pkarr-discovery`, and the direct `iroh` dependency. Also enables `blossom-rs/server` because its 0.6.1 iroh module references server types and validation helpers.

## Test Structure

- `tests/daemon_tests.rs` — Batch/upload/download/verify with a mock Blossom server.
- `tests/lock_tests.rs` — Lock client and daemon lock proxy with a mock server.
- `tests/lock_integration_tests.rs` — Real blossom-rs server: conflict, ownership, admin unlock, verification, lifecycle, and missing locks.
- `tests/bud20_integration_tests.rs` — Compression, full LFS workflow, deduplication, and chunked round trips.
- `tests/concurrent_tests.rs` — Parallel uploads and lock contention.
- `tests/cross_repo_lock_tests.rs` — Lock isolation between repositories.
- `tests/iroh_integration_tests.rs` — Dual transport, HTTP fallback, and iroh-only blobs and locks; requires the `iroh` feature.
- `tests/git_lfs_e2e_tests.rs` — Real Git LFS push/pull; ignored by default, requires the `git-lfs` binary.
- `tests/auth_tests.rs` — blossom-rs authentication and signing.
- `tests/chunker_tests.rs`, `tests/merkle_tests.rs`, `tests/manifest_tests.rs` — Chunking, hashes, Merkle proofs, and manifests.
- `tests/integration_tests.rs`, `tests/e2e_tests.rs` — Client and full workflow tests with mock servers.
- `tests/chunked_streaming_tests.rs` — Property tests for chunked upload/download.
- `tests/live_server_tests.rs` — External server tests, ignored by default; requires test server and key environment variables.

## Dependencies

- `blossom-rs` — v0.6.1 from crates.io; package version stays aligned with this dependency
