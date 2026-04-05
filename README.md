# BlossomLFS

Git LFS custom transfer agent for Blossom blob storage with chunking support for large files (2GB+).

## Features

- **Large file support** (2GB+) via automatic chunking
- **Merkle tree integrity verification** for all chunks
- **Nostr authentication** (kind 24242 events)
- **Parallel chunk uploads/downloads** for performance
- **Resumable downloads** (track which chunks completed)
- **16MB chunks** (configurable)

## Installation

```bash
cargo install blossom-lfs
```

Or build from source:

```bash
git clone https://github.com/MonumentalSystems/BlossomLFS
cd BlossomLFS
cargo build --release
```

## Configuration

### Git LFS Setup

Configure Git LFS to use BlossomLFS as a custom transfer agent:

```bash
git lfs install --local
git config lfs.standalonetransferagent blossom-lfs
git config lfs.customtransfer.blossom-lfs.path /path/to/blossom-lfs
git config -f .lfsconfig lfs.url blossom-lfs
```

### Blossom Configuration

Create `.lfsdalconfig` in your repository root:

```ini
[lfs-dal]
    server = https://your-blossom-server.com
    private-key = nsec1... # Your Nostr private key
    chunk-size = 16777216 # 16MB (optional, default)
    max-concurrent-uploads = 8 # (optional, default)
    max-concurrent-downloads = 8 # (optional, default)
    auth-expiration = 3600 # 1 hour (optional, default)
```

Or use environment variables:

```bash
export BLOSSOM_SERVER_URL="https://your-blossom-server.com"
export NOSTR_PRIVATE_KEY="nsec1..."
```

**⚠️ Security Warning**: Never commit your private key to version control. Use environment variables or `.git/config` (not tracked).

## How It Works

### Upload Flow

```
Large File (2GB+)
    ↓
Split into 16MB chunks
    ↓
Compute SHA256 for each chunk
    ↓
Upload chunks to Blossom server (parallel)
    ↓
Build Merkle tree from chunk hashes
    ↓
Create JSON manifest with merkle root
    ↓
Upload manifest to Blossom
    ↓
Return merkle root as Git LFS OID
```

### Download Flow

```
Git LFS OID (merkle root)
    ↓
Download manifest from Blossom
    ↓
Verify merkle tree integrity
    ↓
Download chunks (parallel)
    ↓
Verify each chunk hash
    ↓
Reassemble file
    ↓
Return to Git LFS
```

### Manifest Format

```json
{
  "version": "1.0",
  "file_size": 2147483648,
  "chunk_size": 16777216,
  "chunks": 128,
  "merkle_root": "abc123...",
  "chunk_hashes": ["hash1", "hash2", ...],
  "original_filename": "large_file.bin",
  "content_type": "application/octet-stream",
  "created_at": 1234567890,
  "blossom_server": "https://cdn.example.com"
}
```

## Architecture

- **Chunker**: Splits files into16MB chunks with SHA256 hashing
- **Merkle Tree**: Binary merkle tree for integrity verification
- **Manifest**: JSON metadata stored as Blossom blob
- **Blossom Client**: HTTP client with Nostr signing (kind 24242 auth events)
- **Git LFS Agent**: Implements custom transfer agent protocol

## Dependencies

- `tokio` - Async runtime
- `reqwest` - HTTP client
- `nostr-sdk` - Nostr signing
- `sha2` - SHA256 hashing
- `serde` - JSON serialization
- `clap` - CLI argument parsing

## Development

### Run Tests

```bash
cargo test
```

### Build Documentation

```bash
cargo doc --open
```

## License

MIT

## Credits

Based on [lfs-dal](https://github.com/regen100/lfs-dal) by regen100.

Integrates with the [Blossom](https://github.com/hzrd149/blossom) protocol.

## References

- [Git LFS Custom Transfers](https://github.com/git-lfs/git-lfs/blob/main/docs/custom-transfers.md)
- [Blossom Protocol](https://github.com/hzrd149/blossom)
- [BUD-01: Server Requirements](https://github.com/hzrd149/blossom/blob/master/buds/01.md)
- [BUD-02: Blob Upload](https://github.com/hzrd149/blossom/blob/master/buds/02.md)
- [BUD-11: Nostr Authorization](https://github.com/hzrd149/blossom/blob/master/buds/11.md)