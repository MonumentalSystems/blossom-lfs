use blossom_lfs::{Agent, Config};
use clap::Parser;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt};
use tracing::error;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
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

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = EnvFilter::try_new(&cli.log_level)
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    if let Some(log_output) = &cli.log_output {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_output)?;

        if cli.log_json {
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
        // Default: stderr so we don't interfere with the LFS protocol on stdout
        if cli.log_json {
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

    if cli.config_info {
        println!("Blossom LFS Configuration:");
        println!("  Set in .lfsdalconfig or .git/config:");
        println!("    lfs-dal.server = <blossom_server_url>");
        println!("    lfs-dal.private-key = <nostr_private_key_or_nsec>");
        println!("    lfs-dal.chunk-size = 16777216 (default: 16MB)");
        println!("    lfs-dal.max-concurrent-uploads = 8 (default)");
        println!("    lfs-dal.max-concurrent-downloads = 8 (default)");
        println!();
        println!("  Or use environment variables:");
        println!("    BLOSSOM_SERVER_URL = <blossom_server_url>");
        println!("    NOSTR_PRIVATE_KEY = <nostr_private_key>");
        return Ok(());
    }

    let config = Config::from_git_config()
        .map_err(|e| anyhow::anyhow!("Failed to load configuration: {}", e))?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let mut agent =
        Agent::new(config, tx).map_err(|e| anyhow::anyhow!("Failed to initialize agent: {}", e))?;

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
