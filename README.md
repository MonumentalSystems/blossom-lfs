# BlossomLFS

Git LFS daemon (v0.4.0) for [Blossom](https://github.com/hzrd149/blossom) blob storage.

A local HTTP server on `localhost:31921` handles all Git LFS operations — vanilla `git lfs` talks to it directly. No custom transfer agent configuration needed.

Built on [blossom-rs](https://crates.io/crates/blossom-rs) for HTTP client, Nostr authentication, and optional iroh QUIC transport.

## Features

- **Pure HTTP daemon** — vanilla `git lfs` talks to `localhost:31921`, no special config
- **BUD-20 compression** — zstd compression + xdelta3 delta encoding for LFS blobs (server-side)
- **BUD-19 file locking** — full Git LFS lock protocol with ownership enforcement and admin override
- **BUD-17 chunked storage** — automatic chunking with Merkle tree integrity for large files
- **Deduplication** — skips uploading blobs that already exist on the server
- **Nostr authentication** (BIP-340 Schnorr, kind-24242 events)
- **Pluggable transport** — HTTP (default) or iroh QUIC peer-to-peer
- **Structured tracing** — OTEL-style semantic fields, optional JSON output

## Installation

```bash
cargo install blossom-lfs

# Or build from source
git clone https://github.com/MonumentalSystems/blossom-lfs.git
cd blossom-lfs
cargo build --release

# With iroh QUIC transport
cargo build --release --features iroh
```

## Quick Start

### 1. Start the daemon

```bash
blossom-lfs daemon
# Listening on http://127.0.0.1:31921
```

### 2. Configure your repo

```bash
cd /path/to/your/repo
blossom-lfs setup
```

This sets `lfs.url`, `lfs.locksurl`, and `lfs.locksverify` in your git config, pointing to the daemon.

### 3. Create per-repo Blossom config

Create `.lfsdalconfig` in your repo root:

```ini
server=https://your-blossom-server.com
private-key=nsec1...
```

Or use environment variables:

```bash
export BLOSSOM_SERVER_URL="https://your-blossom-server.com"
export NOSTR_PRIVATE_KEY="nsec1..."
```

**Security**: Never commit your private key. Use environment variables or `.git/config` (not tracked).

### 4. Use git lfs normally

```bash
git lfs track "*.bin"
git add .gitattributes
git add large-file.bin
git commit -m "Add large file"
git push

# Locking
git lfs lock large-file.bin
git lfs unlock large-file.bin
```

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

## Architecture

```
git lfs (vanilla) → HTTP → localhost:31921/lfs/<b64>/{objects,locks}
                              ↓
                        blossom-lfs daemon (stateless)
                        1. base64url-decode → /path/to/repo
                        2. Config::from_repo_path(path) — reads .lfsdalconfig
                        3. Derive repo slug from git remote
                        4. Forward to Blossom server with Nostr auth
                              ↓
                        Blossom server (HTTP or iroh QUIC)
```

## Configuration

### `.lfsdalconfig`

```ini
server=https://your-blossom-server.com
private-key=nsec1...           # Nostr private key (nsec or hex)
chunk-size=16777216            # 16 MB (optional, default)
transport=http                 # http (default) or iroh
daemon-port=31921              # (optional, default)
```

### Environment Variables

```bash
BLOSSOM_SERVER_URL     # Blossom server URL
NOSTR_PRIVATE_KEY      # Nostr private key
BLOSSOM_TRANSPORT      # http or iroh
BLOSSOM_DAEMON_PORT    # daemon listen port (default 31921)
```

### iroh QUIC Transport

```ini
server=<iroh-endpoint-id>      # base32-encoded iroh endpoint ID
transport=iroh
private-key=nsec1...
```

## Logging

```bash
# Human-readable to stderr
blossom-lfs daemon --log-level debug

# JSON for observability
blossom-lfs daemon --log-json --log-level info

# Log to file
blossom-lfs daemon --log-output /tmp/blossom-lfs.log
```

## Development

```bash
cargo test                                                # 92 tests (5 ignored)
cargo test --test live_server_tests -- --ignored          # needs BLOSSOM_TEST_SERVER + BLOSSOM_TEST_NSEC
cargo test --test git_lfs_e2e_tests -- --ignored          # needs git-lfs binary
cargo fmt --check                                         # CI checks this
cargo clippy                                              # CI checks this
```

## Module Overview

| Module | Purpose |
|---|---|
| `daemon` | Axum HTTP server, Git LFS wire protocol (batch, upload, download, verify, locks) |
| `lock_client` | HTTP client for Blossom BUD-19 lock endpoints with Nostr auth |
| `transport` | Enum dispatch over HTTP (`BlossomClient`) and QUIC (`IrohBlossomClient`) |
| `chunking` | File splitting, Merkle tree, manifest serialization |
| `config` | Loads from `.lfsdalconfig` → `.git/config` → env vars |

## Blossom Protocol Support

- [BUD-01](https://github.com/hzrd149/blossom/blob/master/buds/01.md) — Server requirements
- [BUD-02](https://github.com/hzrd149/blossom/blob/master/buds/02.md) — Blob upload
- [BUD-17](https://github.com/MonumentalSystems/blossom-rs/blob/feature/bud-17-19-lfs-locking/docs/BUD-17.md) — Chunked storage
- [BUD-19](https://github.com/MonumentalSystems/blossom-rs/blob/feature/bud-17-19-lfs-locking/docs/BUD-19.md) — LFS file locking
- [BUD-20](https://github.com/MonumentalSystems/blossom-rs/blob/feature/bud-17-19-lfs-locking/docs/BUD-20.md) — LFS-aware storage efficiency (zstd + xdelta3)

## License

MIT

## Credits

Based on [lfs-dal](https://github.com/regen100/lfs-dal) by regen100.
