//! End-to-end tests using the real `git lfs` binary.
//!
//! These tests require `git-lfs` to be installed on the system.
//! Run with: cargo test --test git_lfs_e2e_tests -- --ignored

use base64::Engine;
use blossom_rs::auth::Signer;
use blossom_rs::server::BlobServer;
use blossom_rs::storage::MemoryBackend;
use blossom_rs::{MemoryDatabase, MemoryLfsVersionDatabase};
use sha2::{Digest, Sha256};
use std::fs;
use std::process::Command;

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn repo_b64(repo_path: &std::path::Path) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(repo_path.to_string_lossy().as_bytes())
}

fn run_git(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {:?} failed: {}", args, e));
    if !output.status.success() {
        panic!(
            "git {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    output
}

fn setup_repo(server_url: &str, nsec_hex: &str, daemon_port: u16, repo_path: &std::path::Path) {
    run_git(repo_path, &["init"]);
    run_git(repo_path, &["remote", "add", "origin", "https://myrepo"]);
    run_git(
        repo_path,
        &[
            "config",
            "lfs.url",
            &format!(
                "http://127.0.0.1:{}/lfs/{}",
                daemon_port,
                repo_b64(repo_path)
            ),
        ],
    );
    run_git(repo_path, &["config", "user.email", "test@test.com"]);
    run_git(repo_path, &["config", "user.name", "Test"]);
    run_git(repo_path, &["lfs", "install"]);
    fs::write(
        repo_path.join(".lfsdalconfig"),
        format!("server={}\nprivate-key={}", server_url, nsec_hex),
    )
    .unwrap();
}

async fn spawn_blossom_server() -> String {
    let server = BlobServer::builder(MemoryBackend::new(), "http://localhost:3000")
        .database(MemoryDatabase::new())
        .require_auth()
        .lfs_version_database(MemoryLfsVersionDatabase::new())
        .build();

    let app = server.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    url
}

async fn spawn_lfs_daemon(port: u16) {
    tokio::spawn(blossom_lfs::daemon::run_daemon(port));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

async fn find_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// Push a tracked file from repo A, then pull it in repo B and verify the
/// content is byte-for-byte identical.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_git_lfs_push_pull() {
    let blossom_url = spawn_blossom_server().await;
    let signer = Signer::generate();
    let daemon_port = find_port().await;
    spawn_lfs_daemon(daemon_port).await;

    // --- Repo A: create, commit, push ---
    let repo_a_dir = tempfile::tempdir().unwrap();
    let repo_a_path = repo_a_dir.path();
    setup_repo(
        &blossom_url,
        &signer.secret_key_hex(),
        daemon_port,
        repo_a_path,
    );

    let large_data: Vec<u8> = (0..2000).map(|i| (i % 256) as u8).collect();
    fs::write(repo_a_path.join("large.bin"), &large_data).unwrap();

    run_git(repo_a_path, &["lfs", "track", "*.bin"]);
    run_git(repo_a_path, &["add", ".gitattributes", "large.bin"]);
    run_git(repo_a_path, &["commit", "-m", "add large file"]);
    run_git(repo_a_path, &["lfs", "push", "--all", "origin"]);

    // --- Repo B: init with same remote, pull ---
    let repo_b_dir = tempfile::tempdir().unwrap();
    let repo_b_path = repo_b_dir.path();
    setup_repo(
        &blossom_url,
        &signer.secret_key_hex(),
        daemon_port,
        repo_b_path,
    );

    fs::write(
        repo_b_path.join(".gitattributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();

    let oid = sha256_hex(&large_data);
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        oid,
        large_data.len()
    );
    fs::write(repo_b_path.join("large.bin"), &pointer).unwrap();

    run_git(repo_b_path, &["add", ".gitattributes", "large.bin"]);
    run_git(repo_b_path, &["commit", "-m", "add large file"]);
    run_git(repo_b_path, &["lfs", "pull"]);

    let pulled_content = fs::read(repo_b_path.join("large.bin")).unwrap();
    assert_eq!(
        &pulled_content[..],
        &large_data[..],
        "pulled content should match original"
    );
}
