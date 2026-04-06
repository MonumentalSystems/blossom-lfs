use base64::Engine;
use blossom_lfs::Config;
use clap::{Parser, Subcommand};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt};
use tracing::error;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[command(flatten)]
    transfer_args: TransferArgs,
}

#[derive(Parser)]
struct TransferArgs {
    /// Set log file name
    #[arg(long, value_name = "PATH")]
    log_output: Option<std::path::PathBuf>,

    /// Set log level (trace, debug, info, warn, error)
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: String,

    /// Emit logs as JSON (structured OTEL-style output)
    #[arg(long)]
    log_json: bool,

    /// List available configuration
    #[arg(long)]
    config_info: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the lock proxy daemon (local HTTP server for Git LFS lock API).
    Daemon {
        /// Port to listen on (default: from config, or 31921)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Configure lfs.locksurl for the current repository.
    SetupLocks,
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = EnvFilter::try_new(&cli.transfer_args.log_level)
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    if let Some(log_output) = &cli.transfer_args.log_output {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_output)?;

        if cli.transfer_args.log_json {
            fmt()
                .json()
                .with_env_filter(filter)
                .with_writer(file)
                .with_target(true)
                .with_span_events(fmt::format::FmtSpan::CLOSE)
                .init();
        } else {
            fmt()
                .with_env_filter(filter)
                .with_writer(file)
                .with_target(true)
                .with_ansi(false)
                .init();
        }
    } else {
        if cli.transfer_args.log_json {
            fmt()
                .json()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(true)
                .with_span_events(fmt::format::FmtSpan::CLOSE)
                .init();
        } else {
            fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(true)
                .init();
        }
    }

    if cli.transfer_args.config_info {
        println!("Blossom LFS Configuration:");
        println!("  Set in .lfsdalconfig or .git/config:");
        println!("    lfs-dal.server = <blossom_server_url>");
        println!("    lfs-dal.private-key = <nostr_private_key_or_nsec>");
        println!("    lfs-dal.chunk-size = 16777216 (default: 16MB)");
        println!("    lfs-dal.max-concurrent-uploads = 8 (default)");
        println!("    lfs-dal.max-concurrent-downloads = 8 (default)");
        println!("    lfs-dal.transport = http (default; or 'iroh' for QUIC P2P)");
        println!("    lfs-dal.daemon-port = 31921 (default)");
        println!();
        println!("  Or use environment variables:");
        println!("    BLOSSOM_SERVER_URL = <blossom_server_url or iroh_node_id>");
        println!("    NOSTR_PRIVATE_KEY = <nostr_private_key>");
        println!("    BLOSSOM_TRANSPORT = http | iroh");
        println!("    BLOSSOM_DAEMON_PORT = 31921 (default)");
        return Ok(());
    }

    match cli.command {
        Some(Commands::Daemon { port }) => {
            let config = Config::from_git_config()
                .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;
            let daemon_port = port.unwrap_or(config.daemon_port);
            blossom_lfs::daemon::run_daemon(daemon_port).await
        }
        Some(Commands::SetupLocks) => {
            let config = Config::from_git_config()
                .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;
            setup_locks(config.daemon_port)
        }
        None => run_transfer_agent().await,
    }
}

fn setup_locks(daemon_port: u16) -> anyhow::Result<()> {
    let repo_path = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;
    let canonical = repo_path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Failed to canonicalize path: {}", e))?;
    let path_str = canonical.to_string_lossy();

    let repo_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path_str.as_bytes());
    let locks_url = format!("http://localhost:{}/lfs/{}/locks", daemon_port, repo_b64);

    std::process::Command::new("git")
        .args(["config", "lfs.locksurl", &locks_url])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run git config: {}", e))?;

    println!("Configured lfs.locksurl = {}", locks_url);
    println!();
    println!(
        "Make sure 'blossom-lfs daemon' is running on port {}.",
        daemon_port
    );
    Ok(())
}

async fn run_transfer_agent() -> anyhow::Result<()> {
    let config = Config::from_git_config()
        .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let mut agent = blossom_lfs::Agent::new(config, tx)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize agent: {}", e))?;

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut stdout = io::stdout();
            if let Err(e) = stdout.write_all(format!("{}\n", msg).as_bytes()).await {
                eprintln!("Failed to write to stdout: {}", e);
            }
            if let Err(e) = stdout.flush().await {
                eprintln!("Failed to flush stdout: {}", e);
            }
        }
    });

    let stdin = io::stdin();
    let mut lines = io::BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await? {
        if let Err(e) = agent.process(&line).await {
            error!(error.message = %e, "error processing LFS request");
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}
