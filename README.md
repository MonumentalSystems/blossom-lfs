# BlossomLFS

Git LFS daemon for [Blossom](https://github.com/hzrd149/blossom) blob storage.

A local HTTP server on `localhost:31921` handles all Git LFS operations — vanilla `git lfs` talks to it directly. No custom transfer agent configuration needed.

Built on [blossom-rs](https://crates.io/crates/blossom-rs) for HTTP client, Nostr authentication, and optional iroh QUIC transport.

## Features

- **Pure HTTP daemon** — vanilla `git lfs` on `localhost:31921`, no special config
- **BUD-20 compression** — zstd compression + xdelta3 delta encoding (server-side)
- **BUD-19 file locking** — full Git LFS lock protocol with ownership enforcement
- **BUD-17 chunked storage** — automatic chunking with Merkle tree integrity
- **Deduplication** — skips uploading blobs that already exist on the server
- **Nostr authentication** (BIP-340 Schnorr, kind-24242 events)
- **Pluggable transport** — HTTP (default) or iroh QUIC peer-to-peer
- **Structured tracing** — OTEL-style semantic fields, optional JSON output

## Getting Started

### 1. Install

```bash
cargo install blossom-lfs
```

Or build from source:

```bash
git clone https://github.com/MonumentalSystems/blossom-lfs.git
cd blossom-lfs
cargo build --release
# Binary at target/release/blossom-lfs
```

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
private-key=nsec1...
```

Or use environment variables:

```bash
export BLOSSOM_SERVER_URL="https://your-blossom-server.com"
export NOSTR_PRIVATE_KEY="nsec1..."
```

**Security**: Never commit your private key. Use environment variables or store in `.git/config` (not tracked) instead.

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
| `blossom-lfs clone <url> [dir]` | Clone + setup + LFS pull in one step |

## Configuration

### `.lfsdalconfig`

```ini
server=https://your-blossom-server.com
private-key=nsec1...           # Nostr private key (nsec or hex)
chunk-size=16777216            # 16 MB (optional, default)
daemon-port=31921              # (optional, default)

# Optional: iroh QUIC for uploads, HTTP for downloads (with fallback)
iroh-endpoint=<iroh-endpoint-id>

# Optional: force single transport
# transport=http               # force all ops through HTTP
# transport=iroh               # force all ops through iroh
```

### Environment Variables

| Variable | Description |
|---|---|
| `BLOSSOM_SERVER_URL` | Blossom server URL |
| `NOSTR_PRIVATE_KEY` | Nostr private key (nsec or hex) |
| `BLOSSOM_DAEMON_PORT` | Daemon listen port (default: 31921) |
| `BLOSSOM_IROH_ENDPOINT` | iroh endpoint ID (optional) |
| `BLOSSOM_TRANSPORT` | Force `http` or `iroh` (optional) |

## Architecture

```
git lfs (vanilla) --> HTTP --> localhost:31921/lfs/<b64>/{objects,locks}
                                |
                          blossom-lfs daemon (stateless)
                          1. base64url-decode --> /path/to/repo
                          2. Config::from_repo_path() -- reads .lfsdalconfig
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
blossom-lfs daemon --log-level debug            # verbose
blossom-lfs daemon --log-json --log-level info  # JSON for observability
blossom-lfs daemon --log-output /tmp/blossom.log
```

## Development

```bash
cargo test                                                # 92 tests (5 ignored)
cargo test --test live_server_tests -- --ignored          # needs BLOSSOM_TEST_SERVER + BLOSSOM_TEST_NSEC
cargo test --test git_lfs_e2e_tests -- --ignored          # needs git-lfs binary
cargo fmt --check
cargo clippy -- -D warnings
```

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
