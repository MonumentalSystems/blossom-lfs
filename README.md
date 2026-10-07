# blossom-lfs

Git LFS daemon for [Blossom](https://github.com/hzrd149/blossom) blob storage.

A local HTTP server on `localhost:31921` handles all Git LFS operations — vanilla `git lfs` talks to it directly. No custom transfer agent configuration needed.

[![CI](https://github.com/MonumentalSystems/blossom-lfs/actions/workflows/ci.yml/badge.svg)](https://github.com/MonumentalSystems/blossom-lfs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/blossom-lfs.svg)](https://crates.io/crates/blossom-lfs)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Version `0.6.1`, matching [blossom-rs 0.6.1](https://crates.io/crates/blossom-rs/0.6.1). Built on blossom-rs for HTTP client, Nostr authentication, and optional iroh QUIC transport.

## Features

- **Local HTTP daemon** — vanilla `git lfs` on `localhost:31921`, configured by `blossom-lfs setup`
- **BUD-20 compression** — zstd compression + xdelta3 delta encoding (server-side)
- **BUD-19 file locking** — full Git LFS lock protocol with ownership enforcement
- **BUD-17 chunked storage** — automatic chunking with Merkle tree integrity
- **Deduplication** — skips uploading blobs that already exist on the server
- **Nostr authentication** (BIP-340 Schnorr, kind-24242 events)
- **Pluggable transport** — HTTP (default) or iroh QUIC peer-to-peer
- **Structured tracing** — OTEL-style semantic fields, optional JSON output

## Getting Started

### 1. Install

Requires Rust **1.94.1 or newer**, Git, and Git LFS. The default build uses HTTP.

```bash
cargo install blossom-lfs --locked
```

Or build from source:

```bash
git clone https://github.com/MonumentalSystems/blossom-lfs.git
cd blossom-lfs
cargo build --release --locked
# Binary at target/release/blossom-lfs
```

To enable iroh QUIC transport, install or build with the `iroh` feature:

```bash
cargo install blossom-lfs --locked --features iroh
# Or, from the source checkout:
cargo build --release --locked --features iroh
```

When building from source, use `target/release/blossom-lfs` or install it onto your PATH with `cargo install --path . --locked` (add `--features iroh` for QUIC).

Then run the installer to check prerequisites and set up git-lfs:

```bash
blossom-lfs install
```

This will:

- Verify `git` and `git-lfs` are installed (attempts to install `git-lfs` if missing)
- Run `git lfs install` to set up global hooks
- Show next steps

### 2. Start the daemon

Run it in the foreground:

```bash
blossom-lfs daemon
```

Or install as a background service that starts on login:

```bash
blossom-lfs install --service
```

This creates:

- **macOS**: `~/Library/LaunchAgents/com.monumentalsystems.blossom-lfs.plist` (launchd)
- **Linux**: `~/.config/systemd/user/blossom-lfs.service` (systemd)

For a custom port, use the same value for the daemon and repository setup:

```bash
export BLOSSOM_DAEMON_PORT=8080
blossom-lfs daemon --port 8080
# In another shell with BLOSSOM_DAEMON_PORT=8080:
blossom-lfs setup
```

The daemon uses `--port` or 31921; `setup` and `clone` read `BLOSSOM_DAEMON_PORT`. For a background service, use `blossom-lfs install --service --port 8080`.

### 3. Clone a repo

```bash
blossom-lfs clone https://github.com/your-org/your-repo.git
```

This wraps `git clone` and handles all LFS bootstrapping automatically:

1. Clones the repo (skipping LFS downloads initially)
2. Configures `lfs.url` to point at the local daemon
3. Pulls all LFS objects through the daemon

All standard `git clone` flags work:

```bash
blossom-lfs clone --recurse-submodules --depth 1 git@github.com:org/repo.git mydir
```

### 4. Set up an existing repo

If you already have a cloned repo:

```bash
cd /path/to/your/repo
blossom-lfs setup
```

This sets `lfs.url`, `lfs.locksurl`, and `lfs.locksverify` in `.git/config`.

### 5. Per-repo Blossom config

Create `.lfsdalconfig` in your repo root (this is typically tracked in the repo):

```ini
server=https://your-blossom-server.com
```

Keep the private key in your local environment. You can also supply the server URL there:

```bash
export BLOSSOM_SERVER_URL="https://your-blossom-server.com"
export NOSTR_PRIVATE_KEY="nsec1..."
```

Downloads do not require a private key. Uploads and lock operations require `NOSTR_PRIVATE_KEY` (nsec or 64-character hex), or a `private-key` entry in the local `.git/config`. Never put a private key in a tracked `.lfsdalconfig`.

Configuration fills each field from `.lfsdalconfig`, then `.git/config`, then environment variables; the first value wins. Environment variables do not override values already set in a file.

### 6. Use git-lfs normally

```bash
git lfs track "*.bin"
git add .gitattributes large-file.bin
git commit -m "Add large file"
git push

# Locking
git lfs lock large-file.bin
git lfs unlock large-file.bin
```

## CLI Reference

| Command | Description |
|---|---|
| `blossom-lfs install` | Check prerequisites, install git-lfs if needed |
| `blossom-lfs install --service` | Also install daemon as a background service |
| `blossom-lfs daemon` | Start the LFS daemon (foreground) |
| `blossom-lfs daemon --port 8080` | Start on a custom port |
| `blossom-lfs setup` | Configure current repo to use the daemon |
| `blossom-lfs uninstall` | Remove the daemon background service |
| `blossom-lfs clone <url> [dir]` | Clone + setup + LFS pull in one step |

## Configuration

### `.lfsdalconfig`

```ini
server=https://your-blossom-server.com
# Default chunk size: 16 MiB
chunk-size=16777216
# Defaults: 8 concurrent uploads and downloads
max-concurrent-uploads=8
max-concurrent-downloads=8

# Optional: iroh uploads with HTTP downloads and automatic fallback.
# Requires a binary built with --features iroh.
# iroh-endpoint=<iroh-endpoint-id>

# Optional: force a transport (http or iroh).
# transport=http
```

Use comments on their own lines; the config parser does not strip inline comments. A server URL is required unless both `transport=iroh` and `iroh-endpoint` are set. For iroh-only operations, configure a private key locally.

### Environment Variables

| Variable | Description |
|---|---|
| `BLOSSOM_SERVER_URL` | Blossom server URL |
| `NOSTR_PRIVATE_KEY` | Nostr private key (nsec or hex) |
| `BLOSSOM_DAEMON_PORT` | Port used by `setup` and `clone` (default: 31921); start the daemon with matching `--port` |
| `BLOSSOM_IROH_ENDPOINT` | iroh endpoint ID (optional) |
| `BLOSSOM_TRANSPORT` | Force `http` or `iroh` (optional) |

## Architecture

```
git lfs (vanilla) --> HTTP --> localhost:31921/lfs/<b64>/{objects,locks}
                                |
                          blossom-lfs daemon (stateless)
                          1. base64url-decode --> /path/to/repo
                          2. Config::from_repo_path() -- merges files and environment
                          3. Derive repo slug from git remote
                          4. Forward to Blossom server with Nostr auth
                                |
                          Blossom server (HTTP or iroh QUIC)
```

### Daemon Routes

```
POST /lfs/<b64>/objects/batch           Git LFS batch API (basic transfer)
GET  /lfs/<b64>/objects/<oid>           Streaming download (chunked reassembly)
PUT  /lfs/<b64>/objects/<oid>           Streaming upload (chunking pipeline)
POST /lfs/<b64>/objects/<oid>/verify    Post-upload verify (HEAD check)
POST /lfs/<b64>/locks                   Create lock  (BUD-19)
GET  /lfs/<b64>/locks                   List locks   (BUD-19)
POST /lfs/<b64>/locks/verify            Verify locks (BUD-19)
POST /lfs/<b64>/locks/<id>/unlock       Unlock       (BUD-19)
```

## Logging

```bash
blossom-lfs --log-level debug daemon            # verbose
blossom-lfs --log-json --log-level info daemon  # JSON for observability
blossom-lfs --log-output /tmp/blossom.log daemon
```

## Development

```bash
cargo test --locked
cargo test --locked --features iroh                       # also runs QUIC integration tests
cargo test --locked --test live_server_tests -- --ignored          # needs BLOSSOM_TEST_SERVER + BLOSSOM_TEST_NSEC
cargo test --locked --test git_lfs_e2e_tests -- --ignored          # needs git-lfs binary
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --features iroh -- -D warnings
cargo doc --locked --no-deps --all-features
```

## Upgrading to 0.6.1

This release updates the direct dependencies to their current stable versions and raises the minimum Rust version to 1.94.1, matching blossom-rs. HTTP lock requests now sign fresh authorization events bound to the server, route, and method, as required by blossom-rs 0.6.x. The optional QUIC transport uses iroh 1.x. Unlocking another user's lock now requires an admin signer and an explicit `force=true` request.

Use HTTPS for remote Blossom servers and review the [blossom-rs upgrade notes](https://github.com/MonumentalSystems/blossom-rs/blob/v0.6.1/RELEASE_NOTES.md) before upgrading a server. The local Git LFS daemon continues to listen on loopback HTTP.

## Blossom Protocol Support

- [BUD-01](https://github.com/hzrd149/blossom/blob/master/buds/01.md) — Server requirements
- [BUD-02](https://github.com/hzrd149/blossom/blob/master/buds/02.md) — Blob upload
- [BUD-17](https://github.com/MonumentalSystems/blossom-rs/blob/master/docs/BUD-17.md) — Chunked storage
- [BUD-19](https://github.com/MonumentalSystems/blossom-rs/blob/master/docs/BUD-19.md) — LFS file locking
- [BUD-20](https://github.com/MonumentalSystems/blossom-rs/blob/master/docs/BUD-20.md) — LFS-aware storage efficiency

## License

MIT

## Credits

Based on [lfs-dal](https://github.com/regen100/lfs-dal) by regen100.
